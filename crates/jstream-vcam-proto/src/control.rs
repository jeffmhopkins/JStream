//! The control block at the start of the shared mapping, and the frame-selection rule.

use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

use crate::{LIVENESS_MS, MAGIC, MAPPING_SIZE, VERSION};

/// [`Control::state`]: no producer has published yet (a fresh, zeroed mapping).
pub const STATE_EMPTY: u32 = 0;
/// [`Control::state`]: a producer is publishing frames.
pub const STATE_LIVE: u32 = 1;
/// [`Control::state`]: the producer stopped cleanly.
pub const STATE_STOPPED: u32 = 2;

/// [`Control::consumer_flags`]: the host process runs in session 0, where services run.
pub const CONSUMER_SESSION0: u32 = 1;
/// [`Control::consumer_flags`]: the host process's exe is `svchost.exe`, Frame Server's host.
pub const CONSUMER_SVCHOST: u32 = 2;
/// [`Control::consumer_flags`]: the host process runs as LOCAL SERVICE (checked by SID).
pub const CONSUMER_LOCAL_SERVICE: u32 = 4;
/// [`Control::consumer_flags`]: the consumer wrote the flags at all.
pub const CONSUMER_KNOWN: u32 = 8;

/// The control block at offset 0 of the shared mapping.
///
/// Every field is atomic, so both processes can read and write it concurrently, and every
/// bit pattern (including the OS's zero fill) is a valid value. The producer writes the
/// producer fields and the consumer the consumer fields.
///
/// The producer writes its fields other than the counters and timestamps before storing
/// `state = STATE_LIVE` (release), and the consumer reads them only after observing it
/// (acquire). Timestamps are QPC values, which compare across processes.
///
/// The layout is fixed by `repr(C)` and pinned by compile-time assertions on every
/// field's offset, so it is the same on every target.
#[repr(C)]
#[derive(Debug, Default)]
pub struct Control {
    /// [`MAGIC`] once a producer has initialised the block.
    pub magic: AtomicU32,
    /// [`VERSION`] of the producer that initialised the block.
    pub version: AtomicU32,

    // --- producer ---
    /// One of `STATE_*`.
    pub state: AtomicU32,
    /// [`Scope::as_u32`](crate::Scope::as_u32) of the namespace holding the texture and
    /// fence names.
    pub scope: AtomicU32,
    /// The producer's process id, part of the texture and fence names.
    pub producer_pid: AtomicU32,
    /// The producer's session id, for `Session\<id>\…` names.
    pub session_id: AtomicU32,
    /// Width of the ring textures in pixels.
    pub width: AtomicU32,
    /// Height of the ring textures in pixels.
    pub height: AtomicU32,
    /// `DXGI_FORMAT` of the ring textures
    /// ([`DXGI_FORMAT_B8G8R8A8_UNORM`](crate::DXGI_FORMAT_B8G8R8A8_UNORM)).
    pub dxgi_format: AtomicU32,
    /// Number of textures in the ring.
    pub ring: AtomicU32,
    /// The adapter the textures live on: its `LUID` as `high << 32 | low`.
    pub adapter_luid: AtomicU64,
    /// The newest complete frame, and the fence value signalled after it. 0 means none yet.
    pub latest_frame: AtomicU64,
    /// QPC when `latest_frame` was published.
    pub latest_qpc: AtomicU64,
    /// QPC of the producer's last loop iteration (liveness).
    pub heartbeat_qpc: AtomicU64,

    // --- consumer (the DLL) ---
    /// The process id of the consumer that last attached.
    pub consumer_pid: AtomicU32,
    /// Times a consumer attached to this block.
    pub consumer_attaches: AtomicU32,
    /// Samples delivered to Frame Server, from any origin.
    pub frames_served: AtomicU64,
    /// Samples delivered from the producer's frames.
    pub frames_shared: AtomicU64,
    /// Samples delivered from the consumer's own fallback pattern (no live producer).
    pub frames_pattern: AtomicU64,
    /// Times the consumer opened the shared textures and fence successfully.
    pub gpu_opens: AtomicU32,
    /// The consumer's last failing `HRESULT`, 0 for none.
    pub consumer_last_hr: AtomicI32,
    /// QPC of the consumer's last served sample.
    pub consumer_heartbeat_qpc: AtomicU64,
    /// Session id of the consumer that last attached (0 for services such as Frame Server).
    pub consumer_session_id: AtomicU32,
    /// `CONSUMER_*` flags describing the consumer that last attached.
    pub consumer_flags: AtomicU32,
    /// Media sources created by a consumer that had this block attached.
    pub consumer_activations: AtomicU32,
    /// Always 0. Makes the tail padding explicit, so the struct has none.
    pub reserved: AtomicU32,
}

// The layout is the contract: any change here is a VERSION bump.
const _: () = {
    assert!(size_of::<Control>() == 136);
    assert!(align_of::<Control>() == 8);
    assert!(size_of::<Control>() <= MAPPING_SIZE);
    assert!(offset_of!(Control, magic) == 0);
    assert!(offset_of!(Control, version) == 4);
    assert!(offset_of!(Control, state) == 8);
    assert!(offset_of!(Control, scope) == 12);
    assert!(offset_of!(Control, producer_pid) == 16);
    assert!(offset_of!(Control, session_id) == 20);
    assert!(offset_of!(Control, width) == 24);
    assert!(offset_of!(Control, height) == 28);
    assert!(offset_of!(Control, dxgi_format) == 32);
    assert!(offset_of!(Control, ring) == 36);
    assert!(offset_of!(Control, adapter_luid) == 40);
    assert!(offset_of!(Control, latest_frame) == 48);
    assert!(offset_of!(Control, latest_qpc) == 56);
    assert!(offset_of!(Control, heartbeat_qpc) == 64);
    assert!(offset_of!(Control, consumer_pid) == 72);
    assert!(offset_of!(Control, consumer_attaches) == 76);
    assert!(offset_of!(Control, frames_served) == 80);
    assert!(offset_of!(Control, frames_shared) == 88);
    assert!(offset_of!(Control, frames_pattern) == 96);
    assert!(offset_of!(Control, gpu_opens) == 104);
    assert!(offset_of!(Control, consumer_last_hr) == 108);
    assert!(offset_of!(Control, consumer_heartbeat_qpc) == 112);
    assert!(offset_of!(Control, consumer_session_id) == 120);
    assert!(offset_of!(Control, consumer_flags) == 124);
    assert!(offset_of!(Control, consumer_activations) == 128);
    assert!(offset_of!(Control, reserved) == 132);
};

impl Control {
    /// Whether a producer of this contract version published a live block.
    pub fn is_live(&self) -> bool {
        self.magic.load(Ordering::Acquire) == MAGIC
            && self.version.load(Ordering::Acquire) == VERSION
            && self.state.load(Ordering::Acquire) == STATE_LIVE
    }

    /// Whether the producer's heartbeat is recent, given the current QPC value and the QPC
    /// frequency (ticks per second).
    pub fn heartbeat_fresh(&self, now_qpc: u64, qpc_freq: u64) -> bool {
        let beat = self.heartbeat_qpc.load(Ordering::Acquire);
        let limit = qpc_freq.saturating_mul(LIVENESS_MS) / 1000;
        beat != 0 && now_qpc.saturating_sub(beat) <= limit
    }
}

/// The ring slot that holds frame `frame`, for a ring of `ring` textures (0 is treated
/// as 1).
pub fn slot_of(frame: u64, ring: u32) -> u32 {
    // The remainder is below `ring`, so it fits in a u32.
    u32::try_from(frame % u64::from(ring.max(1))).unwrap_or(0)
}

/// Which frame the consumer should copy, given the newest published frame `latest`, the
/// fence's completed value `completed` and the ring size.
///
/// Only a frame whose fence signal already completed is used, so the consumer never waits
/// on a signal a dead producer can't deliver. Returns `None` if no complete frame is safe:
/// frame `m` sits in slot `m % ring`, and the producer may already be rendering
/// `latest + 1`, so `m` must be newer than `latest + 1 - ring`.
pub fn choose_frame(latest: u64, completed: u64, ring: u32) -> Option<u64> {
    let m = latest.min(completed);
    (m > 0 && latest - m + 1 < u64::from(ring.max(1))).then_some(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RING;

    #[test]
    fn frame_choice() {
        assert_eq!(choose_frame(10, 10, RING), Some(10));
        assert_eq!(choose_frame(10, 50, RING), Some(10));
        assert_eq!(choose_frame(10, 9, RING), Some(9));
        // Frame 8 is in the slot the producer may be rewriting with frame 11.
        assert_eq!(choose_frame(10, 8, RING), None);
        assert_eq!(choose_frame(0, 0, RING), None);
        assert_eq!(choose_frame(5, 0, RING), None);
        // A one-slot ring is never safe to read, and a zero-slot ring behaves like it.
        assert_eq!(choose_frame(10, 10, 1), None);
        assert_eq!(choose_frame(10, 10, 0), None);
    }

    #[test]
    fn chosen_frame_never_shares_the_next_frames_slot() {
        for latest in 1..20u64 {
            for completed in 0..=latest + 2 {
                if let Some(m) = choose_frame(latest, completed, RING) {
                    assert!(m <= completed && m <= latest);
                    assert_ne!(slot_of(m, RING), slot_of(latest + 1, RING));
                }
            }
        }
    }

    #[test]
    fn slots() {
        assert_eq!(slot_of(1, RING), 1);
        assert_eq!(slot_of(3, RING), 0);
        assert_eq!(slot_of(5, RING), 2);
        assert_eq!(slot_of(7, 0), 0);
        assert_eq!(slot_of(u64::MAX, RING), (u64::MAX % 3) as u32);
    }

    #[test]
    fn a_zeroed_block_is_empty_and_not_live() {
        let c = Control::default();
        assert_eq!(c.state.load(Ordering::Relaxed), STATE_EMPTY);
        assert!(!c.is_live());
        assert!(!c.heartbeat_fresh(1_000, 1_000));
    }

    #[test]
    fn liveness() {
        let c = Control::default();
        c.magic.store(MAGIC, Ordering::Release);
        c.version.store(VERSION, Ordering::Release);
        c.state.store(STATE_LIVE, Ordering::Release);
        assert!(c.is_live());
        c.heartbeat_qpc.store(1_000, Ordering::Release);
        assert!(c.heartbeat_fresh(1_500, 1_000));
        assert!(c.heartbeat_fresh(2_000, 1_000));
        assert!(!c.heartbeat_fresh(3_000, 1_000));
        // A clock behind the heartbeat (another core's QPC read) is still fresh.
        assert!(c.heartbeat_fresh(900, 1_000));

        c.state.store(STATE_STOPPED, Ordering::Release);
        assert!(!c.is_live());
    }

    #[test]
    fn another_version_or_magic_is_not_live() {
        let c = Control::default();
        c.magic.store(MAGIC, Ordering::Release);
        c.version.store(VERSION + 1, Ordering::Release);
        c.state.store(STATE_LIVE, Ordering::Release);
        assert!(!c.is_live());
        c.version.store(VERSION, Ordering::Release);
        c.magic
            .store(u32::from_le_bytes(*b"JS2V"), Ordering::Release);
        assert!(!c.is_live());
    }

    #[test]
    fn consumer_flags_are_distinct_bits() {
        let flags = [
            CONSUMER_SESSION0,
            CONSUMER_SVCHOST,
            CONSUMER_LOCAL_SERVICE,
            CONSUMER_KNOWN,
        ];
        let mut seen = 0;
        for f in flags {
            assert_eq!(f.count_ones(), 1);
            assert_eq!(seen & f, 0);
            seen |= f;
        }
    }
}
