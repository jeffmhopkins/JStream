//! Holding a stream's frames until they're due, in order.
//!
//! Frames arrive early, late, twice (redundancy) or never. [`Playout`] keeps the next 64
//! by sequence number, and gives them back in order once each is due: the time its last
//! sample arrives with no jitter (from [`SenderClock`]) plus the stream's lead, the
//! buffer that covers jitter. A frame not here when it is due is given back concealed
//! (silent) and counted lost, and one that arrives after its turn is dropped and counted
//! late. What comes out is what the mixer's input takes from a device: samples, the
//! stream position of the first sample, and a media-clock time for it, smooth because
//! it comes from the clock estimate and not from the arrivals.

use crate::clock::SenderClock;
use crate::wire::{
    self, AudioHeader, CHANNELS, Event, Frame, MAX_EVENT_BYTES, MAX_EVENTS_PER_FRAME,
    MAX_FRAME_SAMPLES,
};

/// Frames the playout holds ahead of the next one due.
pub const SLOTS: usize = 64;
const MAX_SAMPLES: usize = MAX_FRAME_SAMPLES as usize * CHANNELS as usize;

#[derive(Clone, Copy, Debug)]
struct StoredEvent {
    offset: u16,
    len: u8,
    bytes: [u8; MAX_EVENT_BYTES],
}

#[derive(Clone)]
struct Slot {
    /// The frame's index in the stream, from 0 at its first frame; `None` if empty.
    index: Option<u64>,
    samples: [f32; MAX_SAMPLES],
    events: [StoredEvent; MAX_EVENTS_PER_FRAME],
    event_count: usize,
}

impl Slot {
    fn empty() -> Self {
        Self {
            index: None,
            samples: [0.0; MAX_SAMPLES],
            events: [StoredEvent {
                offset: 0,
                len: 0,
                bytes: [0; MAX_EVENT_BYTES],
            }; MAX_EVENTS_PER_FRAME],
            event_count: 0,
        }
    }
}

/// What happened to a frame given to [`Playout::push`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// Kept until it's due.
    Stored,
    /// The first frame of a new stream (the sender started or restarted): everything
    /// held was dropped, and the next frame out is marked as a discontinuity.
    Restarted,
    /// Already held, or already played: a redundant copy.
    Repeat,
    /// Its turn has passed: dropped.
    Late,
}

/// The counters a playout keeps, for `profile.get` and the source's stats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayoutStats {
    /// Frames given back with their audio.
    pub played: u64,
    /// Frames given back concealed because they never came in time.
    pub concealed: u64,
    /// Frames that came after their turn.
    pub late: u64,
    /// Redundant copies and repeated datagrams.
    pub repeats: u64,
    /// Times the stream restarted.
    pub restarts: u64,
}

/// A frame given back by [`Playout::pop`].
#[derive(Clone, Copy, Debug)]
pub struct Due<'a> {
    /// The stream position of the frame's first sample, in the sender's samples.
    pub position: u64,
    /// The media-clock time, in nanoseconds, the first sample plays at.
    pub time_ns: u64,
    /// Its samples, interleaved stereo; silence when `concealed`.
    pub samples: &'a [f32],
    /// The frame never came; `samples` is silence.
    pub concealed: bool,
    /// The first frame after a restart: not continuous with what came before.
    pub discontinuity: bool,
    events: &'a [StoredEvent],
}

impl<'a> Due<'a> {
    /// The frame's events, each with its offset in samples from the frame's start.
    pub fn events(&self) -> impl Iterator<Item = (u16, Event<'a>)> + 'a {
        self.events
            .iter()
            .filter_map(|e| Some((e.offset, wire::decode(&e.bytes[..usize::from(e.len)])?)))
    }
}

/// The most a frame's start moves toward the clock estimate, as a share of a frame:
/// 0.5%, which no listener hears and which closes a millisecond in a fifth of a second.
pub const SLEW: f64 = 0.005;
/// An error between the frames' times and the clock estimate past which the times jump,
/// marking a discontinuity, instead of slewing: 50 ms.
pub const SNAP_NS: f64 = 50e6;

/// One stream's frames, held until they're due.
///
/// Every buffer is allocated when it is made, so pushing and popping never allocate.
pub struct Playout {
    lead_ns: u64,
    stream: Option<u32>,
    first_seq: u32,
    frame_samples: u64,
    sample_rate: u32,
    clock: SenderClock,
    slots: Box<[Slot]>,
    /// The index of the next frame due, once the stream has a first frame.
    next: Option<u64>,
    discontinuity: bool,
    /// When the next frame starts on the media clock, ns: the times handed out run
    /// on from each other, and slew toward the clock estimate rather than jump to it.
    timeline: Option<f64>,
    silence: Box<[f32]>,
    stats: PlayoutStats,
}

impl std::fmt::Debug for Playout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Playout")
            .field("lead_ns", &self.lead_ns)
            .field("stream", &self.stream)
            .field("next", &self.next)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Playout {
    /// A playout holding frames `lead_ns` past the time they'd arrive with no jitter.
    #[must_use]
    pub fn new(lead_ns: u64) -> Self {
        Self {
            lead_ns,
            stream: None,
            first_seq: 0,
            frame_samples: 1,
            sample_rate: 48_000,
            clock: SenderClock::new(48_000),
            slots: vec![Slot::empty(); SLOTS].into_boxed_slice(),
            next: None,
            discontinuity: false,
            timeline: None,
            silence: vec![0.0; MAX_SAMPLES].into_boxed_slice(),
            stats: PlayoutStats::default(),
        }
    }

    /// Changes the lead, as an automatic buffer does after a dropout.
    pub fn set_lead_ns(&mut self, lead_ns: u64) {
        self.lead_ns = lead_ns;
    }

    /// The lead.
    #[must_use]
    pub const fn lead_ns(&self) -> u64 {
        self.lead_ns
    }

    /// The counters so far.
    #[must_use]
    pub const fn stats(&self) -> PlayoutStats {
        self.stats
    }

    /// The clock estimate, for its drift.
    #[must_use]
    pub const fn clock(&self) -> &SenderClock {
        &self.clock
    }

    /// Takes one frame of a parsed audio packet, with its audio decoded to interleaved
    /// stereo `samples` (empty for a MIDI-only stream, held as silence), arrived at
    /// `arrival_ns`. Give a packet's frames oldest first.
    pub fn push(
        &mut self,
        header: &AudioHeader,
        frame: &Frame<'_>,
        samples: &[f32],
        arrival_ns: u64,
    ) -> Pushed {
        let restarted = self.stream != Some(header.stream_id)
            || u64::from(header.frame_samples) != self.frame_samples
            || header.sample_rate != self.sample_rate;
        if restarted {
            self.restart(header, frame.seq);
        }
        let index = match u64::try_from(i64::from(frame.seq.wrapping_sub(self.first_seq) as i32)) {
            Ok(i) => i,
            // Before the stream's first frame: older than anything played.
            Err(_) => {
                self.stats.late += 1;
                return Pushed::Late;
            }
        };
        let end = (index + 1) * self.frame_samples;
        self.clock.observe(end, arrival_ns);
        let next = *self.next.get_or_insert(index);
        if index < next {
            // Already played, or its turn passed while it was missing.
            self.stats.late += 1;
            return Pushed::Late;
        }
        if index >= next + SLOTS as u64 {
            // Far ahead of what's due: the stream outran the playout. Skip what can't
            // be held, concealing it.
            let skip = index + 1 - SLOTS as u64;
            for i in next..skip {
                let slot = &mut self.slots[(i % SLOTS as u64) as usize];
                if slot.index == Some(i) {
                    slot.index = None;
                }
                self.stats.concealed += 1;
            }
            self.next = Some(skip);
        }
        let slot = &mut self.slots[(index % SLOTS as u64) as usize];
        if slot.index == Some(index) {
            self.stats.repeats += 1;
            return Pushed::Repeat;
        }
        slot.index = Some(index);
        let n = samples.len().min(MAX_SAMPLES);
        slot.samples[..n].copy_from_slice(&samples[..n]);
        slot.samples[n..].fill(0.0);
        slot.event_count = 0;
        for (offset, raw) in frame.raw_events() {
            let e = &mut slot.events[slot.event_count];
            e.offset = offset;
            e.len = raw.len() as u8;
            e.bytes[..raw.len()].copy_from_slice(raw);
            slot.event_count += 1;
        }
        if restarted {
            Pushed::Restarted
        } else {
            Pushed::Stored
        }
    }

    fn restart(&mut self, header: &AudioHeader, seq: u32) {
        if self.stream.is_some() {
            self.stats.restarts += 1;
        }
        self.stream = Some(header.stream_id);
        self.first_seq = seq;
        self.frame_samples = u64::from(header.frame_samples.max(1));
        self.sample_rate = header.sample_rate;
        self.clock.reset(header.sample_rate);
        for s in self.slots.iter_mut() {
            s.index = None;
        }
        self.next = None;
        self.timeline = None;
        self.discontinuity = true;
    }

    /// One frame's length on the media clock, ns, with the drift once it's known.
    fn frame_ns(&self) -> f64 {
        let drift = self.clock.drift_ppm().unwrap_or(0.0) / 1e6;
        self.frame_samples as f64 / f64::from(self.sample_rate.max(1)) * 1e9 * (1.0 + drift)
    }

    /// When the next frame starts on the media clock, ns, and whether that is a jump.
    /// The estimate moves (as the lowest arrival improves, and once the drift is known),
    /// so the start runs on from the last frame's end and moves toward the estimate by
    /// at most [`SLEW`] of a frame at a time. Only an error past [`SNAP_NS`] jumps.
    fn next_start(&self) -> Option<(f64, bool)> {
        let next = self.next?;
        let target = self.clock.arrival_of(next * self.frame_samples)? as f64 + self.lead_ns as f64;
        Some(match self.timeline {
            Some(t) if (target - t).abs() <= SNAP_NS => {
                let step = self.frame_ns() * SLEW;
                (t + (target - t).clamp(-step, step), false)
            }
            Some(_) => (target, true),
            None => (target, false),
        })
    }

    /// When the next frame is due, in media-clock nanoseconds: when its last sample
    /// arrives with no jitter, plus the lead.
    #[must_use]
    pub fn next_due_ns(&self) -> Option<u64> {
        let (start, _) = self.next_start()?;
        let due = start + self.frame_ns();
        (due.is_finite() && due >= 0.0).then(|| due.round() as u64)
    }

    /// Gives back the next frame if it is due at `now_ns`: with its audio, or concealed
    /// if it never came. Call it until it gives `None`.
    pub fn pop(&mut self, now_ns: u64) -> Option<Due<'_>> {
        let due = self.next_due_ns()?;
        if due > now_ns {
            return None;
        }
        let (start, jumped) = self.next_start()?;
        let index = self.next?;
        self.next = Some(index + 1);
        self.timeline = Some(start + self.frame_ns());
        let position = index * self.frame_samples;
        let time_ns = start.max(0.0).round() as u64;
        if jumped {
            self.discontinuity = true;
        }
        let discontinuity = std::mem::take(&mut self.discontinuity);
        let len = (self.frame_samples as usize * usize::from(CHANNELS)).min(MAX_SAMPLES);
        let slot = &mut self.slots[(index % SLOTS as u64) as usize];
        if slot.index == Some(index) {
            slot.index = None;
            self.stats.played += 1;
            let slot = &self.slots[(index % SLOTS as u64) as usize];
            Some(Due {
                position,
                time_ns,
                samples: &slot.samples[..len],
                concealed: false,
                discontinuity,
                events: &slot.events[..slot.event_count],
            })
        } else {
            self.stats.concealed += 1;
            Some(Due {
                position,
                time_ns,
                samples: &self.silence[..len],
                concealed: true,
                discontinuity,
                events: &[],
            })
        }
    }

    /// How far ahead of `now_ns` the newest held audio reaches, in nanoseconds: what
    /// JTunnel shows as LATENCY. Zero when nothing is held.
    #[must_use]
    pub fn held_ns(&self, now_ns: u64) -> u64 {
        let newest = self.slots.iter().filter_map(|s| s.index).max();
        newest
            .and_then(|i| self.clock.arrival_of((i + 1) * self.frame_samples))
            .map_or(0, |t| (t + self.lead_ns).saturating_sub(now_ns))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Codec, FrameOut, Packet, write_audio};

    fn header(seq: u32, stream_id: u32) -> AudioHeader {
        AudioHeader {
            stream_id,
            seq,
            sample_rate: 48_000,
            codec: Codec::None,
            frame_samples: 48,
        }
    }

    /// Pushes frame `seq` (MIDI only, one CC carrying `seq` as its value) at `t`.
    fn push(p: &mut Playout, seq: u32, stream: u32, t: u64) -> Pushed {
        let value = [0xB0, 1, (seq % 128) as u8];
        let events: [(u16, &[u8]); 1] = [(3, &value)];
        let mut buf = [0; 64];
        let h = header(seq, stream);
        let n = write_audio(
            &mut buf,
            &h,
            &[FrameOut {
                events: &events,
                audio: &[],
            }],
        )
        .unwrap_or(0);
        let Ok(Packet::Audio {
            header,
            frames: [Some(f), _],
        }) = wire::parse(&buf[..n])
        else {
            panic!("bad packet");
        };
        p.push(&header, &f, &[], t)
    }

    const MS: u64 = 1_000_000;

    #[test]
    fn frames_come_out_in_order_when_due_and_gaps_are_concealed() {
        let mut p = Playout::new(5 * MS);
        // 1 ms frames: frame k ends at (k + 1) ms; the path takes 2 ms.
        let arrive = |k: u64| (k + 1) * MS + 2 * MS;
        assert_eq!(push(&mut p, 100, 1, arrive(0)), Pushed::Restarted);
        assert_eq!(push(&mut p, 102, 1, arrive(2) + MS), Pushed::Stored);
        assert_eq!(push(&mut p, 101, 1, arrive(1) + 3 * MS), Pushed::Stored);
        assert_eq!(push(&mut p, 101, 1, arrive(1) + 3 * MS), Pushed::Repeat);
        // Frame 0 is due at 1 ms + 2 ms + 5 ms lead.
        assert!(p.pop(7 * MS).is_none());
        let first = p
            .pop(8 * MS)
            .map(|d| (d.position, d.concealed, d.discontinuity));
        assert_eq!(first, Some((0, false, true)));
        let d = p
            .pop(10 * MS)
            .map(|d| (d.position, d.events().collect::<Vec<_>>()));
        assert!(matches!(
            d,
            Some((48, ref e)) if matches!(e[..], [(3, Event::Midi(jstream_midi::Message::ControlChange { value: 101, .. }))])
        ));
        assert_eq!(p.pop(10 * MS).map(|d| d.position), Some(96));
        // Frame 3 never comes: at its time it's concealed.
        let gap = p.pop(11 * MS).map(|d| (d.position, d.concealed));
        assert_eq!(gap, Some((144, true)));
        // And arriving after that, it's late.
        assert_eq!(push(&mut p, 103, 1, 12 * MS), Pushed::Late);
        let s = p.stats();
        assert_eq!((s.played, s.concealed, s.late, s.repeats), (3, 1, 1, 1));
    }

    #[test]
    fn a_restart_drops_what_was_held_and_marks_a_discontinuity() {
        let mut p = Playout::new(0);
        push(&mut p, 5, 1, MS);
        push(&mut p, 6, 1, 2 * MS);
        assert_eq!(push(&mut p, 900, 2, 3 * MS), Pushed::Restarted);
        assert_eq!(p.stats().restarts, 1);
        let d = p.pop(u64::MAX / 2).map(|d| (d.position, d.discontinuity));
        assert_eq!(d, Some((0, true)));
        assert!(p.held_ns(0) == 0);
    }

    #[test]
    fn a_stream_far_ahead_skips_what_cannot_be_held() {
        let mut p = Playout::new(0);
        push(&mut p, 0, 1, MS);
        assert_eq!(push(&mut p, 200, 1, 202 * MS), Pushed::Stored);
        // Frames 0..=136 can't be held beside 200 in 64 slots.
        assert_eq!(p.stats().concealed, 137);
        assert_eq!(p.pop(u64::MAX / 2).map(|d| d.position), Some(137 * 48));
    }
}
