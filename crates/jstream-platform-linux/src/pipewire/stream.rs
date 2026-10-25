//! A PipeWire video stream on the portal's remote, putting each frame into the capture
//! mailbox (ADR-0060 §3), and, when the portal session sends it, the pointer from each
//! buffer's cursor metadata into a second mailbox, on either path.
//!
//! On the CPU path the stream takes shared-memory buffers and copies each frame out.
//! On the GPU path (a stream connected with the context's [`gpu::Shared`]) it first
//! offers DMA-BUFs in the modifiers the device imports, then shared memory: a DMA-BUF
//! frame is imported once per PipeWire buffer ([`dmabuf::import`]), waited on until the
//! producer's writes are done (`poll` on the DMA-BUF), copied into a frame texture
//! ([`blit::Ring`]), and its buffer goes back to PipeWire once the GPU has finished
//! that copy. A frame that doesn't import or copy ends the GPU path for the stream: it
//! offers shared memory only, and PipeWire renegotiates to the CPU path.
//!
//! The stream runs on a `pw_thread_loop`, PipeWire's own thread, and its callbacks
//! (format changes, buffers, frames, state changes) run there. They catch panics
//! (R-UNSAFE-005) and only touch the stream's [`Callbacks`] and the shared state.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jstream_platform::mailbox::Mailbox;
use jstream_platform::{CaptureCursor, CursorImage, MediaClock, PlatformError, VideoSurface};

use super::ffi::{
    CHUNK_SKIP, FLAG_AUTOCONNECT, FLAG_MAP_BUFFERS, Fns, PW_DIRECTION_INPUT,
    PW_VERSION_STREAM_EVENTS, Ptr, PwBuffer, PwStreamEvents, STATE_ERROR, STATE_STREAMING,
    STATE_UNCONNECTED, SpaBuffer, SpaHook, SpaMeta, SpaPod,
};
use crate::MonotonicClock;
use crate::dl::SystemLibrary;
use crate::gpu::blit::{self, Ring};
use crate::gpu::{self, dmabuf};
use crate::spa::{
    self, Chunk, Crop, CursorBitmap, CursorMeta, DATA_DMA_BUF, META_CURSOR, META_HEADER,
    META_HEADER_SIZE, META_REGION_SIZE, META_VIDEO_CROP, PARAM_FORMAT, Pod, VideoFormat,
};

/// `struct spa_meta_header` in <spa/buffer/meta.h>.
#[repr(C)]
struct SpaMetaHeader {
    flags: u32,
    offset: u32,
    pts: i64,
    dts_offset: i64,
    seq: u64,
}

/// `struct spa_meta_region` in <spa/buffer/meta.h>.
#[repr(C)]
struct SpaMetaRegion {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// `PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS`.
const STREAM_FLAGS: u32 = FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS;
/// `SPA_META_HEADER_FLAG_CORRUPTED`.
const HEADER_CORRUPTED: u32 = 1 << 1;

/// What the stream's callbacks share with the [`CaptureStream`](jstream_platform::CaptureStream)
/// that reads it, and with the portal watcher.
pub(crate) struct Shared {
    /// The newest frame.
    pub(crate) mailbox: Mailbox<VideoSurface>,
    /// The pointer, when it changed since it was last taken; only with cursor metadata.
    pub(crate) cursor: Mailbox<CaptureCursor>,
    pub(crate) captured: AtomicU64,
    pub(crate) skipped: AtomicU64,
    lost: AtomicBool,
    reason: Mutex<String>,
    /// Why the GPU path ended, once a DMA-BUF frame failed and the stream fell back to
    /// shared memory.
    pub(crate) gpu_fallback: Mutex<Option<String>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("captured", &self.captured)
            .field("lost", &self.lost)
            .field("gpu_fallback", &self.gpu_fallback)
            .finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            mailbox: Mailbox::new(),
            cursor: Mailbox::new(),
            captured: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            lost: AtomicBool::new(false),
            reason: Mutex::new(String::new()),
            gpu_fallback: Mutex::new(None),
        }
    }

    /// Marks the capture lost, keeping the first reason.
    pub(crate) fn lose(&self, reason: &str) {
        if let Ok(mut r) = self.reason.lock()
            && r.is_empty()
        {
            reason.clone_into(&mut r);
        }
        self.lost.store(true, Ordering::Release);
    }

    /// Why the capture was lost, once it was. Never waits: a reason being written is
    /// reported as unknown.
    pub(crate) fn lost(&self) -> Option<String> {
        if !self.lost.load(Ordering::Acquire) {
            return None;
        }
        Some(
            self.reason
                .try_lock()
                .map(|r| r.clone())
                .unwrap_or_else(|_| "the capture stopped".into()),
        )
    }
}

/// The GPU path's state: the context, the frame ring, and the imported DMA-BUFs, one
/// per PipeWire buffer (keyed by its `pw_buffer` address, removed in `remove_buffer`).
struct DmaPath {
    shared: Arc<gpu::Shared>,
    ring: Ring,
    imports: HashMap<usize, wgpu::Texture>,
}

/// How long a frame waits for the producer's writes to its DMA-BUF before it is
/// dropped.
const PRODUCER_WAIT: Duration = Duration::from_millis(100);
/// How long a frame waits for its GPU copy before the GPU path is given up.
const COPY_WAIT: Duration = Duration::from_secs(1);

/// The callbacks' state, owned by [`VideoStream`] and used only on PipeWire's thread
/// (and on the creating thread under the loop lock, before the stream connects).
struct Callbacks {
    fns: Fns,
    stream: Ptr,
    format: Option<VideoFormat>,
    clock: MonotonicClock,
    shared: Arc<Shared>,
    /// Kept so the pods' pointers handed to `pw_stream_update_params` stay valid.
    params: Vec<Pod>,
    /// The `EnumFormat` offers, kept for the same reason.
    offers: Vec<Pod>,
    /// The context, while the stream may still take DMA-BUFs.
    gpu: Option<Arc<gpu::Shared>>,
    /// Set up once PipeWire settles on a DMA-BUF format.
    dma: Option<DmaPath>,
    streaming: bool,
    /// The pointer so far, when the stream asks for cursor metadata.
    cursor: Option<CursorTrack>,
}

/// The pointer as the buffers so far give it: the last picture (a buffer only carries
/// one when it changes) and the state last put in the mailbox.
#[derive(Debug, Default)]
struct CursorTrack {
    image: Option<Arc<CursorImage>>,
    last: CaptureCursor,
}

impl CursorTrack {
    /// The pointer after a buffer's `meta`, in the frame's pixels (the buffer's less the
    /// crop's origin), or `None` when it is as it was.
    fn update(&mut self, meta: CursorMeta, crop: Option<Crop>) -> Option<CaptureCursor> {
        let now = match meta {
            CursorMeta::Hidden => CaptureCursor::Hidden,
            CursorMeta::At { x, y, bitmap } => {
                match bitmap {
                    CursorBitmap::Unchanged => {}
                    CursorBitmap::Empty => self.image = None,
                    CursorBitmap::Image(image) => self.image = Some(Arc::new(image)),
                }
                let (dx, dy) = match crop {
                    Some(c) if c.width > 0 && c.height > 0 => (c.x.max(0), c.y.max(0)),
                    _ => (0, 0),
                };
                match &self.image {
                    Some(image) => CaptureCursor::Shown {
                        x: x.saturating_sub(dx),
                        y: y.saturating_sub(dy),
                        image: Arc::clone(image),
                    },
                    None => CaptureCursor::Hidden,
                }
            }
        };
        let same = match (&now, &self.last) {
            (
                CaptureCursor::Shown { x, y, image },
                CaptureCursor::Shown {
                    x: lx,
                    y: ly,
                    image: li,
                },
            ) => x == lx && y == ly && Arc::ptr_eq(image, li),
            (now, last) => now == last,
        };
        if same {
            return None;
        }
        self.last = now.clone();
        Some(now)
    }
}

/// The header's time and the crop, from a buffer's metadata; `None` for a frame the
/// producer marked corrupted.
///
/// # Safety
/// `buf` is a dequeued buffer's `spa_buffer`, not yet queued back.
unsafe fn metas(buf: &SpaBuffer) -> Option<(Option<i64>, Option<Crop>)> {
    let metas: &[SpaMeta] = if buf.metas.is_null() {
        &[]
    } else {
        // SAFETY: `metas` holds `n_metas` entries.
        unsafe { std::slice::from_raw_parts(buf.metas, buf.n_metas as usize) }
    };
    let meta = |ty: u32, size: u32| {
        metas
            .iter()
            .find(|m| m.type_ == ty && m.size >= size && !m.data.is_null())
            .map(|m| m.data)
    };
    let mut header_pts = None;
    if let Some(h) = meta(META_HEADER, META_HEADER_SIZE) {
        // SAFETY: a header meta of at least its struct's size; read unaligned to be
        // safe about the producer's layout.
        let h = unsafe { h.cast::<SpaMetaHeader>().read_unaligned() };
        if h.flags & HEADER_CORRUPTED != 0 {
            return None;
        }
        header_pts = Some(h.pts);
    }
    let crop = meta(META_VIDEO_CROP, META_REGION_SIZE).map(|r| {
        // SAFETY: a region meta of at least its struct's size.
        let r = unsafe { r.cast::<SpaMetaRegion>().read_unaligned() };
        Crop {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }
    });
    Some((header_pts, crop))
}

/// Whether the DMA-BUF's pending writes finished within `timeout`: a DMA-BUF polls
/// readable once its write fences have signalled, which is how a consumer honours the
/// producer's implicit synchronisation. A `poll` error counts as ready, since the
/// kernel then has no fences to report.
fn writes_done(fd: BorrowedFd<'_>, timeout: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = c_int::try_from(timeout.as_millis()).unwrap_or(c_int::MAX);
    // SAFETY: one valid pollfd for the call's duration; the descriptor stays open
    // (borrowed).
    let n = unsafe { libc::poll(&raw mut pfd, 1, ms) };
    n < 0 || (n > 0 && pfd.revents & libc::POLLIN != 0)
}

impl Callbacks {
    fn param_changed(&mut self, id: u32, param: *const SpaPod) {
        if id != PARAM_FORMAT || param.is_null() {
            return;
        }
        // SAFETY: PipeWire passes a valid pod for the call's duration: a header, then
        // `size` body bytes.
        let pod = unsafe {
            let size = (*param).size as usize;
            std::slice::from_raw_parts(param.cast::<u8>(), 8 + size)
        };
        self.format = spa::parse_format(pod);
        let Some(format) = self.format else {
            self.shared
                .lose("PipeWire settled on a video format JStream didn't offer");
            return;
        };
        let dma = format.modifier.is_some();
        if dma {
            let Some(shared) = self.gpu.clone() else {
                self.shared
                    .lose("PipeWire settled on DMA-BUF buffers JStream didn't offer");
                return;
            };
            match &mut self.dma {
                Some(path) => path.imports.clear(),
                None => {
                    self.dma = Some(DmaPath {
                        ring: Ring::new(Arc::clone(&shared)),
                        shared,
                        imports: HashMap::new(),
                    });
                }
            }
        } else {
            self.dma = None;
        }
        self.params = spa::buffer_params(dma).into();
        if self.cursor.is_some() {
            self.params.push(spa::cursor_param());
        }
        let mut ptrs: Vec<*const c_void> = self.params.iter().map(Pod::as_ptr).collect();
        // SAFETY: `pw_stream_update_params` (see `Fns`), on this callback's own stream,
        // from its thread; `ptrs` points at `params.len()` pods kept alive in `self`.
        unsafe {
            (self.fns.stream_update_params)(self.stream, ptrs.as_mut_ptr(), ptrs.len() as u32);
        }
    }

    fn state_changed(&mut self, state: c_int, error: *const c_char) {
        let reason = if error.is_null() {
            String::new()
        } else {
            // SAFETY: a NUL-terminated string valid for the call, when non-null.
            unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned()
        };
        match state {
            STATE_STREAMING => self.streaming = true,
            STATE_ERROR => self
                .shared
                .lose(&format!("PipeWire stream error: {reason}")),
            STATE_UNCONNECTED if self.streaming => {
                self.shared.lose("the PipeWire stream disconnected");
            }
            _ => {}
        }
    }

    fn process(&mut self) {
        // SAFETY: `pw_stream_dequeue_buffer` on this callback's stream, on its thread.
        let b = unsafe { (self.fns.stream_dequeue_buffer)(self.stream) };
        if b.is_null() {
            return;
        }
        // SAFETY: a dequeued buffer, queued back below.
        unsafe { self.note_cursor(&*b) };
        let mut failed = None;
        let frame = if self.format.is_some_and(|f| f.modifier.is_some()) {
            // SAFETY: as below. The GPU copy is finished before the buffer goes back.
            match unsafe { self.read_dma_buf(&*b, b as usize) } {
                Ok(frame) => frame,
                Err(reason) => {
                    failed = Some(reason);
                    None
                }
            }
        } else {
            // SAFETY: a dequeued `pw_buffer` and the `spa_buffer`, metas, datas and
            // chunk it points to stay valid until it is queued back, below; each
            // pointer is checked for null and each count bounds the arrays it
            // describes.
            unsafe { self.read(&*b) }
        };
        // SAFETY: gives the buffer dequeued above back to its stream.
        unsafe { (self.fns.stream_queue_buffer)(self.stream, b) };
        if let Some(frame) = frame {
            self.shared.captured.fetch_add(1, Ordering::Relaxed);
            if self.shared.mailbox.put(frame).is_some() {
                self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some(reason) = failed {
            self.fall_back(reason);
        }
    }

    /// Ends the GPU path: offers shared memory only, so PipeWire renegotiates the
    /// stream onto the CPU copy. Keeps the first reason.
    fn fall_back(&mut self, reason: String) {
        if let Ok(mut r) = self.shared.gpu_fallback.lock()
            && r.is_none()
        {
            *r = Some(reason);
        }
        self.gpu = None;
        self.dma = None;
        self.offers = vec![spa::enum_format()];
        let mut ptrs: Vec<*const c_void> = self.offers.iter().map(Pod::as_ptr).collect();
        // SAFETY: `pw_stream_update_params` (see `Fns`), on this callback's own stream,
        // from its thread; `ptrs` points at `offers.len()` pods kept alive in `self`.
        unsafe {
            (self.fns.stream_update_params)(self.stream, ptrs.as_mut_ptr(), ptrs.len() as u32);
        }
    }

    fn remove_buffer(&mut self, b: *mut PwBuffer) {
        if let Some(path) = &mut self.dma {
            path.imports.remove(&(b as usize));
        }
    }

    /// The DMA-BUF frame in `b` (whose address is `key`), copied into a frame texture.
    /// `Ok(None)` for a frame to skip; an error ends the GPU path.
    ///
    /// # Safety
    /// `b` is a dequeued buffer of this stream, not yet queued back.
    unsafe fn read_dma_buf(
        &mut self,
        b: &PwBuffer,
        key: usize,
    ) -> Result<Option<VideoSurface>, String> {
        let (Some(format), Some(path)) = (self.format, self.dma.as_mut()) else {
            return Ok(None);
        };
        let Some(modifier) = format.modifier else {
            return Ok(None);
        };
        if b.buffer.is_null() {
            return Ok(None);
        }
        // SAFETY: the caller's contract; non-null.
        let buf = unsafe { &*b.buffer };
        if buf.n_datas == 0 || buf.datas.is_null() {
            return Ok(None);
        }
        if buf.n_datas != 1 {
            return Err(format!(
                "a DMA-BUF frame of {} planes; only one was offered",
                buf.n_datas
            ));
        }
        // SAFETY: `datas` holds `n_datas` (one) entries.
        let d = unsafe { &*buf.datas };
        if d.type_ != DATA_DMA_BUF {
            return Err(format!(
                "a buffer of data type {} on a DMA-BUF format",
                d.type_
            ));
        }
        if d.chunk.is_null() {
            return Ok(None);
        }
        // SAFETY: non-null, owned by the buffer.
        let c = unsafe { &*d.chunk };
        if c.flags & CHUNK_SKIP != 0 {
            return Ok(None);
        }
        // SAFETY: the caller's contract.
        let Some((header_pts, crop)) = (unsafe { metas(buf) }) else {
            return Ok(None);
        };
        let raw = i32::try_from(d.fd)
            .ok()
            .filter(|fd| *fd >= 0)
            .ok_or("a DMA-BUF buffer without a file descriptor")?;
        let stride = u32::try_from(c.stride)
            .ok()
            .filter(|s| *s > 0)
            .ok_or("a DMA-BUF plane without a stride")?;
        // SAFETY: PipeWire keeps the buffer's descriptor open until the buffer is
        // removed, which can't happen while it is dequeued; it is borrowed for this
        // call only (the import duplicates it).
        let fd = unsafe { BorrowedFd::borrow_raw(raw) };
        let DmaPath {
            shared,
            ring,
            imports,
        } = path;
        let texture = match imports.entry(key) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(v) => v.insert(
                dmabuf::import(
                    &shared.device,
                    format.format,
                    (format.width, format.height),
                    modifier,
                    dmabuf::Plane {
                        fd,
                        offset: c.offset,
                        stride,
                    },
                )
                .map_err(|e| format!("importing a DMA-BUF: {e}"))?,
            ),
        };
        if !writes_done(fd, PRODUCER_WAIT) {
            // The producer's writes didn't finish in time: drop the frame, counted, as
            // a full ring does below.
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }
        let pts = spa::frame_pts(header_pts, self.clock.now(), self.clock.timebase());
        let source = blit::Source {
            texture,
            visible: spa::visible(format, crop),
            padded: format.format.padded(),
        };
        let Some((frame, index)) = ring
            .copy(source, pts)
            .map_err(|e| format!("copying a DMA-BUF frame: {e}"))?
        else {
            self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        };
        shared
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: Some(COPY_WAIT),
            })
            .map_err(|e| format!("the GPU copy of a DMA-BUF frame didn't finish: {e}"))?;
        Ok(Some(VideoSurface::Gpu(frame)))
    }

    /// Notes the pointer in `b`'s cursor metadata, when the stream asks for it, on
    /// either path and before the frame is read: a buffer can carry a pointer move and
    /// no frame, or a frame the producer marked corrupted.
    ///
    /// # Safety
    /// `b` is a dequeued buffer of this stream, not yet queued back.
    unsafe fn note_cursor(&mut self, b: &PwBuffer) {
        let Some(track) = &mut self.cursor else {
            return;
        };
        if b.buffer.is_null() {
            return;
        }
        // SAFETY: the caller's contract; non-null.
        let buf = unsafe { &*b.buffer };
        let metas: &[SpaMeta] = if buf.metas.is_null() {
            &[]
        } else {
            // SAFETY: `metas` holds `n_metas` entries.
            unsafe { std::slice::from_raw_parts(buf.metas, buf.n_metas as usize) }
        };
        let meta_entry = |ty: u32, size: u32| {
            metas
                .iter()
                .find(|m| m.type_ == ty && m.size >= size && !m.data.is_null())
        };
        let Some(m) = meta_entry(META_CURSOR, 1) else {
            return;
        };
        let crop = meta_entry(META_VIDEO_CROP, META_REGION_SIZE).map(|r| {
            // SAFETY: a region meta of at least its struct's size.
            let r = unsafe { r.data.cast::<SpaMetaRegion>().read_unaligned() };
            Crop {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
            }
        });
        // SAFETY: a meta's `data` holds `size` bytes for the buffer's lifetime;
        // `parse_cursor` checks every offset in them.
        let bytes = unsafe { std::slice::from_raw_parts(m.data.cast::<u8>(), m.size as usize) };
        if let Some(update) = spa::parse_cursor(bytes).and_then(|c| track.update(c, crop)) {
            self.shared.cursor.put(update);
        }
    }

    /// The frame in `b`, copied out.
    ///
    /// # Safety
    /// `b` is a dequeued buffer of this stream, not yet queued back.
    unsafe fn read(&self, b: &PwBuffer) -> Option<VideoSurface> {
        let format = self.format?;
        if b.buffer.is_null() {
            return None;
        }
        // SAFETY: the caller's contract; non-null.
        let buf = unsafe { &*b.buffer };
        if buf.n_datas == 0 || buf.datas.is_null() {
            return None;
        }
        // SAFETY: `datas` holds `n_datas` (at least one) entries.
        let d = unsafe { &*buf.datas };
        if d.data.is_null() || d.chunk.is_null() {
            return None;
        }
        // SAFETY: non-null, owned by the buffer.
        let c = unsafe { &*d.chunk };
        if c.flags & CHUNK_SKIP != 0 {
            return None;
        }
        // SAFETY: MAP_BUFFERS mapped `maxsize` bytes at `data` for the buffer's
        // lifetime.
        let data = unsafe { std::slice::from_raw_parts(d.data.cast::<u8>(), d.maxsize as usize) };
        // SAFETY: the caller's contract.
        let (header_pts, crop) = unsafe { metas(buf) }?;
        let chunk = Chunk {
            offset: c.offset % d.maxsize.max(1),
            size: c.size,
            stride: c.stride,
        };
        let pts = spa::frame_pts(header_pts, self.clock.now(), self.clock.timebase());
        spa::copy_frame(data, chunk, format, crop, pts)
            .ok()
            .map(VideoSurface::Cpu)
    }
}

/// Runs `f` on the callbacks behind `data`, catching a panic (R-UNSAFE-005), which
/// marks the capture lost.
///
/// # Safety
/// `data` is the `Callbacks` pointer registered with the listener, and PipeWire calls
/// its listeners on one thread, so this is the only reference.
unsafe fn with(data: *mut c_void, f: impl FnOnce(&mut Callbacks)) {
    // SAFETY: the function's contract.
    let cb = unsafe { &mut *data.cast::<Callbacks>() };
    let shared = Arc::clone(&cb.shared);
    if catch_unwind(AssertUnwindSafe(|| f(cb))).is_err() {
        shared.lose("the capture callback panicked");
    }
}

unsafe extern "C" fn on_state_changed(
    data: *mut c_void,
    _old: c_int,
    state: c_int,
    error: *const c_char,
) {
    // SAFETY: registered with this `data` (see `with`).
    unsafe { with(data, |cb| cb.state_changed(state, error)) };
}

unsafe extern "C" fn on_param_changed(data: *mut c_void, id: u32, param: *const SpaPod) {
    // SAFETY: registered with this `data` (see `with`).
    unsafe { with(data, |cb| cb.param_changed(id, param)) };
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    // SAFETY: registered with this `data` (see `with`).
    unsafe { with(data, Callbacks::process) };
}

unsafe extern "C" fn on_remove_buffer(data: *mut c_void, b: *mut PwBuffer) {
    // SAFETY: registered with this `data` (see `with`).
    unsafe { with(data, |cb| cb.remove_buffer(b)) };
}

static EVENTS: PwStreamEvents = PwStreamEvents {
    version: PW_VERSION_STREAM_EVENTS,
    destroy: None,
    state_changed: Some(on_state_changed),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param_changed),
    add_buffer: None,
    remove_buffer: Some(on_remove_buffer),
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

/// A connected PipeWire video stream. Dropping it disconnects and frees everything,
/// PipeWire's thread first.
pub(crate) struct VideoStream {
    fns: Fns,
    thread_loop: Ptr,
    context: Ptr,
    core: Ptr,
    stream: Ptr,
    /// Linked into the stream's listener list until the stream is destroyed.
    hook: Box<SpaHook>,
    callbacks: Box<Callbacks>,
    _lib: SystemLibrary,
}

// SAFETY: the raw pointers are PipeWire objects this value owns. After `connect`
// returns they are touched only by `Drop`, which takes the thread loop's lock before
// the stream and stops the loop before the rest, as libpipewire requires of any
// thread; the callbacks run only on PipeWire's own thread.
unsafe impl Send for VideoStream {}

impl std::fmt::Debug for VideoStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoStream").finish_non_exhaustive()
    }
}

fn pw_failed(call: &'static str) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: std::io::Error::last_os_error().to_string(),
    }
}

impl VideoStream {
    /// Connects to node `node_id` on the PipeWire remote `fd` (from the portal's
    /// `OpenPipeWireRemote`), delivering frames into `shared`: on `gpu`'s device when it
    /// is given (the GPU path, falling back to CPU frames), else as CPU frames; and the
    /// pointer too when `cursor_metadata` (the portal session sends it as metadata).
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when libpipewire lacks a function, and
    /// [`PlatformError::Os`] when PipeWire refuses a step.
    pub(crate) fn connect(
        lib: &SystemLibrary,
        fd: OwnedFd,
        node_id: u32,
        shared: Arc<Shared>,
        gpu: Option<Arc<gpu::Shared>>,
        cursor_metadata: bool,
    ) -> Result<Self, PlatformError> {
        let fns = Fns::load(lib)?;
        let clock = MonotonicClock::new()?;
        // SAFETY: `pw_init` with no arguments may be called more than once (it keeps a
        // reference count) and from any thread.
        unsafe { (fns.init)(ptr::null_mut(), ptr::null_mut()) };
        let name = c"jstream-capture";
        // SAFETY: `pw_thread_loop_new` with a static name and no properties.
        let thread_loop = unsafe { (fns.thread_loop_new)(name.as_ptr(), ptr::null()) };
        if thread_loop.is_null() {
            return Err(pw_failed("pw_thread_loop_new"));
        }
        let mut this = Self {
            fns,
            thread_loop,
            context: ptr::null_mut(),
            core: ptr::null_mut(),
            stream: ptr::null_mut(),
            hook: SpaHook::boxed(),
            callbacks: Box::new(Callbacks {
                fns,
                stream: ptr::null_mut(),
                format: None,
                clock,
                shared,
                params: Vec::new(),
                offers: spa::enum_formats(gpu.as_ref().map_or(&[], |g| &g.modifiers)),
                gpu,
                dma: None,
                streaming: false,
                cursor: cursor_metadata.then(CursorTrack::default),
            }),
            _lib: lib.clone(),
        };
        // SAFETY: `pw_context_new` on the thread loop's loop, before it runs; no
        // properties, no user data.
        this.context = unsafe {
            let pw_loop = (fns.thread_loop_get_loop)(thread_loop);
            (fns.context_new)(pw_loop, ptr::null_mut(), 0)
        };
        if this.context.is_null() {
            return Err(pw_failed("pw_context_new"));
        }
        // SAFETY: starts PipeWire's thread for this loop.
        if unsafe { (fns.thread_loop_start)(thread_loop) } < 0 {
            return Err(pw_failed("pw_thread_loop_start"));
        }
        // SAFETY: from here to the unlock, the loop's lock is held, as every call on
        // its objects from another thread requires.
        unsafe { (fns.thread_loop_lock)(thread_loop) };
        let connected = this.connect_locked(fd, node_id);
        // SAFETY: releases the lock taken above.
        unsafe { (fns.thread_loop_unlock)(thread_loop) };
        connected.map(|()| this)
    }

    /// The steps that need the loop's lock.
    fn connect_locked(&mut self, fd: OwnedFd, node_id: u32) -> Result<(), PlatformError> {
        let fns = self.fns;
        // SAFETY: `pw_context_connect_fd` takes ownership of the socket (it closes it on
        // disconnect or failure), so it is released from `OwnedFd` here.
        self.core =
            unsafe { (fns.context_connect_fd)(self.context, fd.into_raw_fd(), ptr::null_mut(), 0) };
        if self.core.is_null() {
            return Err(pw_failed("pw_context_connect_fd"));
        }
        let props = CString::new("media.type=Video media.category=Capture media.role=Screen")
            .map_err(|_| pw_failed("pw_properties_new_string"))?;
        // SAFETY: `pw_properties_new_string` parses a NUL-terminated string; the stream
        // takes ownership of the result (null is allowed and means none).
        let props = unsafe { (fns.properties_new_string)(props.as_ptr()) };
        let name = c"JStream screen capture";
        // SAFETY: `pw_stream_new` on our core, with a static name; it takes `props`.
        self.stream = unsafe { (fns.stream_new)(self.core, name.as_ptr(), props) };
        if self.stream.is_null() {
            return Err(pw_failed("pw_stream_new"));
        }
        self.callbacks.stream = self.stream;
        let data: *mut Callbacks = &raw mut *self.callbacks;
        // SAFETY: `pw_stream_add_listener` links `hook` (zeroed, boxed so it doesn't
        // move) and calls `EVENTS` (static) with `data` (boxed, outlives the stream,
        // which `Drop` destroys first).
        unsafe {
            (fns.stream_add_listener)(
                self.stream,
                &raw mut *self.hook,
                &raw const EVENTS,
                data.cast(),
            )
        };
        let mut params: Vec<*const c_void> =
            self.callbacks.offers.iter().map(Pod::as_ptr).collect();
        // SAFETY: `pw_stream_connect` with the offers, pods that outlive the call (the
        // stream copies its params, and `callbacks` keeps them).
        let rc = unsafe {
            (fns.stream_connect)(
                self.stream,
                PW_DIRECTION_INPUT,
                node_id,
                STREAM_FLAGS,
                params.as_mut_ptr(),
                params.len() as u32,
            )
        };
        if rc < 0 {
            return Err(PlatformError::Os {
                call: "pw_stream_connect",
                code: u32::try_from(-rc).ok(),
                message: std::io::Error::from_raw_os_error(-rc).to_string(),
            });
        }
        Ok(())
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        let fns = self.fns;
        // SAFETY: each object is destroyed once, in libpipewire's order: the stream
        // under the loop lock (which removes its listeners, so the callbacks are never
        // called again), then the loop's thread is stopped, then the core, the context
        // and the loop. Null pointers are the steps `connect` never reached.
        unsafe {
            if !self.stream.is_null() {
                (fns.thread_loop_lock)(self.thread_loop);
                (fns.stream_disconnect)(self.stream);
                (fns.stream_destroy)(self.stream);
                (fns.thread_loop_unlock)(self.thread_loop);
            }
            (fns.thread_loop_stop)(self.thread_loop);
            if !self.core.is_null() {
                (fns.core_disconnect)(self.core);
            }
            if !self.context.is_null() {
                (fns.context_destroy)(self.context);
            }
            (fns.thread_loop_destroy)(self.thread_loop);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::{Shared, SpaMetaHeader, SpaMetaRegion};

    /// The video-only layouts match libspa's on 64-bit Linux; the shared ones are
    /// checked in `ffi`.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn layouts_match_the_c_headers() {
        assert_eq!(size_of::<SpaMetaHeader>(), 32);
        assert_eq!(size_of::<SpaMetaRegion>(), 16);
    }

    /// `writes_done` waits for the descriptor to poll readable, as a DMA-BUF does once
    /// its writers' fences signal; a pipe stands in for one here.
    #[test]
    fn writes_done_waits_for_a_readable_descriptor() {
        use std::io::Write;
        use std::os::fd::AsFd;
        use std::time::{Duration, Instant};

        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let started = Instant::now();
        assert!(!super::writes_done(
            reader.as_fd(),
            Duration::from_millis(20)
        ));
        assert!(started.elapsed() >= Duration::from_millis(20), "it waited");
        writer.write_all(b"x").expect("write");
        assert!(super::writes_done(reader.as_fd(), Duration::ZERO));
    }

    #[test]
    fn the_first_loss_reason_is_kept() {
        let s = Shared::new();
        assert_eq!(s.lost(), None);
        s.lose("first");
        s.lose("second");
        assert_eq!(s.lost().as_deref(), Some("first"));
    }
}

#[cfg(test)]
mod cursor_tests {
    use std::sync::Arc;

    use jstream_platform::media::FrameSize;
    use jstream_platform::{CaptureCursor, CursorImage};

    use super::CursorTrack;
    use crate::spa::{Crop, CursorBitmap, CursorMeta};

    fn image(red: u8) -> CursorImage {
        CursorImage::new(FrameSize::new(1, 1).expect("size"), vec![red, 0, 0, 255]).expect("image")
    }

    fn at(x: i32, y: i32, bitmap: CursorBitmap) -> CursorMeta {
        CursorMeta::At { x, y, bitmap }
    }

    #[test]
    fn the_pointer_keeps_its_picture_and_reports_only_changes() {
        let mut track = CursorTrack::default();
        // A position before any picture is an invisible pointer.
        assert_eq!(
            track.update(at(5, 5, CursorBitmap::Unchanged), None),
            Some(CaptureCursor::Hidden)
        );
        let Some(CaptureCursor::Shown { x, y, image: first }) =
            track.update(at(5, 6, CursorBitmap::Image(image(1))), None)
        else {
            panic!("shown");
        };
        assert_eq!((x, y), (5, 6));
        // The same place and picture is no change, so nothing is put in the mailbox.
        assert_eq!(track.update(at(5, 6, CursorBitmap::Unchanged), None), None);
        // A move keeps the picture, shared rather than copied.
        let Some(CaptureCursor::Shown { image: moved, .. }) =
            track.update(at(9, 6, CursorBitmap::Unchanged), None)
        else {
            panic!("shown");
        };
        assert!(Arc::ptr_eq(&first, &moved));
        // An empty picture hides it, and so does a pointer that leaves the stream.
        assert_eq!(
            track.update(at(9, 6, CursorBitmap::Empty), None),
            Some(CaptureCursor::Hidden)
        );
        assert!(
            track
                .update(at(9, 6, CursorBitmap::Image(image(2))), None)
                .is_some()
        );
        assert_eq!(
            track.update(CursorMeta::Hidden, None),
            Some(CaptureCursor::Hidden)
        );
        assert_eq!(track.update(CursorMeta::Hidden, None), None);
    }

    #[test]
    fn the_pointer_is_placed_in_the_cropped_frame() {
        let mut track = CursorTrack::default();
        let crop = Crop {
            x: 100,
            y: 40,
            width: 640,
            height: 480,
        };
        let Some(CaptureCursor::Shown { x, y, .. }) =
            track.update(at(150, 50, CursorBitmap::Image(image(1))), Some(crop))
        else {
            panic!("shown");
        };
        assert_eq!((x, y), (50, 10));
        // An empty crop is ignored, as the frame copy ignores it.
        let empty = Crop { width: 0, ..crop };
        let Some(CaptureCursor::Shown { x, y, .. }) =
            track.update(at(150, 50, CursorBitmap::Unchanged), Some(empty))
        else {
            panic!("shown");
        };
        assert_eq!((x, y), (150, 50));
    }
}
