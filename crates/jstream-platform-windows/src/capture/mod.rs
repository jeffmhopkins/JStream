//! Display and window capture through Windows.Graphics.Capture: the
//! [`Capture`](jstream_platform::Capture) backend.
//!
//! Frames reach the engine on the CPU path (a staging readback converted to NV12) or,
//! on a GPU context, the GPU path, both described below. Targets are named as `id` describes.
//!
//! # Behaviour
//!
//! **`WgcCapture`** (`Capture`), Windows.Graphics.Capture of displays and windows.
//! `WgcCapture::new()` is the CPU path, ARCH's fallback and the default
//! (ARCH › GPU pipeline, step 6).
//! `WgcCapture::on_gpu(&GpuContext)` is the GPU path (steps 1 and 2), described after
//! this list:
//! - **Targets:** the displays, then the top-level windows a user would pick: visible,
//!   titled, not owned by another window, not tool windows, not cloaked, and not this
//!   process's own. A display's `DeviceId` is `monitor:` and its GDI device name
//!   (`monitor:\\.\DISPLAY1`); its name is the device name (`DISPLAY1`), with the current
//!   mode's size. A window's is `hwnd:` and its handle in hex (`hwnd:0x1a2b`); its name is
//!   its title. Windows may renumber displays when the topology changes, and may reuse a
//!   handle once its window is destroyed.
//! - **Device:** each capture creates its own D3D11 device, on the adapter that drives the
//!   display (for a window, the display it is on), else on the default hardware adapter.
//! - **Frames:** a free-threaded frame pool of two BGRA buffers. On each `FrameArrived`,
//!   on a WinRT thread-pool thread, the frame is copied into a staging texture, read back
//!   and converted to NV12 in BT.709 limited range, which is what the H.264 encoder's CPU
//!   path takes, by `jstream_platform::nv12`, the converter Linux's VA-API encoder uses
//!   too. Chroma is left-sited, the H.264 default: each chroma sample filters the
//!   two rows of its block with a `[1 2 1]` tap centred on the block's left column. NV12
//!   needs even dimensions, so an odd width or height loses its last column or row. When
//!   the content size changes, the pool is recreated at the new size, and the frames
//!   after it have that size.
//! - **Mailbox:** frames go to `jstream_platform::mailbox`, the single-slot mailbox every
//!   platform's capture shares: one atomic cell that both sides swap, so `latest` never
//!   waits (R-RT-002). A frame
//!   replaced before `latest` took it counts in `frames_skipped`, and its planes are reused
//!   for the next frame of the same size. Otherwise each frame allocates its planes, on
//!   the capture thread, and the mailbox boxes every frame it is given.
//! - **Times:** `pts` is the frame's `SystemRelativeTime`, which WGC reports as QPC in
//!   100 ns units. That is `QpcClock`'s time line and unit, so it is used as is. The
//!   capture tests check that every `pts` is at most a second before `QpcClock::now` and
//!   at most 100 ms after it. It can be ahead: on a 29 Hz display the GPU path, which
//!   delivers a frame within milliseconds, saw frames stamped about 15 ms after `now`, so
//!   the time is the composition's target rather than when the frame was ready.
//! - **Settings:** the yellow capture border is turned off, and the cursor is captured
//!   unless `start_with` asks for `CursorCapture::Hidden`, which turns
//!   `IsCursorCaptureEnabled` off (`Metadata` draws it in: WGC can't send it apart). Both
//!   are best effort: builds without these settings keep their defaults, so the pointer
//!   stays drawn in there.
//! - **Where the pointer is** (`capture/pointer.rs`, `CaptureStream::pointer`): read from
//!   the OS when asked, since WGC doesn't send it: `GetCursorPos`, and the display's
//!   `GetMonitorInfoW` area or the window's `DWMWA_EXTENDED_FRAME_BOUNDS`, with the
//!   calling thread made per-monitor DPI aware (v2) for those calls only, so all three
//!   are in physical pixels. The point is scaled to the newest frame's size, which the
//!   frame handler records. `None` before the first frame, on a secure desktop, or once
//!   the target has gone. None of the calls waits.
//! - **End of a capture:** when the window closes or the display goes away, `latest`
//!   returns `DeviceLost` from then on. A failure in the frame handler ends the capture
//!   the same way, with its error. A panic there is caught and reported as `InvalidState`
//!   (R-UNSAFE-005); the enumeration callbacks catch
//!   panics too.
//! - **Threads:** each capture keeps the process MTA alive (`CoIncrementMTAUsage`), so it
//!   can be started, read and dropped from threads that never initialised COM. From the
//!   first capture on, one more usage is held until the process exits: the `windows`
//!   crate caches agile WinRT activation factories for the process, and once the last
//!   usage went, COM could unload the DLLs behind them, so a later capture crashed.
//! - **Teardown:** dropping a stream unregisters its handlers, closes the session and
//!   pool, and then releases the handler's devices, textures and ring on the dropping
//!   thread, waiting for a handler that is mid-frame. A frame left in the mailbox is
//!   released too.
//! - **Start:** a target that goes between the lookup and the start of the capture is
//!   `NotFound` (except a reused window handle), as for a target that was never there.
//! - **GPU path** (`on_gpu`, for a context `WindowsGpu` opened; another backend's is
//!   `InvalidState`). The engine reaches it through `Capture::on_context`, which
//!   `WgcCapture` answers with `on_gpu` on either path, so a host builds the CPU backend
//!   and the engine moves capture onto its context (see
//!   Open questions).
//!   - **Device:** every capture uses the context's D3D11 device, so frames are on the
//!     context's adapter. A display driven by another adapter is still captured; WGC
//!     copies across adapters itself.
//!   - **Frames:** each frame is copied on the GPU (`CopySubresourceRegion`) into a slot
//!     of the stream's ring of BGRA textures, allocated by D3D11 with a shared NT handle
//!     and opened on D3D12. It is delivered as `VideoSurface::Gpu`: a `Bgra8UnormSrgb`
//!     `wgpu::Texture` with `TEXTURE_BINDING | COPY_SRC`, the frame's content size (no
//!     even-size rounding), `PixelFormat::Bgra8` in sRGB (BT.709 primaries, full range),
//!     opaque, and `pts` as on the CPU path. That is the format `jstream-render`'s
//!     `Compositor::import` takes.
//!   - **Sync:** each stream has its own shared fence, which D3D11 signals after each copy
//!     (then flushes). `FrameSync::stage` waits on it for the frame's copy and signals the
//!     context's fence when the submit is done. Only one queue signals each fence, so its
//!     values only increase.
//!   - **Ring:** four slots. A slot is free once nothing holds its frame and the context's
//!     fence has passed its last staged use (a frame never staged frees its slot as soon
//!     as it drops). The next frame goes to a free slot of its size, else a new one while
//!     the ring has room. When every slot is held, the frame is dropped and counted in
//!     `frames_skipped`; capture never waits (R-RT-004).
//!     Free slots of an old size are released when the size changes.
//!   - **Driver lifetime:** every slot and fence holds the context's platform state, so
//!     the wgpu device clone outlives their raw objects; the capture keeps the device
//!     alive, not the `GpuContext`.
//! - **Not yet:** the CPU readback is not yet counted in stats or measured against a
//!   budget, as ARCH step 6 asks.

// Platform-independent, so they are tested on every platform.
#[cfg(any(windows, test))]
mod id;
#[cfg(windows)]
mod pointer;
#[cfg(any(windows, test))]
pub(crate) mod ring;
#[cfg(windows)]
mod targets;
#[cfg(windows)]
mod wgc;

#[cfg(windows)]
pub use wgc::WgcCapture;
