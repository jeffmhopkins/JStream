//! Joining a sender's state messages from their pieces (ADR-0171 decision 1).

use crate::wire::{MAX_STATE_PIECES, StatePiece};
use std::collections::HashMap;

/// The most messages one sender has part-way assembled; a piece of one more forgets the
/// one that waited longest.
pub const MAX_PARTIAL_STATE: usize = 128;
/// The most keys whose last whole message is remembered, so a resend of it is known for
/// one: a sender holds no more than this many keys (ADR-0171 decision 1).
pub const MAX_STATE_KEYS: usize = 1024;

/// What one piece did, from [`StateAssembler::push`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateStep {
    /// It finished a message: its bytes, all of its pieces in order.
    Whole(Vec<u8>),
    /// It belongs to the message the key last finished: the sender's resend of
    /// something already here, which says the key is still current.
    Repeat,
    /// It is part of a message not finished yet.
    Partial,
}

/// A piece's outcome, and whether its stream ID started the sender's state again first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatePush {
    /// A new stream ID: the sender started again, so everything it sent before is gone
    /// (here, and for the caller to forget too). The first piece of all says so too.
    pub restarted: bool,
    /// What the piece did.
    pub step: StateStep,
}

struct Partial {
    message_id: u32,
    pieces: u8,
    have: u8,
    touched: u64,
    parts: Vec<Option<Vec<u8>>>,
}

/// Puts one sender's state messages back together from their pieces, which may come in
/// any order, repeated or not at all, as JTunnel's own receiver does: pieces join per key,
/// all or nothing, and a piece of a different message for the key starts that one over.
/// On the receive thread only; it allocates.
#[derive(Default)]
pub struct StateAssembler {
    stream_id: Option<u32>,
    partial: HashMap<String, Partial>,
    /// The message each key last finished.
    done: HashMap<String, u32>,
    touches: u64,
    evicted: u64,
}

impl std::fmt::Debug for StateAssembler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateAssembler")
            .field("stream_id", &self.stream_id)
            .field("partial", &self.partial.len())
            .field("done", &self.done.len())
            .finish_non_exhaustive()
    }
}

impl StateAssembler {
    /// An assembler that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one piece (as [`parse`](crate::wire::parse) checked it).
    pub fn push(&mut self, piece: &StatePiece<'_>) -> StatePush {
        let restarted = self.stream_id != Some(piece.stream_id);
        if restarted {
            self.reset();
            self.stream_id = Some(piece.stream_id);
        }
        let step = self.step(piece);
        StatePush { restarted, step }
    }

    fn step(&mut self, piece: &StatePiece<'_>) -> StateStep {
        if self.done.get(piece.key) == Some(&piece.message_id) {
            return StateStep::Repeat;
        }
        if piece.pieces == 1 {
            self.partial.remove(piece.key);
            self.finish(piece.key, piece.message_id);
            return StateStep::Whole(piece.data.to_vec());
        }
        let fresh = self
            .partial
            .get(piece.key)
            .is_none_or(|p| p.message_id != piece.message_id || p.pieces != piece.pieces);
        if fresh {
            if !self.partial.contains_key(piece.key) && self.partial.len() >= MAX_PARTIAL_STATE {
                // Forget the one that has waited longest.
                if let Some(oldest) = self
                    .partial
                    .iter()
                    .min_by_key(|(_, p)| p.touched)
                    .map(|(k, _)| k.clone())
                {
                    self.partial.remove(&oldest);
                    self.evicted += 1;
                }
            }
            self.partial.insert(
                piece.key.to_owned(),
                Partial {
                    message_id: piece.message_id,
                    pieces: piece.pieces,
                    have: 0,
                    touched: 0,
                    parts: vec![None; usize::from(piece.pieces.min(MAX_STATE_PIECES))],
                },
            );
        }
        self.touches += 1;
        let Some(p) = self.partial.get_mut(piece.key) else {
            return StateStep::Partial;
        };
        p.touched = self.touches;
        if let Some(slot) = p.parts.get_mut(usize::from(piece.piece))
            && slot.is_none()
        {
            *slot = Some(piece.data.to_vec());
            p.have += 1;
        }
        if p.have < p.pieces {
            return StateStep::Partial;
        }
        let whole = self
            .partial
            .remove(piece.key)
            .map(|p| p.parts.into_iter().flatten().flatten().collect())
            .unwrap_or_default();
        self.finish(piece.key, piece.message_id);
        StateStep::Whole(whole)
    }

    fn finish(&mut self, key: &str, message_id: u32) {
        if let Some(id) = self.done.get_mut(key) {
            *id = message_id;
        } else if self.done.len() < MAX_STATE_KEYS {
            self.done.insert(key.to_owned(), message_id);
        }
    }

    /// Forgets that `key` finished a message, so the next piece of it, a resend
    /// included, is assembled again rather than taken as a [`StateStep::Repeat`]: for
    /// a caller that lost or refused the message and wants it back.
    pub fn forget(&mut self, key: &str) {
        self.done.remove(key);
    }

    /// Forgets everything: the stream, the messages part-way and the ones finished.
    pub fn reset(&mut self) {
        self.stream_id = None;
        self.partial.clear();
        self.done.clear();
    }

    /// How many messages part-way are waiting for pieces.
    #[must_use]
    pub fn partial(&self) -> usize {
        self.partial.len()
    }

    /// How many messages part-way were forgotten because [`MAX_PARTIAL_STATE`] others
    /// were waiting.
    #[must_use]
    pub const fn evicted(&self) -> u64 {
        self.evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece<'a>(id: u32, i: u8, n: u8, key: &'a str, data: &'a [u8]) -> StatePiece<'a> {
        StatePiece {
            stream_id: 9,
            message_id: id,
            piece: i,
            pieces: n,
            key,
            data,
        }
    }

    #[test]
    fn a_message_in_one_piece_is_whole_and_its_resend_a_repeat() {
        let mut a = StateAssembler::new();
        let first = a.push(&piece(1, 0, 1, "clock", b"{}"));
        assert_eq!(
            first,
            StatePush {
                restarted: true,
                step: StateStep::Whole(b"{}".to_vec())
            }
        );
        let again = a.push(&piece(1, 0, 1, "clock", b"{}"));
        assert_eq!(again.step, StateStep::Repeat);
        assert!(!again.restarted);
        assert_eq!(
            a.push(&piece(2, 0, 1, "clock", b"[]")).step,
            StateStep::Whole(b"[]".to_vec())
        );
    }

    #[test]
    fn pieces_join_in_any_order_repeated_and_a_resend_finishes_a_lost_one() {
        let mut a = StateAssembler::new();
        assert_eq!(
            a.push(&piece(5, 2, 3, "track/1", b"ef")).step,
            StateStep::Partial
        );
        assert_eq!(
            a.push(&piece(5, 0, 3, "track/1", b"ab")).step,
            StateStep::Partial
        );
        assert_eq!(
            a.push(&piece(5, 0, 3, "track/1", b"XX")).step,
            StateStep::Partial
        );
        // Piece 1 was lost; the refresh a second later repeats the message.
        assert_eq!(
            a.push(&piece(5, 2, 3, "track/1", b"ef")).step,
            StateStep::Partial
        );
        assert_eq!(
            a.push(&piece(5, 1, 3, "track/1", b"cd")).step,
            StateStep::Whole(b"abcdef".to_vec())
        );
        assert_eq!(
            a.push(&piece(5, 0, 3, "track/1", b"ab")).step,
            StateStep::Repeat
        );
        assert_eq!(a.partial(), 0);
    }

    #[test]
    fn a_newer_message_starts_the_key_over_and_a_new_stream_forgets_everything() {
        let mut a = StateAssembler::new();
        a.push(&piece(1, 0, 2, "mixer", b"a"));
        a.push(&piece(2, 0, 2, "mixer", b"x"));
        assert_eq!(
            a.push(&piece(1, 1, 2, "mixer", b"b")).step,
            StateStep::Partial
        );
        a.push(&piece(2, 0, 2, "mixer", b"x"));
        assert_eq!(
            a.push(&piece(2, 1, 2, "mixer", b"y")).step,
            StateStep::Whole(b"xy".to_vec())
        );
        let mut other = piece(2, 0, 1, "mixer", b"z");
        other.stream_id = 10;
        let pushed = a.push(&other);
        assert!(pushed.restarted);
        assert_eq!(
            pushed.step,
            StateStep::Whole(b"z".to_vec()),
            "not a repeat any more"
        );
    }

    #[test]
    fn a_forgotten_key_takes_its_resend_again() {
        let mut a = StateAssembler::new();
        a.push(&piece(1, 0, 1, "clock", b"{}"));
        assert_eq!(
            a.push(&piece(1, 0, 1, "clock", b"{}")).step,
            StateStep::Repeat
        );
        a.forget("clock");
        assert_eq!(
            a.push(&piece(1, 0, 1, "clock", b"{}")).step,
            StateStep::Whole(b"{}".to_vec())
        );
    }

    #[test]
    fn a_new_piece_count_under_the_same_message_starts_over() {
        let mut a = StateAssembler::new();
        assert!(
            a.push(&piece(1, 0, 2, "k", b"a")).restarted,
            "the first piece starts the stream"
        );
        assert_eq!(a.push(&piece(1, 1, 3, "k", b"b")).step, StateStep::Partial);
        // Piece 0 of the two-piece form is gone: the three-piece one needs all three.
        assert_eq!(a.push(&piece(1, 0, 3, "k", b"x")).step, StateStep::Partial);
        assert_eq!(
            a.push(&piece(1, 2, 3, "k", b"c")).step,
            StateStep::Whole(b"xbc".to_vec())
        );
    }

    #[test]
    fn at_most_so_many_messages_wait_part_way() {
        let mut a = StateAssembler::new();
        let keys: Vec<String> = (0..=MAX_PARTIAL_STATE).map(|n| format!("k{n}")).collect();
        for key in &keys {
            a.push(&piece(1, 0, 2, key, b"a"));
        }
        assert_eq!(a.partial(), MAX_PARTIAL_STATE);
        assert_eq!(a.evicted(), 1);
        // The first waited longest and went; the last finishes.
        assert_eq!(a.push(&piece(1, 1, 2, "k0", b"b")).step, StateStep::Partial);
        assert_eq!(
            a.push(&piece(1, 1, 2, &keys[MAX_PARTIAL_STATE], b"b")).step,
            StateStep::Whole(b"ab".to_vec())
        );
    }
}
