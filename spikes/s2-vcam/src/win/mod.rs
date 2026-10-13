//! Windows-only code. `shared`, `reg` and `log` are used by both the DLL and the exe; the
//! COM objects (`dll`, `activate`, `source`, `stream`, `frames`) run inside Frame Server.

mod activate;
mod attrs;
pub mod dll;
mod frames;
pub mod log;
pub mod reg;
pub mod shared;
mod source;
mod stream;

use windows_core::GUID;

/// The media source's class id.
pub const CLSID: GUID = GUID::from_u128(crate::proto::CLSID_U128);
