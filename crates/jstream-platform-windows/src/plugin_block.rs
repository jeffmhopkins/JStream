//! The shared block of one CLAP filter instance (`jstream-plugin-proto`, ADR-0035), as a
//! named Windows file mapping: the engine creates it, and the plugin host it starts opens
//! it by name. Plus the host's real-time thread registration with MMCSS.

use jstream_platform::PlatformError;
use jstream_plugin_proto::Shared;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery,
};
use windows::core::HSTRING;

use crate::com::{Mmcss, OsResult};
use crate::vcam::shared::{Handle, SecurityAttributes, user_sid};

/// The bytes a block needs.
const SIZE: usize = size_of::<Shared>();

/// One CLAP filter instance's shared block, mapped into this process. Unmapped on drop;
/// the mapping itself lives while either process has it open.
pub struct PluginBlock {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    _handle: Handle,
}

// SAFETY: the view is plain shared memory that is only ever read and written through
// `Shared`'s atomics, and the handle is owned (see `Handle`).
unsafe impl Send for PluginBlock {}
// SAFETY: as above: `&PluginBlock` gives only `&Shared`, which is all atomics.
unsafe impl Sync for PluginBlock {}

impl std::fmt::Debug for PluginBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginBlock").finish_non_exhaustive()
    }
}

impl PluginBlock {
    /// Creates a new, zeroed block named `name` (`Local\JStreamPlugin-…`) that only this
    /// user can open. The engine's side; the name must be new.
    ///
    /// # Errors
    /// The OS refuses, or a mapping of that name already exists.
    pub fn create(name: &str) -> Result<Self, PlatformError> {
        let sid = user_sid()?;
        let sa = SecurityAttributes::from_sddl(&format!("D:P(A;;GA;;;{sid})"))?;
        let size = u32::try_from(SIZE).map_err(|_| PlatformError::Unsupported {
            what: "a plugin block larger than 4 GiB".into(),
        })?;
        // SAFETY: `sa` outlives the call; the name is a valid wide string.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(sa.as_ptr()),
                PAGE_READWRITE,
                0,
                size,
                &HSTRING::from(name),
            )
        }
        .os("CreateFileMappingW")?;
        // SAFETY: plain call, right after the call whose status it reads.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let handle = Handle(handle);
        if existed {
            return Err(PlatformError::Unsupported {
                what: format!("a second plugin block named {name}"),
            });
        }
        Self::map(handle)
    }

    /// Opens the block named `name` that the engine created. The host's side.
    ///
    /// # Errors
    /// No such mapping, or one too small to be a block.
    pub fn open(name: &str) -> Result<Self, PlatformError> {
        // SAFETY: the name is a valid wide string.
        let handle =
            unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &HSTRING::from(name)) }
                .os("OpenFileMappingW")?;
        Self::map(Handle(handle))
    }

    /// Maps the whole of `handle`'s section and checks it holds a `Shared`.
    fn map(handle: Handle) -> Result<Self, PlatformError> {
        // SAFETY: `handle` is a live section handle; 0 maps the whole section.
        let view = unsafe { MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            return Err(PlatformError::Os {
                call: "MapViewOfFile",
                code: None,
                message: windows::core::Error::from_thread().message(),
            });
        }
        let block = Self {
            view,
            _handle: handle,
        };
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: `view` is a live mapping in this process; `info` is writable for its
        // size.
        let got = unsafe {
            VirtualQuery(
                Some(block.view.Value.cast_const()),
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if got == 0 || info.RegionSize < SIZE {
            return Err(PlatformError::Unsupported {
                what: "a plugin block smaller than this version's".into(),
            });
        }
        Ok(block)
    }
}

impl AsRef<Shared> for PluginBlock {
    fn as_ref(&self) -> &Shared {
        // SAFETY: the view is at least `SIZE` bytes (checked in `map`), page-aligned
        // (more than `Shared`'s 8) and mapped until `self` drops; `Shared` is all atomics,
        // so any bit pattern, and the other process writing it, are valid.
        unsafe { &*self.view.Value.cast::<Shared>() }
    }
}

impl Drop for PluginBlock {
    fn drop(&mut self) {
        // SAFETY: unmapped exactly once; no `as_ref()` reference outlives `self`.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
        }
    }
}

/// This thread registered with MMCSS as "Pro Audio" (ARCH › Threads, ADR-0014) until
/// dropped. Best effort: a thread MMCSS refuses still runs. Not `Send`: it must be
/// dropped on the thread that registered.
pub struct ProAudioThread {
    _mmcss: Mmcss,
    _thread: std::marker::PhantomData<*const ()>,
}

impl std::fmt::Debug for ProAudioThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProAudioThread").finish_non_exhaustive()
    }
}

impl ProAudioThread {
    /// Registers the calling thread.
    pub fn register() -> Self {
        Self {
            _mmcss: Mmcss::pro_audio(),
            _thread: std::marker::PhantomData,
        }
    }
}
