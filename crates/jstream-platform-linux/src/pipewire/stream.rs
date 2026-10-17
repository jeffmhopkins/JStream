//! A PipeWire video stream on the portal's remote, copying each frame into the capture
//! mailbox (ADR-0060 §3), and, when the portal session sends it, the pointer from each
//! buffer's cursor metadata into a second mailbox. Shared-memory buffers only: the
//! DMA-BUF import into Vulkan is a follow-up.
//!
//! The stream runs on a `pw_thread_loop`, PipeWire's own thread, and its callbacks
//! (format changes, frames, state changes) run there. They catch panics
//! (R-UNSAFE-005) and only touch the stream's [`Callbacks`] and the shared state.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use jstream_platform::mailbox::Mailbox;
use jstream_platform::{CaptureCursor, CursorImage, MediaClock, PlatformError, VideoSurface};

use crate::MonotonicClock;
use crate::dl::SystemLibrary;
use crate::spa::{
    self, Chunk, Crop, CursorBitmap, CursorMeta, META_CURSOR, META_HEADER, META_HEADER_SIZE,
    META_REGION_SIZE, META_VIDEO_CROP, PARAM_FORMAT, Pod, VideoFormat,
};

/// `struct spa_list` in <spa/utils/list.h>.
#[repr(C)]
struct SpaList {
    next: *mut SpaList,
    prev: *mut SpaList,
}

/// `struct spa_hook` in <spa/utils/hook.h>: a listener's storage, which PipeWire links
/// into the stream's listener list. Zeroed before `pw_stream_add_listener` fills it.
#[repr(C)]
struct SpaHook {
    link: SpaList,
    funcs: *const c_void,
    data: *mut c_void,
    removed: Option<unsafe extern "C" fn(*mut SpaHook)>,
    priv_: *mut c_void,
}

/// `struct pw_buffer` in <pipewire/stream.h> (only its first field is read).
#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
}

/// `struct spa_buffer` in <spa/buffer/buffer.h>.
#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut SpaMeta,
    datas: *mut SpaData,
}

/// `struct spa_meta` in <spa/buffer/meta.h>.
#[repr(C)]
struct SpaMeta {
    type_: u32,
    size: u32,
    data: *mut c_void,
}

/// `struct spa_data` in <spa/buffer/buffer.h>.
#[repr(C)]
struct SpaData {
    type_: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut c_void,
    chunk: *mut SpaChunk,
}

/// `struct spa_chunk` in <spa/buffer/buffer.h>.
#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

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

/// `struct spa_pod` in <spa/pod/pod.h>.
#[repr(C)]
struct SpaPod {
    size: u32,
    type_: u32,
}

/// `struct pw_stream_events` in <pipewire/stream.h>, version 2.
#[repr(C)]
struct PwStreamEvents {
    version: u32,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    control_info: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const SpaPod)>,
    add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    process: Option<unsafe extern "C" fn(*mut c_void)>,
    drained: Option<unsafe extern "C" fn(*mut c_void)>,
    command: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
    trigger_done: Option<unsafe extern "C" fn(*mut c_void)>,
}

const PW_VERSION_STREAM_EVENTS: u32 = 2;
/// `PW_DIRECTION_INPUT` (`SPA_DIRECTION_INPUT`).
const PW_DIRECTION_INPUT: c_int = 0;
/// `PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS`.
const STREAM_FLAGS: u32 = (1 << 0) | (1 << 2);
/// `PW_STREAM_STATE_ERROR`.
const STATE_ERROR: c_int = -1;
/// `PW_STREAM_STATE_UNCONNECTED`.
const STATE_UNCONNECTED: c_int = 0;
/// `PW_STREAM_STATE_STREAMING`.
const STATE_STREAMING: c_int = 3;
/// `SPA_CHUNK_FLAG_CORRUPTED | SPA_CHUNK_FLAG_EMPTY`.
const CHUNK_SKIP: i32 = (1 << 0) | (1 << 1);
/// `SPA_META_HEADER_FLAG_CORRUPTED`.
const HEADER_CORRUPTED: u32 = 1 << 1;

type Ptr = *mut c_void;

/// The libpipewire functions the stream calls, with their C declarations.
#[derive(Clone, Copy)]
struct Fns {
    /// `void pw_init(int *argc, char **argv[])`
    init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    /// `struct pw_thread_loop *pw_thread_loop_new(const char *name, const struct spa_dict *props)`
    thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> Ptr,
    /// `struct pw_loop *pw_thread_loop_get_loop(struct pw_thread_loop *loop)`
    thread_loop_get_loop: unsafe extern "C" fn(Ptr) -> Ptr,
    /// `int pw_thread_loop_start(struct pw_thread_loop *loop)`
    thread_loop_start: unsafe extern "C" fn(Ptr) -> c_int,
    /// `void pw_thread_loop_stop(struct pw_thread_loop *loop)`
    thread_loop_stop: unsafe extern "C" fn(Ptr),
    /// `void pw_thread_loop_lock(struct pw_thread_loop *loop)`
    thread_loop_lock: unsafe extern "C" fn(Ptr),
    /// `void pw_thread_loop_unlock(struct pw_thread_loop *loop)`
    thread_loop_unlock: unsafe extern "C" fn(Ptr),
    /// `void pw_thread_loop_destroy(struct pw_thread_loop *loop)`
    thread_loop_destroy: unsafe extern "C" fn(Ptr),
    /// `struct pw_context *pw_context_new(struct pw_loop *main_loop, struct pw_properties *props, size_t user_data_size)`
    context_new: unsafe extern "C" fn(Ptr, Ptr, usize) -> Ptr,
    /// `void pw_context_destroy(struct pw_context *context)`
    context_destroy: unsafe extern "C" fn(Ptr),
    /// `struct pw_core *pw_context_connect_fd(struct pw_context *context, int fd, struct pw_properties *properties, size_t user_data_size)`
    context_connect_fd: unsafe extern "C" fn(Ptr, c_int, Ptr, usize) -> Ptr,
    /// `int pw_core_disconnect(struct pw_core *core)`
    core_disconnect: unsafe extern "C" fn(Ptr) -> c_int,
    /// `struct pw_properties *pw_properties_new_string(const char *args)`
    properties_new_string: unsafe extern "C" fn(*const c_char) -> Ptr,
    /// `struct pw_stream *pw_stream_new(struct pw_core *core, const char *name, struct pw_properties *props)`
    stream_new: unsafe extern "C" fn(Ptr, *const c_char, Ptr) -> Ptr,
    /// `void pw_stream_add_listener(struct pw_stream *stream, struct spa_hook *listener, const struct pw_stream_events *events, void *data)`
    stream_add_listener: unsafe extern "C" fn(Ptr, *mut SpaHook, *const PwStreamEvents, Ptr),
    /// `int pw_stream_connect(struct pw_stream *stream, enum pw_direction direction, uint32_t target_id, enum pw_stream_flags flags, const struct spa_pod **params, uint32_t n_params)`
    stream_connect: unsafe extern "C" fn(Ptr, c_int, u32, u32, *mut *const c_void, u32) -> c_int,
    /// `int pw_stream_update_params(struct pw_stream *stream, const struct spa_pod **params, uint32_t n_params)`
    stream_update_params: unsafe extern "C" fn(Ptr, *mut *const c_void, u32) -> c_int,
    /// `struct pw_buffer *pw_stream_dequeue_buffer(struct pw_stream *stream)`
    stream_dequeue_buffer: unsafe extern "C" fn(Ptr) -> *mut PwBuffer,
    /// `int pw_stream_queue_buffer(struct pw_stream *stream, struct pw_buffer *buffer)`
    stream_queue_buffer: unsafe extern "C" fn(Ptr, *mut PwBuffer) -> c_int,
    /// `int pw_stream_disconnect(struct pw_stream *stream)`
    stream_disconnect: unsafe extern "C" fn(Ptr) -> c_int,
    /// `void pw_stream_destroy(struct pw_stream *stream)`
    stream_destroy: unsafe extern "C" fn(Ptr),
}

impl Fns {
    fn load(lib: &SystemLibrary) -> Result<Self, PlatformError> {
        // SAFETY: each type below is the C declaration in its field's doc comment, from
        // <pipewire/pipewire.h>, <pipewire/thread-loop.h>, <pipewire/context.h>,
        // <pipewire/core.h>, <pipewire/properties.h> and <pipewire/stream.h>; `lib` is
        // kept beside the table (in `VideoStream`) so they stay callable.
        unsafe {
            Ok(Self {
                init: lib.function("pw_init")?,
                thread_loop_new: lib.function("pw_thread_loop_new")?,
                thread_loop_get_loop: lib.function("pw_thread_loop_get_loop")?,
                thread_loop_start: lib.function("pw_thread_loop_start")?,
                thread_loop_stop: lib.function("pw_thread_loop_stop")?,
                thread_loop_lock: lib.function("pw_thread_loop_lock")?,
                thread_loop_unlock: lib.function("pw_thread_loop_unlock")?,
                thread_loop_destroy: lib.function("pw_thread_loop_destroy")?,
                context_new: lib.function("pw_context_new")?,
                context_destroy: lib.function("pw_context_destroy")?,
                context_connect_fd: lib.function("pw_context_connect_fd")?,
                core_disconnect: lib.function("pw_core_disconnect")?,
                properties_new_string: lib.function("pw_properties_new_string")?,
                stream_new: lib.function("pw_stream_new")?,
                stream_add_listener: lib.function("pw_stream_add_listener")?,
                stream_connect: lib.function("pw_stream_connect")?,
                stream_update_params: lib.function("pw_stream_update_params")?,
                stream_dequeue_buffer: lib.function("pw_stream_dequeue_buffer")?,
                stream_queue_buffer: lib.function("pw_stream_queue_buffer")?,
                stream_disconnect: lib.function("pw_stream_disconnect")?,
                stream_destroy: lib.function("pw_stream_destroy")?,
            })
        }
    }
}

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
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("captured", &self.captured)
            .field("lost", &self.lost)
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
        if self.format.is_none() {
            self.shared
                .lose("PipeWire settled on a video format JStream didn't offer");
            return;
        }
        self.params = spa::buffer_params().into();
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
        // SAFETY: a dequeued `pw_buffer` and the `spa_buffer`, metas, datas and chunk it
        // points to stay valid until it is queued back, below; each pointer is checked
        // for null and each count bounds the arrays it describes.
        let frame = unsafe { self.read(&*b) };
        // SAFETY: gives the buffer dequeued above back to its stream.
        unsafe { (self.fns.stream_queue_buffer)(self.stream, b) };
        if let Some(frame) = frame {
            self.shared.captured.fetch_add(1, Ordering::Relaxed);
            if self.shared.mailbox.put(frame).is_some() {
                self.shared.skipped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// The frame in `b`, copied out, after the pointer in its cursor metadata is
    /// noted: a buffer can carry a pointer move and no frame.
    ///
    /// # Safety
    /// `b` is a dequeued buffer of this stream, not yet queued back.
    unsafe fn read(&mut self, b: &PwBuffer) -> Option<VideoSurface> {
        if b.buffer.is_null() {
            return None;
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
        let meta = |ty: u32, size: u32| meta_entry(ty, size).map(|m| m.data);
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
        if let Some(track) = &mut self.cursor
            && let Some(m) = meta_entry(META_CURSOR, 1)
        {
            // SAFETY: a meta's `data` holds `size` bytes for the buffer's lifetime;
            // `parse_cursor` checks every offset in them.
            let bytes = unsafe { std::slice::from_raw_parts(m.data.cast::<u8>(), m.size as usize) };
            if let Some(update) = spa::parse_cursor(bytes).and_then(|c| track.update(c, crop)) {
                self.shared.cursor.put(update);
            }
        }
        let format = self.format?;
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
        let mut header_pts = None;
        if let Some(h) = meta(META_HEADER, META_HEADER_SIZE) {
            // SAFETY: a header meta of at least its struct's size; read unaligned to
            // be safe about the producer's layout.
            let h = unsafe { h.cast::<SpaMetaHeader>().read_unaligned() };
            if h.flags & HEADER_CORRUPTED != 0 {
                return None;
            }
            header_pts = Some(h.pts);
        }
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

static EVENTS: PwStreamEvents = PwStreamEvents {
    version: PW_VERSION_STREAM_EVENTS,
    destroy: None,
    state_changed: Some(on_state_changed),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param_changed),
    add_buffer: None,
    remove_buffer: None,
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
    /// `OpenPipeWireRemote`), delivering frames into `shared`, and the pointer too when
    /// `cursor_metadata` (the portal session sends it as metadata).
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when libpipewire lacks a function, and
    /// [`PlatformError::Os`] when PipeWire refuses a step.
    pub(crate) fn connect(
        lib: &SystemLibrary,
        fd: OwnedFd,
        node_id: u32,
        shared: Arc<Shared>,
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
            // SAFETY: `spa_hook` is pointers and an optional function pointer; all-zero
            // (null, None) is the state `pw_stream_add_listener` expects.
            hook: Box::new(unsafe { std::mem::zeroed() }),
            callbacks: Box::new(Callbacks {
                fns,
                stream: ptr::null_mut(),
                format: None,
                clock,
                shared,
                params: Vec::new(),
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
        let offer = spa::enum_format();
        let mut params = [offer.as_ptr()];
        // SAFETY: `pw_stream_connect` with one pod that outlives the call (the stream
        // copies its params).
        let rc = unsafe {
            (fns.stream_connect)(
                self.stream,
                PW_DIRECTION_INPUT,
                node_id,
                STREAM_FLAGS,
                params.as_mut_ptr(),
                1,
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
    use std::mem::{offset_of, size_of};

    use super::{
        PwStreamEvents, Shared, SpaBuffer, SpaChunk, SpaData, SpaHook, SpaMeta, SpaMetaHeader,
        SpaMetaRegion,
    };

    /// The struct layouts match libspa's and libpipewire's on 64-bit Linux.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn layouts_match_the_c_headers() {
        assert_eq!(size_of::<SpaHook>(), 48);
        assert_eq!(size_of::<SpaBuffer>(), 24);
        assert_eq!(size_of::<SpaMeta>(), 16);
        assert_eq!(size_of::<SpaData>(), 40);
        assert_eq!(offset_of!(SpaData, data), 24);
        assert_eq!(offset_of!(SpaData, chunk), 32);
        assert_eq!(size_of::<SpaChunk>(), 16);
        assert_eq!(size_of::<SpaMetaHeader>(), 32);
        assert_eq!(size_of::<SpaMetaRegion>(), 16);
        assert_eq!(offset_of!(PwStreamEvents, process), 64);
        assert_eq!(size_of::<PwStreamEvents>(), 96);
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
