//! Direct vendor encoders on the GPU path (ADR-0217): NVIDIA's NVENC and AMD's AMF,
//! reached through the vendor's own runtime DLL, which its display driver installs, and
//! tried before the Media Foundation hardware MFT for the same GPU.
//!
//! Both read the context's encode targets the way the MFT path does: each target is
//! copied on the GPU into a texture of the encoder's own ([`Ring`], `EncodeGpu::copy`),
//! so the target goes back to its producer as soon as the copy is queued, and the
//! encoder reads that texture in place. Nothing touches the CPU. They are configured
//! like the MFT path (CBR or peak-constrained VBR, the keyframe interval as the GOP, no
//! B-frames so `dts == pts`, BT.709 limited range, a quality preset tuned for live
//! streaming) and frame their packets the same way (Annex B with the parameter sets on
//! every IDR frame, AV1 as one temporal unit of low-overhead OBUs).
//!
//! This module holds what both share and what is tested on every platform: which
//! vendor an adapter is ([`Vendor`]), which direct encoders to try ([`Policy`], from the
//! `JSTREAM_DIRECT_ENCODERS` variable), and the bookkeeping of the input textures and
//! the frames in flight ([`Slots`]). The vendor sessions are `nvenc` and `amf`, Windows
//! only, with their hand-written FFI declarations in `nvenc::ffi` and `amf::ffi`, whose
//! layouts are checked at compile time on every platform.

use std::collections::VecDeque;

use jstream_platform::media::MediaTime;

#[cfg(any(windows, test))]
pub(crate) mod amf;
#[cfg(any(windows, test))]
pub(crate) mod nvenc;
#[cfg(windows)]
mod ring;

#[cfg(windows)]
pub(crate) use open::{open_direct, policy};
#[cfg(windows)]
pub(crate) use ring::Ring;

/// The variable that chooses which direct encoders the GPU path tries ([`Policy`]).
#[cfg(windows)]
pub(crate) const VARIABLE: &str = "JSTREAM_DIRECT_ENCODERS";

/// A GPU maker, from its adapter's PCI vendor ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vendor {
    /// NVIDIA (`0x10DE`): NVENC.
    Nvidia,
    /// AMD (`0x1002`): AMF.
    Amd,
    /// Any other (Intel's encoders are reached through Media Foundation only, ADR-0140).
    Other,
}

impl Vendor {
    /// The vendor of a PCI vendor ID (`DXGI_ADAPTER_DESC::VendorId`).
    pub(crate) fn of_pci(id: u32) -> Self {
        match id {
            0x10DE => Self::Nvidia,
            0x1002 | 0x1022 => Self::Amd,
            _ => Self::Other,
        }
    }

    /// The direct encoder's name, for messages and logs.
    pub(crate) fn encoder(self) -> Option<&'static str> {
        match self {
            Self::Nvidia => Some("NVENC"),
            Self::Amd => Some("AMF"),
            Self::Other => None,
        }
    }
}

/// Which direct encoders the GPU path tries before Media Foundation, from
/// `JSTREAM_DIRECT_ENCODERS`'s value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Policy {
    /// None: every GPU encoder is a Media Foundation MFT (`off`).
    Off,
    /// The default (unset, or `on`): the direct encoder of the context's GPU, whichever
    /// vendor it is.
    On,
}

impl Policy {
    /// The policy a value of `JSTREAM_DIRECT_ENCODERS` names, and whether the value named one; an
    /// unknown value keeps the default, so the caller can warn about it.
    pub(crate) fn named(value: Option<&str>) -> (Self, bool) {
        let Some(value) = value.map(str::trim) else {
            return (Self::On, true);
        };
        match value.to_ascii_lowercase().as_str() {
            "" | "on" | "1" | "true" | "auto" => (Self::On, true),
            "off" | "0" | "false" | "mf" => (Self::Off, true),
            _ => (Self::On, false),
        }
    }

    /// Whether to try `vendor`'s direct encoder.
    pub(crate) fn tries(self, vendor: Vendor) -> bool {
        self == Self::On && vendor.encoder().is_some()
    }
}

/// One frame in the encoder: the input slot it was copied into, the sequence number it
/// was sent with (the timestamp the encoder hands back on its packet) and its `pts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sent {
    /// The input slot, free again once the frame's packet is out.
    pub(crate) slot: usize,
    /// The timestamp the encoder carries through to the frame's packet.
    pub(crate) seq: u64,
    /// The frame's exact `pts`, which its packet gets back.
    pub(crate) pts: MediaTime,
}

/// The encoder's input slots (a texture each, plus whatever the vendor keeps beside it)
/// and the frames in flight, oldest first.
///
/// A slot is taken when a frame is copied into it and freed when that frame's packet
/// comes out: with no B-frames and no look-ahead, an encoder is done with an input once
/// its packet is out. Frames the encoder dropped are freed when a later frame's packet
/// arrives, since packets come out in the order frames went in.
#[derive(Debug)]
pub(crate) struct Slots {
    free: Vec<usize>,
    sent: VecDeque<Sent>,
    next_seq: u64,
}

impl Slots {
    /// `count` slots, all free.
    pub(crate) fn new(count: usize) -> Self {
        Self {
            // Popped from the end, so slot 0 is used first.
            free: (0..count).rev().collect(),
            sent: VecDeque::with_capacity(count),
            next_seq: 0,
        }
    }

    /// A free slot to copy the next frame into, or `None` when the encoder holds them
    /// all. The slot stays free until [`send`](Self::send) records a frame in it.
    pub(crate) fn peek_free(&self) -> Option<usize> {
        self.free.last().copied()
    }

    /// Records a frame copied into `slot` (which [`peek_free`](Self::peek_free) gave)
    /// and returns what to send it with.
    pub(crate) fn send(&mut self, slot: usize, pts: MediaTime) -> Sent {
        if let Some(i) = self.free.iter().rposition(|&s| s == slot) {
            self.free.remove(i);
        }
        let sent = Sent {
            slot,
            seq: self.next_seq,
            pts,
        };
        self.next_seq += 1;
        self.sent.push_back(sent);
        sent
    }

    /// Takes back a frame that was recorded but that the encoder refused, freeing its
    /// slot: the last one sent, when its sequence number is `seq`.
    pub(crate) fn unsend(&mut self, seq: u64) {
        if self.sent.back().is_some_and(|s| s.seq == seq)
            && let Some(sent) = self.sent.pop_back()
        {
            self.free.push(sent.slot);
        }
    }

    /// The oldest frame in flight, the next whose packet should come out.
    pub(crate) fn oldest(&self) -> Option<Sent> {
        self.sent.front().copied()
    }

    /// The packet of the frame sent with `seq` is out: frees its slot and every older
    /// frame's (dropped by the encoder), and returns it. `None` for a sequence number
    /// not in flight, which frees nothing.
    pub(crate) fn complete(&mut self, seq: u64) -> Option<Sent> {
        if !self.sent.iter().any(|s| s.seq == seq) {
            return None;
        }
        while let Some(sent) = self.sent.pop_front() {
            self.free.push(sent.slot);
            if sent.seq == seq {
                return Some(sent);
            }
        }
        None
    }

    /// Frames sent and not yet out.
    pub(crate) fn in_flight(&self) -> usize {
        self.sent.len()
    }
}

/// The Windows side: opening the direct encoder for the context's vendor.
#[cfg(windows)]
mod open {
    use jstream_platform::{HardwarePreference, PlatformError, VideoEncoder, VideoEncoderConfig};

    use super::{Policy, VARIABLE, Vendor};
    use crate::gpu::EncodeGpu;

    /// The policy [`VARIABLE`] names, with a warning for a value that names none.
    pub(crate) fn policy() -> Policy {
        let value = std::env::var(VARIABLE).ok();
        let (policy, known) = Policy::named(value.as_deref());
        if !known {
            tracing::warn!(
                variable = VARIABLE,
                value = value.as_deref().unwrap_or_default(),
                "not a value this variable takes (on or off); trying direct encoders"
            );
        }
        policy
    }

    /// Opens the direct encoder of `gpu`'s vendor for `config`, or says why not:
    /// `Ok(None)` when the policy or the vendor has none to try, and `Err` when it was
    /// tried and refused, which the caller logs before opening a Media Foundation
    /// encoder instead.
    pub(crate) fn open_direct(
        config: &VideoEncoderConfig,
        gpu: &EncodeGpu,
        policy: Policy,
    ) -> Result<Option<Box<dyn VideoEncoder>>, PlatformError> {
        let vendor = gpu.vendor();
        // Both are hardware encoders: a configuration that avoids hardware goes to the
        // MFT path, which refuses it.
        if !policy.tries(vendor) || config.hardware == HardwarePreference::Avoid {
            return Ok(None);
        }
        let encoder: Box<dyn VideoEncoder> = match vendor {
            Vendor::Nvidia => Box::new(super::nvenc::NvencEncoder::open(config, gpu)?),
            Vendor::Amd => Box::new(super::amf::AmfEncoder::open(config, gpu)?),
            Vendor::Other => return Ok(None),
        };
        Ok(Some(encoder))
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{MediaTime, Timebase};

    use super::{Policy, Slots, Vendor};

    #[test]
    fn vendors_come_from_the_pci_id() {
        assert_eq!(Vendor::of_pci(0x10DE), Vendor::Nvidia);
        assert_eq!(Vendor::of_pci(0x1002), Vendor::Amd);
        assert_eq!(Vendor::of_pci(0x8086), Vendor::Other);
        assert_eq!(Vendor::Nvidia.encoder(), Some("NVENC"));
        assert_eq!(Vendor::Amd.encoder(), Some("AMF"));
        assert_eq!(Vendor::Other.encoder(), None);
    }

    #[test]
    fn the_variable_turns_direct_encoders_off() {
        assert_eq!(Policy::named(None), (Policy::On, true));
        assert_eq!(Policy::named(Some("on")), (Policy::On, true));
        assert_eq!(Policy::named(Some(" OFF ")), (Policy::Off, true));
        assert_eq!(Policy::named(Some("0")), (Policy::Off, true));
        assert_eq!(Policy::named(Some("mf")), (Policy::Off, true));
        assert_eq!(Policy::named(Some("sometimes")), (Policy::On, false));
        assert!(Policy::On.tries(Vendor::Nvidia));
        assert!(Policy::On.tries(Vendor::Amd));
        assert!(!Policy::On.tries(Vendor::Other));
        assert!(!Policy::Off.tries(Vendor::Amd));
    }

    fn at(i: i64) -> MediaTime {
        MediaTime::new(i * 3000, Timebase::MPEG_90K)
    }

    #[test]
    fn slots_free_when_their_packet_is_out() {
        let mut slots = Slots::new(2);
        let a = slots.send(slots.peek_free().expect("free"), at(0));
        let b = slots.send(slots.peek_free().expect("free"), at(1));
        assert_ne!(a.slot, b.slot);
        assert_eq!((a.seq, b.seq), (0, 1));
        assert_eq!(slots.peek_free(), None, "both held");
        assert_eq!(slots.oldest(), Some(a));
        assert_eq!(slots.complete(a.seq), Some(a));
        assert_eq!(slots.peek_free(), Some(a.slot));
        assert_eq!(slots.in_flight(), 1);
        assert_eq!(slots.complete(b.seq).map(|s| s.pts), Some(at(1)));
        assert_eq!(slots.in_flight(), 0);
    }

    #[test]
    fn a_later_packet_frees_the_frames_the_encoder_dropped() {
        let mut slots = Slots::new(3);
        let sent: Vec<_> = (0..3)
            .map(|i| slots.send(slots.peek_free().expect("free"), at(i)))
            .collect();
        assert_eq!(slots.complete(sent[2].seq), Some(sent[2]));
        assert_eq!(slots.in_flight(), 0);
        let mut free = Vec::new();
        while let Some(slot) = slots.peek_free() {
            free.push(slot);
            slots.send(slot, at(9));
        }
        free.sort_unstable();
        assert_eq!(free, [0, 1, 2]);
    }

    #[test]
    fn an_unknown_packet_frees_nothing_and_a_refused_frame_is_taken_back() {
        let mut slots = Slots::new(2);
        let a = slots.send(slots.peek_free().expect("free"), at(0));
        assert_eq!(slots.complete(7), None);
        assert_eq!(slots.in_flight(), 1);
        let b = slots.send(slots.peek_free().expect("free"), at(1));
        // Only the last frame sent can be taken back.
        slots.unsend(a.seq);
        assert_eq!(slots.in_flight(), 2);
        slots.unsend(b.seq);
        assert_eq!(slots.in_flight(), 1);
        assert_eq!(slots.peek_free(), Some(b.slot));
        // Sequence numbers keep counting, so a packet can't be mistaken for another.
        assert_eq!(slots.send(b.slot, at(2)).seq, 2);
    }
}
