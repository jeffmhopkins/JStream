//! The shared block of one CLAP filter instance (`jstream-plugin-proto`, ADR-0035), as an
//! unnamed Windows file mapping: the engine creates it and duplicates its handle into the
//! plugin host it starts, which maps it from that handle. With no name, no other process
//! can open it, and the handle isn't inheritable, so no other child of the engine gets it
//! either (#351). Plus the host's real-time thread registration with MMCSS.

use jstream_platform::PlatformError;
use jstream_plugin_proto::Shared;
use windows::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::core::PCWSTR;

use crate::com::{Mmcss, OsResult};
use crate::vcam::shared::{Handle, SecurityAttributes, user_sid};

/// The bytes a block needs.
const SIZE: usize = size_of::<Shared>();

/// One CLAP filter instance's shared block, mapped into this process. Unmapped on drop;
/// the mapping itself lives while either process has it open.
pub struct PluginBlock {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    handle: Handle,
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
    /// Creates a new, zeroed, unnamed block, with a DACL that grants only this user. The
    /// engine's side: the host gets it through [`PluginBlock::share_with`].
    ///
    /// # Errors
    /// The OS refuses.
    pub fn create() -> Result<Self, PlatformError> {
        let sid = user_sid()?;
        let sa = SecurityAttributes::from_sddl(&format!("D:P(A;;GA;;;{sid})"))?;
        let size = u32::try_from(SIZE).map_err(|_| PlatformError::Unsupported {
            what: "a plugin block larger than 4 GiB".into(),
        })?;
        // SAFETY: `sa` outlives the call; a null name makes an unnamed section, and
        // `sa` says the handle isn't inheritable.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(sa.as_ptr()),
                PAGE_READWRITE,
                0,
                size,
                PCWSTR::null(),
            )
        }
        .os("CreateFileMappingW")?;
        Self::map(Handle(handle))
    }

    /// Duplicates the block's section handle into `process` (a host the engine just
    /// started, opened with `PROCESS_DUP_HANDLE`), returning the handle's value there for
    /// the host's `ToHost::Block`. The copy isn't inheritable and closes with that
    /// process.
    ///
    /// # Errors
    /// The OS refuses, as when the process has already exited.
    pub fn share_with(&self, process: HANDLE) -> Result<u64, PlatformError> {
        let mut target = HANDLE::default();
        // SAFETY: both process handles and the section handle are live for the call
        // (the current process's is a pseudo-handle), and `target` is a valid
        // out-pointer.
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle.0,
                process,
                &mut target,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .os("DuplicateHandle")?;
        Ok(target.0 as usize as u64)
    }

    /// Maps the block whose section handle the engine duplicated into this process,
    /// `value` from its `ToHost::Block`, taking ownership of that handle. The host's
    /// side.
    ///
    /// # Errors
    /// `value` can't be a handle, or isn't a section at least a block's size.
    pub fn from_handle(value: u64) -> Result<Self, PlatformError> {
        // Handle values are non-zero multiples of 4 that fit a pointer.
        let raw = usize::try_from(value)
            .ok()
            .filter(|&v| v != 0 && v % 4 == 0)
            .ok_or_else(|| PlatformError::Unsupported {
                what: format!("a plugin block handle of {value:#x}"),
            })?;
        Self::map(Handle(HANDLE(raw as *mut std::ffi::c_void)))
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
        let block = Self { view, handle };
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
