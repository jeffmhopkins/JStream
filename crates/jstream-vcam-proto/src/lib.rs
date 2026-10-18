//! The cross-process contract between the engine (the producer) and the virtual camera's
//! media source DLL (`jstream-vcam-windows`, the consumer, loaded by the Windows camera
//! Frame Server): the shared-memory control block, the names and security of the shared
//! objects, and the frame format. Layer 0: no dependencies, no `unsafe`, no Windows
//! bindings, so both sides and the tests build on any host.
//!
//! Taken from spike S2, which proved this hand-off on Windows 11.
//!
//! **Hand-off per frame `n`** (1-based): the producer renders into ring slot
//! [`slot_of(n, ring)`](slot_of), queues `Signal(fence, n)` on its D3D11 context, flushes,
//! then stores [`Control::latest_frame`] `= n` with release ordering. The DLL loads
//! `latest_frame` (acquire), picks a safe frame with [`choose_frame`], queues
//! `Wait(fence, m)` on its own context and copies slot `m`. The fence value is the frame
//! number. The ring has no back-pressure: the copy must finish within about one frame
//! period, before the producer comes back round to that slot.
//!
//! **Logging** is not part of this contract. Spike S2 logged to a `%ProgramData%` folder
//! every interactive user could write to; the DLL needs ETW or a location a user can't
//! redirect instead, which `jstream-vcam-windows` decides.
//!
//! **Compatibility:** a consumer uses a block only when [`Control::is_live`] holds, which
//! checks [`MAGIC`] and [`VERSION`]. Any change to the layout, the names or the meaning of
//! a field bumps [`VERSION`].
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-vcam-proto/README.md#purpose, docs/security/threat-model.md#vcam

#![forbid(unsafe_code)]

mod control;
mod names;

pub use control::{
    CONSUMER_KNOWN, CONSUMER_LOCAL_SERVICE, CONSUMER_SESSION0, CONSUMER_SVCHOST, Control,
    STATE_EMPTY, STATE_LIVE, STATE_STOPPED, choose_frame, slot_of,
};
pub use names::{
    CONTROL_NAME, SESSION_PROBE_MAX, Scope, control_open_names, fence_base, sddl_objects,
    sddl_user_only, texture_base,
};

/// `JSVC` in little-endian byte order, so a zeroed or foreign mapping is never mistaken for
/// a control block.
pub const MAGIC: u32 = u32::from_le_bytes(*b"JSVC");

/// The contract's version. Bumped on any change to the layout, the object names or the
/// meaning of a field.
pub const VERSION: u32 = 1;

/// The virtual camera's frame width in pixels. Frame Server sees exactly this size.
pub const WIDTH: u32 = 1920;
/// The virtual camera's frame height in pixels.
pub const HEIGHT: u32 = 1080;
/// The virtual camera's frame rate in frames per second.
pub const FPS: u32 = 30;

/// `DXGI_FORMAT_B8G8R8A8_UNORM`, the format of the ring textures. Written as a number so
/// this crate needs no Windows bindings.
pub const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;

/// Shared textures in the producer's ring.
pub const RING: u32 = 3;

/// A producer whose heartbeat is older than this many milliseconds is treated as gone.
pub const LIVENESS_MS: u64 = 1000;

/// Bytes mapped for the control block: one page. [`Control`] is far smaller, which a
/// compile-time assertion checks.
pub const MAPPING_SIZE: usize = 4096;

/// The media source's CLSID as a registry string, also the `sourceId` given to
/// `MFCreateVirtualCamera`. Distinct from spike S2's, so the two never collide.
pub const CLSID_STR: &str = "{0C867C2A-F49D-418D-8712-5F83CE096CCC}";
/// [`CLSID_STR`] as a number (`GUID::from_u128` takes this form).
pub const CLSID_U128: u128 = 0x0c867c2a_f49d_418d_8712_5f83ce096ccc;

/// The camera's name in the Camera app and in browsers.
pub const FRIENDLY_NAME: &str = "JStream Virtual Camera";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clsid_string_matches_value() {
        let hex: String = CLSID_STR.chars().filter(char::is_ascii_hexdigit).collect();
        assert_eq!(u128::from_str_radix(&hex, 16), Ok(CLSID_U128));
        // Registry form: braces, upper case, 8-4-4-4-12.
        let inner = CLSID_STR.trim_start_matches('{').trim_end_matches('}');
        let groups: Vec<usize> = inner.split('-').map(str::len).collect();
        assert_eq!(groups, [8, 4, 4, 4, 12]);
        assert_eq!(inner, inner.to_ascii_uppercase());
    }

    #[test]
    fn magic_spells_jsvc() {
        assert_eq!(MAGIC.to_le_bytes(), *b"JSVC");
        assert_ne!(MAGIC, 0);
    }
}
