//! The shared block of one CLAP filter instance (`jstream-plugin-proto`, ADR-0035), as an
//! unnamed Windows file mapping: the engine creates it and duplicates its handle into the
//! plugin host it starts, which maps it from that handle. With no name, no other process
//! can open it, and the handle isn't inheritable, so no other child of the engine gets it
//! either (#351). Plus the host's real-time thread registration with MMCSS.
//!
//! # Behaviour
//!
//! **`PluginBlock`** and **`ProAudioThread`**, for CLAP filters hosted out of process
//! (ADR-0035).
//! - `PluginBlock` is one filter instance's
//!   `jstream-plugin-proto` `Shared` block
//!   as an unnamed file mapping (#351). `create` (the engine) makes a new, zeroed one
//!   exactly the block's size, with a DACL naming the user's SID alone and a handle that
//!   isn't inheritable. With no name, no other process can open it, and no other child
//!   of the engine inherits it. `share_with` duplicates its handle into one host process
//!   and returns the handle's value there. `from_handle` (the plugin host) maps the
//!   section from that value, taking ownership of the handle once it maps (a value that
//!   doesn't map is left open, since it may not be the host's to close), and refuses a
//!   value that can't be a handle or a section smaller than this version's block. Both give
//!   `&Shared` through `AsRef`, unmap on drop, and are `Send` and `Sync`, since the
//!   block is all atomics.
//! - `ProAudioThread::register` registers the calling thread with MMCSS "Pro Audio"
//!   until the guard drops, as the WASAPI threads do. It is best effort, and the guard
//!   isn't `Send`.
//! - `WindowsPluginHosts` is the `PluginHosts` backend. It runs `PLUGIN_HOST_EXE`
//!   (`jstream-plugin-host.exe`, beside the engine's executable, or another path through
//!   `with_exe`). Every host goes in one job object that kills its processes when the
//!   engine's handle closes (and suppresses the crash dialog), so no host outlives the
//!   engine. A host starts with a cleared environment that keeps only the system and
//!   user-folder variables (`KEPT_ENV`), so it never sees the engine's keys, tokens or
//!   configuration, and its stderr is discarded. `start` creates an unnamed block,
//!   initialised with the request's configuration, starts `run <file> <plugin-id>`,
//!   duplicates the block's handle into the new process and sends its value as the
//!   first control message (`Block`), before `Start`. It keeps the stdin and stdout
//!   pipes as the control channel; a thread decodes stdout into messages and stops on a
//!   frame that doesn't decode. The host's block is an `EngineBlock`, so the mixer's miss
//!   counts stay in the engine's memory, where the host can't reset them. Dropping a host kills it. `scan` runs `scan <file>` and kills the
//!   host after its answer, or after 20 s. `search_paths` is CLAP's order on Windows:
//!   `CLAP_PATH`, then `%COMMONPROGRAMFILES%\CLAP` and
//!   `%LOCALAPPDATA%\Programs\Common\CLAP`, keeping the ones that exist, without
//!   repeats.
//! - **Not yet:** a test that starts a real host process (CI's Windows job builds the
//!   launcher and tests `search_paths` only), and keeping the plugin from inheriting handles other threads make
//!   inheritable while it starts.

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
        Self::map(Handle(handle), true)
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
    /// `value` from its `ToHost::Block`, taking ownership of that handle once it maps.
    /// The host's side.
    ///
    /// `value` comes from the engine, the trusted side, but this stays a safe function:
    /// if it names something other than a section, `MapViewOfFile` fails, and then the
    /// value is left open rather than closed, so a handle some other code owns is never
    /// closed through it. Only a value that maps as a section is owned from then on.
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
        Self::map(Handle(HANDLE(raw as *mut std::ffi::c_void)), false)
    }

    /// Maps the whole of `handle`'s section and checks it holds a `Shared`. When the
    /// mapping fails and `owned` is false, the handle is left open: it may not be ours.
    fn map(handle: Handle, owned: bool) -> Result<Self, PlatformError> {
        // SAFETY: `MapViewOfFile` takes any handle value: one that isn't a live section
        // this process may map fails with a null view, which is checked below, and maps
        // nothing. 0 maps the whole section.
        let view = unsafe { MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
        if view.Value.is_null() {
            if !owned {
                std::mem::forget(handle);
            }
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
