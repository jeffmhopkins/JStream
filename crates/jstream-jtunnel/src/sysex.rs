//! Joining a SysEx message's pieces.

use crate::wire::{MAX_SYSEX_BYTES, SysexPiece};

/// Joins the [`SysexPiece`]s of one stream back into whole SysEx messages, as JTunnel's
/// receiver does: a message missing a piece, or with one out of order, is dropped whole
/// rather than delivered broken.
///
/// Its buffer is allocated once, when it is made, so pushing pieces never allocates.
#[derive(Clone, Debug)]
pub struct SysexAssembler {
    buf: Box<[u8]>,
    len: usize,
    /// The index the next piece must have, or `None` outside a message.
    next: Option<u16>,
    dropped: u64,
}

impl Default for SysexAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl SysexAssembler {
    /// An assembler outside any message.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: vec![0; MAX_SYSEX_BYTES].into_boxed_slice(),
            len: 0,
            next: None,
            dropped: 0,
        }
    }

    /// Takes the next piece. Gives the whole message when this piece is its last and
    /// every piece arrived in order.
    pub fn push(&mut self, piece: &SysexPiece<'_>) -> Option<&[u8]> {
        if piece.first {
            if self.next.is_some() {
                self.dropped += 1;
            }
            self.len = 0;
            self.next = Some(0);
        }
        let Some(next) = self.next else {
            // A piece of a message whose start was lost, or that was already dropped.
            return None;
        };
        if next != u16::from(piece.index) || self.len + piece.data.len() > self.buf.len() {
            self.abandon();
            return None;
        }
        self.buf[self.len..self.len + piece.data.len()].copy_from_slice(piece.data);
        self.len += piece.data.len();
        if !piece.last {
            self.next = Some(next + 1);
            return None;
        }
        self.next = None;
        Some(&self.buf[..self.len])
    }

    /// Drops a message in progress: a frame of the stream was lost, so one of its pieces
    /// may be missing.
    pub fn abandon(&mut self) {
        if self.next.take().is_some() {
            self.dropped += 1;
        }
        self.len = 0;
    }

    /// How many messages were dropped because a piece was missing.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(first: bool, last: bool, index: u8, data: &[u8]) -> SysexPiece<'_> {
        SysexPiece {
            first,
            last,
            index,
            data,
        }
    }

    #[test]
    fn joins_pieces_in_order() {
        let mut a = SysexAssembler::new();
        assert_eq!(
            a.push(&piece(true, true, 0, &[0xF0, 1, 0xF7])),
            Some(&[0xF0, 1, 0xF7][..])
        );
        assert_eq!(a.push(&piece(true, false, 0, &[0xF0, 1])), None);
        assert_eq!(a.push(&piece(false, false, 1, &[2, 3])), None);
        assert_eq!(
            a.push(&piece(false, true, 2, &[0xF7])),
            Some(&[0xF0, 1, 2, 3, 0xF7][..])
        );
        assert_eq!(a.dropped(), 0);
    }

    #[test]
    fn a_missing_piece_drops_the_whole_message() {
        let mut a = SysexAssembler::new();
        assert_eq!(a.push(&piece(true, false, 0, &[0xF0])), None);
        assert_eq!(a.push(&piece(false, true, 2, &[0xF7])), None);
        assert_eq!(a.dropped(), 1);
        // Pieces of a message whose start never came are ignored.
        assert_eq!(a.push(&piece(false, true, 1, &[0xF7])), None);
        assert_eq!(a.dropped(), 1);
        // A lost frame abandons a message in progress.
        assert_eq!(a.push(&piece(true, false, 0, &[0xF0])), None);
        a.abandon();
        assert_eq!(a.push(&piece(false, true, 1, &[0xF7])), None);
        assert_eq!(a.dropped(), 2);
        // A new first piece restarts, dropping the unfinished one.
        assert_eq!(a.push(&piece(true, false, 0, &[0xF0])), None);
        assert_eq!(
            a.push(&piece(true, true, 0, &[0xF0, 0xF7])),
            Some(&[0xF0, 0xF7][..])
        );
        assert_eq!(a.dropped(), 3);
    }

    #[test]
    fn carries_the_longest_message() {
        let mut a = SysexAssembler::new();
        let chunk = [5_u8; crate::wire::SYSEX_PIECE_BYTES];
        let mut out = None;
        for i in 0..=255_u8 {
            out = a.push(&piece(i == 0, i == 255, i, &chunk)).map(<[u8]>::len);
        }
        assert_eq!(out, Some(MAX_SYSEX_BYTES));
    }
}
