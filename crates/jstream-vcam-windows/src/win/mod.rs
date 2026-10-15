//! The COM objects Frame Server loads, and what they need: the DLL exports and panic
//! guard (`dll`), the registered activator (`activate`), the media source and its stream
//! (`source`, `stream`), where the pixels come from (`frames`), the control-block mapping
//! (`shared`), the log (`log`) and the self-registration (`reg`).

mod activate;
mod attrs;
mod dll;
mod frames;
mod log;
mod reg;
mod shared;
mod source;
mod stream;

use windows_core::GUID;

/// The media source's class id, from the contract.
const CLSID: GUID = GUID::from_u128(jstream_vcam_proto::CLSID_U128);
