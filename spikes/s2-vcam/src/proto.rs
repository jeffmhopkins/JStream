//! The cross-process contract between the producer exe and the media source DLL: the
//! shared-memory control block, the names of the shared objects, and the frame format.
//!
//! Platform-independent (plain atomics), so its tests run on any host.
//!
//! Hand-off per frame `n` (1-based): the producer renders into ring slot `n % RING`, queues
//! `Signal(fence, n)` on its D3D11 context, flushes, then stores `latest_frame = n` with
//! release ordering. The DLL loads `latest_frame` (acquire), queues `Wait(fence, n)` on its
//! own context and copies slot `n % RING`. The fence value is the frame number.

use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

use crate::evidence::ConsumerSnapshot;

/// `JS2V`, so a zeroed or foreign mapping is never mistaken for ours.
pub const MAGIC: u32 = u32::from_le_bytes(*b"JS2V");
/// Bumped on any layout change.
pub const VERSION: u32 = 2;

/// The virtual camera's frame size and rate. Frame Server sees exactly this.
pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const FPS: u32 = 30;

/// Shared textures in the producer's ring.
pub const RING: u32 = 3;

/// A producer whose heartbeat is older than this is treated as gone.
pub const LIVENESS_MS: u64 = 1000;

/// Bytes mapped for the control block (one page; the struct is far smaller).
pub const MAPPING_SIZE: usize = 4096;

/// The media source's CLSID, also the `sourceId` given to `MFCreateVirtualCamera`.
/// Fresh for this spike, so it can't collide with VCamSample or a later JStream build.
pub const CLSID_STR: &str = "{491711D3-16FD-46F0-A9A3-E6CA192679DE}";
pub const CLSID_U128: u128 = 0x491711d3_16fd_46f0_a9a3_e6ca192679de;

/// The camera's name in the Camera app and browsers.
pub const FRIENDLY_NAME: &str = "JStream S2";

/// Base name of the control block's file mapping.
pub const CONTROL_NAME: &str = "JStreamS2-Control";

/// Where the producer created its named objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `Global\…`: visible to every session, including Frame Server's session 0.
    /// Creating a global file mapping needs `SeCreateGlobalPrivilege`, which a
    /// non-elevated user usually lacks.
    Global,
    /// The producer's own session namespace (`Local\…`). The DLL, in session 0, opens it
    /// through `Session\<id>\…`.
    Session,
}

impl Scope {
    pub fn as_u32(self) -> u32 {
        match self {
            Scope::Global => 1,
            Scope::Session => 2,
        }
    }

    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Scope::Global),
            2 => Some(Scope::Session),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Session => "session",
        }
    }

    /// The name the producer passes when creating an object in this scope.
    pub fn create_name(self, base: &str) -> String {
        match self {
            Scope::Global => format!("Global\\{base}"),
            Scope::Session => format!("Local\\{base}"),
        }
    }

    /// The name another session (Frame Server) opens the object by.
    pub fn open_name(self, session_id: u32, base: &str) -> String {
        match self {
            Scope::Global => format!("Global\\{base}"),
            Scope::Session => format!("Session\\{session_id}\\{base}"),
        }
    }
}

/// Base name of ring texture `slot`. The producer's PID keeps names unique across
/// producer restarts while the DLL may still hold the previous objects open.
pub fn texture_base(producer_pid: u32, slot: u32) -> String {
    format!("JStreamS2-Tex{slot}-{producer_pid}")
}

/// Base name of the shared fence.
pub fn fence_base(producer_pid: u32) -> String {
    format!("JStreamS2-Fence-{producer_pid}")
}

/// `state` values.
pub const STATE_EMPTY: u32 = 0;
pub const STATE_LIVE: u32 = 1;
pub const STATE_STOPPED: u32 = 2;

/// The control block at the start of the shared mapping. Every field is atomic, so both
/// processes can read and write it concurrently without UB. The producer writes the
/// "producer" fields and the DLL the "consumer" fields.
///
/// Producer fields other than the counters are written before `state = STATE_LIVE`
/// (release) and only read after observing it (acquire).
#[repr(C)]
#[derive(Default)]
pub struct Control {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    // --- producer ---
    pub state: AtomicU32,
    /// [`Scope::as_u32`] of the texture and fence names.
    pub scope: AtomicU32,
    pub producer_pid: AtomicU32,
    /// The producer's session id, for `Session\<id>\…` names.
    pub session_id: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// `DXGI_FORMAT` of the ring textures (B8G8R8A8_UNORM).
    pub dxgi_format: AtomicU32,
    pub ring: AtomicU32,
    /// The adapter the textures live on (`LUID` as `high << 32 | low`).
    pub adapter_luid: AtomicU64,
    /// The newest complete frame; also the fence value signalled after it. 0 = none yet.
    pub latest_frame: AtomicU64,
    /// QPC when `latest_frame` was published.
    pub latest_qpc: AtomicU64,
    /// QPC of the producer's last loop iteration (liveness).
    pub heartbeat_qpc: AtomicU64,
    // --- consumer (the DLL) ---
    pub consumer_pid: AtomicU32,
    /// Times a DLL instance attached to this block.
    pub consumer_attaches: AtomicU32,
    /// Samples delivered to Frame Server, by origin.
    pub frames_served: AtomicU64,
    pub frames_shared: AtomicU64,
    pub frames_pattern: AtomicU64,
    /// Times the DLL (re)opened the shared textures and fence successfully.
    pub gpu_opens: AtomicU32,
    /// Last failing HRESULT on the consumer side (0 = none).
    pub consumer_last_hr: AtomicI32,
    /// QPC of the DLL's last served sample.
    pub consumer_heartbeat_qpc: AtomicU64,
    /// Session id of the process that last attached (0 = services, such as Frame Server).
    pub consumer_session_id: AtomicU32,
    /// `CONSUMER_*` flags describing the process that last attached.
    pub consumer_flags: AtomicU32,
    /// Media sources created by a DLL instance that had this block attached.
    pub consumer_activations: AtomicU32,
}

/// `consumer_flags`: the host process runs in session 0 (where services run).
pub const CONSUMER_SESSION0: u32 = 1;
/// `consumer_flags`: the host process's exe is `svchost.exe` (Frame Server's host).
pub const CONSUMER_SVCHOST: u32 = 2;
/// `consumer_flags`: the host process runs as LOCAL SERVICE (checked by SID).
pub const CONSUMER_LOCAL_SERVICE: u32 = 4;
/// `consumer_flags`: the flags were written at all (a v2 DLL attached).
pub const CONSUMER_KNOWN: u32 = 8;

const _: () = assert!(std::mem::size_of::<Control>() <= MAPPING_SIZE);

impl Control {
    /// The consumer-side counters and identity, for before/after comparison.
    pub fn consumer_snapshot(&self) -> ConsumerSnapshot {
        ConsumerSnapshot {
            pid: self.consumer_pid.load(Ordering::Acquire),
            attaches: self.consumer_attaches.load(Ordering::Acquire),
            activations: self.consumer_activations.load(Ordering::Acquire),
            served: self.frames_served.load(Ordering::Acquire),
            shared: self.frames_shared.load(Ordering::Acquire),
            session_id: self.consumer_session_id.load(Ordering::Acquire),
            flags: self.consumer_flags.load(Ordering::Acquire),
        }
    }

    /// Whether a producer published a compatible, live block.
    pub fn is_live(&self) -> bool {
        self.magic.load(Ordering::Acquire) == MAGIC
            && self.version.load(Ordering::Acquire) == VERSION
            && self.state.load(Ordering::Acquire) == STATE_LIVE
    }

    /// Whether the producer's heartbeat is recent, given the current QPC and frequency.
    pub fn heartbeat_fresh(&self, now_qpc: u64, qpc_freq: u64) -> bool {
        let beat = self.heartbeat_qpc.load(Ordering::Acquire);
        let limit = qpc_freq.saturating_mul(LIVENESS_MS) / 1000;
        beat != 0 && now_qpc.saturating_sub(beat) <= limit
    }
}

/// Which frame the DLL should copy, given the newest published frame `latest`, the
/// fence's completed value and the ring size. Only a frame whose fence signal already
/// completed is used, so the DLL never blocks on a signal a dead producer can't deliver.
/// `None` if no complete frame is safe: frame `m` sits in slot `m % ring`, and the
/// producer may already be rendering `latest + 1`, so `m` must be newer than
/// `latest + 1 - ring`.
pub fn choose_frame(latest: u64, completed: u64, ring: u32) -> Option<u64> {
    let m = latest.min(completed);
    (m > 0 && latest - m + 1 < u64::from(ring.max(1))).then_some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_choice() {
        assert_eq!(choose_frame(10, 10, 3), Some(10));
        assert_eq!(choose_frame(10, 50, 3), Some(10));
        assert_eq!(choose_frame(10, 9, 3), Some(9));
        // Frame 8 is in the slot the producer may be rewriting with frame 11.
        assert_eq!(choose_frame(10, 8, 3), None);
        assert_eq!(choose_frame(0, 0, 3), None);
        assert_eq!(choose_frame(5, 0, 3), None);
    }

    #[test]
    fn names() {
        assert_eq!(
            Scope::Session.open_name(1, &texture_base(42, 2)),
            "Session\\1\\JStreamS2-Tex2-42"
        );
        assert_eq!(
            Scope::Global.create_name(CONTROL_NAME),
            "Global\\JStreamS2-Control"
        );
        assert_eq!(
            Scope::from_u32(Scope::Session.as_u32()),
            Some(Scope::Session)
        );
        assert_eq!(Scope::from_u32(0), None);
    }

    #[test]
    fn clsid_string_matches_value() {
        let hex: String = CLSID_STR.chars().filter(char::is_ascii_hexdigit).collect();
        assert_eq!(u128::from_str_radix(&hex, 16), Ok(CLSID_U128));
    }

    #[test]
    fn liveness() {
        let c = Control::default();
        assert!(!c.is_live());
        c.magic.store(MAGIC, Ordering::Release);
        c.version.store(VERSION, Ordering::Release);
        c.state.store(STATE_LIVE, Ordering::Release);
        assert!(c.is_live());
        c.heartbeat_qpc.store(1_000, Ordering::Release);
        assert!(c.heartbeat_fresh(1_500, 1_000));
        assert!(!c.heartbeat_fresh(3_000, 1_000));
    }
}
