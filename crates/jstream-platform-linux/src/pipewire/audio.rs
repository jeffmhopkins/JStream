//! PipeWire audio (ADR-0060 §4): a connection to the user's PipeWire daemon that reads
//! the registry into a [`Graph`], and the audio streams that run on it.
//!
//! Each [`Connection`] has its own `pw_thread_loop`, PipeWire's own thread, and every
//! callback (the core's, the registry's, the default metadata's and the stream's) runs
//! there, with the loop's lock held, on the one [`State`] the connection owns. So the
//! state has a single user at a time: the loop's thread, or this crate's thread while it
//! holds the lock. A stream is a connection plus one `pw_stream`, so it is also the
//! "one audio thread per stream" of docs/ARCHITECTURE.md#threading. Its `process`
//! callback runs the engine's handler there and doesn't allocate, lock or wait
//! (R-RT-001); the callbacks catch panics (R-UNSAFE-005).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::time::{Duration, Instant};

use jstream_platform::media::{AudioFormat, MediaTime};
use jstream_platform::{
    AudioInputHandler, AudioOutputHandler, AudioPacket, AudioPacketFlags, AudioRenderBuffer,
    AudioStream, MediaClock, PlatformError,
};

use super::ffi::{
    self, CHUNK_SKIP, FLAG_AUTOCONNECT, FLAG_MAP_BUFFERS, Fns, PW_DIRECTION_INPUT,
    PW_DIRECTION_OUTPUT, PW_ID_ANY, PW_ID_CORE, PW_VERSION_CORE_EVENTS, PW_VERSION_METADATA,
    PW_VERSION_METADATA_EVENTS, PW_VERSION_REGISTRY, PW_VERSION_REGISTRY_EVENTS,
    PW_VERSION_STREAM_EVENTS, Ptr, PwBuffer, PwCoreEvents, PwCoreInfo, PwCoreMethods,
    PwMetadataEvents, PwMetadataMethods, PwRegistryEvents, PwRegistryMethods, PwStreamEvents,
    PwTime, STATE_ERROR, STATE_STREAMING, STATE_UNCONNECTED, SpaDict, SpaHook, SpaInterface,
    SpaPod, TYPE_CLIENT, TYPE_METADATA, TYPE_NODE,
};
use super::graph::{Graph, Plan, packet_hns};
use crate::MonotonicClock;
use crate::dl::SystemLibrary;
use crate::spa::audio::{self as spa_audio};

/// How long a registry round trip may take before the daemon counts as unresponsive.
const ROUND_TRIP: Duration = Duration::from_secs(3);

/// The most frames one packet is converted through the scratch buffer, when a
/// buffer's samples aren't aligned for `f32` (which a mapped buffer always is in
/// practice). Longer packets are cut to it.
const SCRATCH_FRAMES: usize = 8192;

/// What the connection's callbacks share, behind the raw pointer every listener was
/// registered with. Used only on the loop's thread, or under the loop's lock.
struct State {
    fns: Fns,
    thread_loop: Ptr,
    graph: Graph,
    registry: Ptr,
    registry_hook: Box<SpaHook>,
    /// The bound `default` metadata object, once the registry announced it.
    metadata: Ptr,
    metadata_hook: Box<SpaHook>,
    /// The sequence number of the round trip being waited for, and whether it is done.
    pending: c_int,
    done: bool,
    /// An error the daemon sent about the core: the connection is unusable.
    error: Option<String>,
    stream: Option<StreamState>,
}

/// A connection to the user's PipeWire daemon, with the registry read.
pub(crate) struct Connection {
    fns: Fns,
    thread_loop: Ptr,
    context: Ptr,
    core: Ptr,
    core_hook: Box<SpaHook>,
    /// From `Box::into_raw`, freed in `Drop` once the loop is stopped. A raw pointer,
    /// not a box, so no `&mut` to it is held while the loop's thread uses it.
    state: *mut State,
    _lib: SystemLibrary,
}

// SAFETY: the raw pointers are PipeWire objects and a state this value owns. Another
// thread touches them only with the loop's lock held (or after the loop is stopped, in
// `Drop`), as libpipewire requires; the callbacks run only on the loop's own thread.
unsafe impl Send for Connection {}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

fn pw_failed(call: &'static str) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: std::io::Error::last_os_error().to_string(),
    }
}

/// The methods table and object of a proxy, read from its `spa_interface` as libspa's
/// `spa_interface_call` macros do.
///
/// # Safety
/// `object` is a live proxy whose interface's methods are a `T`.
unsafe fn methods<'a, T>(object: Ptr) -> Option<(&'a T, *mut c_void)> {
    if object.is_null() {
        return None;
    }
    // SAFETY: the function's contract: a proxy starts with its `spa_interface`.
    let iface = unsafe { &*object.cast::<SpaInterface>() };
    if iface.cb.funcs.is_null() {
        return None;
    }
    // SAFETY: the function's contract.
    Some((unsafe { &*iface.cb.funcs.cast::<T>() }, iface.cb.data))
}

impl Connection {
    /// Connects to the daemon and reads the registry: the nodes, the default devices and
    /// the core's clock rate.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when no daemon runs for this user or
    /// libpipewire lacks a function, and [`PlatformError::Os`] when the daemon refuses
    /// a step or doesn't answer within a few seconds.
    pub(crate) fn open(lib: &SystemLibrary) -> Result<Self, PlatformError> {
        Self::open_to(lib, None)
    }

    /// [`Connection::open`], to the daemon whose socket is `remote` (PipeWire's
    /// `remote.name`) rather than the user's default.
    pub(crate) fn open_to(
        lib: &SystemLibrary,
        remote: Option<&'static CStr>,
    ) -> Result<Self, PlatformError> {
        let fns = Fns::load(lib)?;
        // SAFETY: `pw_init` with no arguments may be called more than once (it keeps a
        // reference count) and from any thread.
        unsafe { (fns.init)(ptr::null_mut(), ptr::null_mut()) };
        // SAFETY: `pw_thread_loop_new` with a static name and no properties.
        let thread_loop = unsafe { (fns.thread_loop_new)(c"jstream-audio".as_ptr(), ptr::null()) };
        if thread_loop.is_null() {
            return Err(pw_failed("pw_thread_loop_new"));
        }
        let state = Box::into_raw(Box::new(State {
            fns,
            thread_loop,
            graph: Graph::default(),
            registry: ptr::null_mut(),
            registry_hook: SpaHook::boxed(),
            metadata: ptr::null_mut(),
            metadata_hook: SpaHook::boxed(),
            pending: 0,
            done: false,
            error: None,
            stream: None,
        }));
        let mut this = Self {
            fns,
            thread_loop,
            context: ptr::null_mut(),
            core: ptr::null_mut(),
            core_hook: SpaHook::boxed(),
            state,
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
        let locked = this.lock();
        let read = this.connect_locked(remote);
        drop(locked);
        read.map(|()| this)
    }

    /// Takes the loop's lock until the guard drops.
    fn lock(&self) -> LoopLock {
        // SAFETY: the loop is alive while `self` is, and the guard unlocks it once.
        unsafe { (self.fns.thread_loop_lock)(self.thread_loop) };
        LoopLock {
            fns: self.fns,
            thread_loop: self.thread_loop,
        }
    }

    fn connect_locked(&mut self, remote: Option<&CStr>) -> Result<(), PlatformError> {
        let fns = self.fns;
        // SAFETY: an empty property string makes an empty set; `pw_context_connect`
        // takes ownership of it.
        let props = unsafe { (fns.properties_new_string)(c"".as_ptr()) };
        if !props.is_null() {
            // SAFETY: NUL-terminated strings, copied by the calls.
            unsafe {
                (fns.properties_set)(props, c"application.name".as_ptr(), c"JStream".as_ptr());
                if let Some(remote) = remote {
                    (fns.properties_set)(props, c"remote.name".as_ptr(), remote.as_ptr());
                }
            };
        }
        // SAFETY: `pw_context_connect` on our context, with the lock held; it takes
        // `props`.
        self.core = unsafe { (fns.context_connect)(self.context, props, 0) };
        if self.core.is_null() {
            return Err(PlatformError::MissingComponent {
                component: "PipeWire daemon",
                remedy: format!(
                    "no PipeWire daemon answered for this user ({}); start it, for \
                     example with `systemctl --user start pipewire wireplumber`",
                    std::io::Error::last_os_error()
                ),
            });
        }
        let data: *mut c_void = self.state.cast();
        // SAFETY: the core is a live proxy whose methods are `pw_core_methods`.
        let (core, object) = unsafe { methods::<PwCoreMethods>(self.core) }
            .ok_or_else(|| pw_failed("pw_core_add_listener"))?;
        let (Some(add_listener), Some(get_registry)) = (core.add_listener, core.get_registry)
        else {
            return Err(pw_failed("pw_core_get_registry"));
        };
        // SAFETY: links `core_hook` (boxed, outlives the core, which `Drop` disconnects
        // first) to the static `CORE_EVENTS`, called with `data` (the state, freed only
        // after the loop stops).
        unsafe {
            add_listener(
                object,
                &raw mut *self.core_hook,
                &raw const CORE_EVENTS,
                data,
            )
        };
        // SAFETY: as above, for the registry, which the state owns.
        unsafe {
            let registry = get_registry(object, PW_VERSION_REGISTRY, 0);
            if registry.is_null() {
                return Err(pw_failed("pw_core_get_registry"));
            }
            (*self.state).registry = registry;
            let (reg, reg_object) = methods::<PwRegistryMethods>(registry)
                .ok_or_else(|| pw_failed("pw_registry_add_listener"))?;
            let add = reg
                .add_listener
                .ok_or_else(|| pw_failed("pw_registry_add_listener"))?;
            add(
                reg_object,
                &raw mut *(*self.state).registry_hook,
                &raw const REGISTRY_EVENTS,
                data,
            );
        }
        // The first round trip delivers the globals, and binds the default metadata;
        // the second delivers the metadata's properties.
        self.round_trip()?;
        self.round_trip()
    }

    /// Waits, with the lock held, until the daemon has answered everything sent so far.
    fn round_trip(&mut self) -> Result<(), PlatformError> {
        // SAFETY: the core is a live proxy whose methods are `pw_core_methods`.
        let (core, object) = unsafe { methods::<PwCoreMethods>(self.core) }
            .ok_or_else(|| pw_failed("pw_core_sync"))?;
        let sync = core.sync.ok_or_else(|| pw_failed("pw_core_sync"))?;
        // SAFETY: `pw_core_sync(core, PW_ID_CORE, 0)` with the lock held; it returns
        // the sequence number its `done` event will carry.
        let seq = unsafe { sync(object, PW_ID_CORE, 0) };
        if seq < 0 {
            return Err(PlatformError::Os {
                call: "pw_core_sync",
                code: u32::try_from(-seq).ok(),
                message: std::io::Error::from_raw_os_error(-seq).to_string(),
            });
        }
        // SAFETY: the lock is held, so the loop's thread isn't using the state.
        unsafe {
            (*self.state).pending = seq;
            (*self.state).done = false;
        }
        let deadline = Instant::now() + ROUND_TRIP;
        loop {
            // SAFETY: as above; the reads end before the wait releases the lock.
            let (done, error) = unsafe { ((*self.state).done, (*self.state).error.clone()) };
            if let Some(message) = error {
                return Err(PlatformError::Os {
                    call: "pw_core_sync",
                    code: None,
                    message,
                });
            }
            if done {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(PlatformError::Os {
                    call: "pw_core_sync",
                    code: None,
                    message: "the PipeWire daemon didn't answer".into(),
                });
            }
            // SAFETY: with the lock held, as the call requires; it releases the lock
            // while it waits for `pw_thread_loop_signal` or a second, then retakes it.
            unsafe { (self.fns.thread_loop_timed_wait)(self.thread_loop, 1) };
        }
    }

    /// The graph as the registry described it.
    pub(crate) fn graph(&self) -> Graph {
        let _locked = self.lock();
        // SAFETY: the lock is held.
        unsafe { (*self.state).graph.clone() }
    }
}

/// The loop's lock, released on drop.
struct LoopLock {
    fns: Fns,
    thread_loop: Ptr,
}

impl Drop for LoopLock {
    fn drop(&mut self) {
        // SAFETY: releases the lock `Connection::lock` took.
        unsafe { (self.fns.thread_loop_unlock)(self.thread_loop) };
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let fns = self.fns;
        // SAFETY: the loop's thread is stopped first, so nothing else uses the state or
        // the proxies; then each object is destroyed once, children before the core:
        // the metadata and registry proxies, the core, the context, the loop. Null
        // pointers are the steps `open` never reached. The state (and a stream's
        // handler in it) is freed last, after its last callback has returned.
        unsafe {
            (fns.thread_loop_stop)(self.thread_loop);
            let state = &mut *self.state;
            if !state.metadata.is_null() {
                (fns.proxy_destroy)(state.metadata);
            }
            if !state.registry.is_null() {
                (fns.proxy_destroy)(state.registry);
            }
            if !self.core.is_null() {
                (fns.core_disconnect)(self.core);
            }
            if !self.context.is_null() {
                (fns.context_destroy)(self.context);
            }
            (fns.thread_loop_destroy)(self.thread_loop);
            drop(Box::from_raw(self.state));
        }
    }
}

/// Runs `f` on the state behind `data`, catching a panic (R-UNSAFE-005). A panic
/// stops a stream's handler from being called again.
///
/// # Safety
/// `data` is the connection's state pointer, and the caller is the loop's thread
/// running a listener, so no other reference to the state exists.
unsafe fn with(data: *mut c_void, f: impl FnOnce(&mut State)) {
    // SAFETY: the function's contract.
    let state = unsafe { &mut *data.cast::<State>() };
    if catch_unwind(AssertUnwindSafe(|| f(&mut *state))).is_err()
        && let Some(stream) = state.stream.as_mut()
    {
        stream.failed = true;
    }
}

unsafe extern "C" fn on_core_info(data: *mut c_void, info: *const PwCoreInfo) {
    // SAFETY: registered with the state (see `with`); `info` is valid for the call.
    unsafe {
        with(data, |s| {
            if !info.is_null() {
                s.graph.set_core_props(&ffi::dict((*info).props));
            }
        });
    }
}

unsafe extern "C" fn on_core_done(data: *mut c_void, id: u32, seq: c_int) {
    // SAFETY: registered with the state (see `with`).
    unsafe {
        with(data, |s| {
            if id == PW_ID_CORE && seq == s.pending {
                s.done = true;
                (s.fns.thread_loop_signal)(s.thread_loop, false);
            }
        });
    }
}

unsafe extern "C" fn on_core_error(
    data: *mut c_void,
    id: u32,
    _seq: c_int,
    res: c_int,
    message: *const c_char,
) {
    // SAFETY: registered with the state (see `with`); `message` is null or a C string
    // valid for the call.
    unsafe {
        with(data, |s| {
            if id != PW_ID_CORE {
                return;
            }
            let message = ffi::string(message).unwrap_or_default();
            let message = format!(
                "PipeWire: {message} ({})",
                std::io::Error::from_raw_os_error(-res)
            );
            if let Some(stream) = s.stream.as_mut() {
                stream.fail(PlatformError::DeviceLost {
                    what: stream.what.clone(),
                    reason: message.clone(),
                });
            }
            s.error = Some(message);
            (s.fns.thread_loop_signal)(s.thread_loop, false);
        });
    }
}

static CORE_EVENTS: PwCoreEvents = PwCoreEvents {
    version: PW_VERSION_CORE_EVENTS,
    info: Some(on_core_info),
    done: Some(on_core_done),
    ping: None,
    error: Some(on_core_error),
    remove_id: None,
    bound_id: None,
    add_mem: None,
    remove_mem: None,
};

unsafe extern "C" fn on_global(
    data: *mut c_void,
    id: u32,
    _permissions: u32,
    type_: *const c_char,
    _version: u32,
    props: *const SpaDict,
) {
    // SAFETY: registered with the state (see `with`); the strings and dict are valid
    // for the call.
    unsafe {
        with(data, |s| {
            let ty = ffi::string(type_).unwrap_or_default();
            let props = ffi::dict(props);
            if ty == TYPE_NODE {
                s.graph.add(id, &props);
            } else if ty == TYPE_CLIENT {
                s.graph.add_client(id, &props);
            } else if ty == TYPE_METADATA
                && s.metadata.is_null()
                && props
                    .iter()
                    .any(|(k, v)| k == "metadata.name" && v == "default")
            {
                s.bind_metadata(id);
            }
        });
    }
}

unsafe extern "C" fn on_global_remove(data: *mut c_void, id: u32) {
    // SAFETY: registered with the state (see `with`).
    unsafe {
        with(data, |s| {
            s.graph.remove(id);
            if let Some(stream) = s.stream.as_mut()
                && stream.watch == Some(id)
            {
                stream.fail(PlatformError::DeviceLost {
                    what: stream.what.clone(),
                    reason: "it was removed".into(),
                });
            }
        });
    }
}

static REGISTRY_EVENTS: PwRegistryEvents = PwRegistryEvents {
    version: PW_VERSION_REGISTRY_EVENTS,
    global: Some(on_global),
    global_remove: Some(on_global_remove),
};

unsafe extern "C" fn on_metadata_property(
    data: *mut c_void,
    subject: u32,
    key: *const c_char,
    _type: *const c_char,
    value: *const c_char,
) -> c_int {
    // SAFETY: registered with the state (see `with`); the strings are null or valid for
    // the call.
    unsafe {
        with(data, |s| {
            if subject != PW_ID_CORE {
                return;
            }
            match ffi::string(key) {
                Some(key) => s.graph.set_default(&key, ffi::string(value).as_deref()),
                // A null key clears every property.
                None => {
                    s.graph.set_default("default.audio.source", None);
                    s.graph.set_default("default.audio.sink", None);
                }
            }
        });
    }
    0
}

static METADATA_EVENTS: PwMetadataEvents = PwMetadataEvents {
    version: PW_VERSION_METADATA_EVENTS,
    property: Some(on_metadata_property),
};

impl State {
    /// Binds the `default` metadata object `id` and listens to its properties. On the
    /// loop's thread, from the registry's `global` event.
    fn bind_metadata(&mut self, id: u32) {
        let Ok(ty) = CString::new(TYPE_METADATA) else {
            return;
        };
        // SAFETY: the registry is a live proxy whose methods are `pw_registry_methods`;
        // `pw_registry_bind` returns a new proxy (freed in `Connection::drop`), whose
        // methods are `pw_metadata_methods`. The hook is boxed in the state and the
        // listener data is the state, both alive until the proxy is destroyed.
        unsafe {
            let Some((reg, object)) = methods::<PwRegistryMethods>(self.registry) else {
                return;
            };
            let Some(bind) = reg.bind else { return };
            let metadata = bind(object, id, ty.as_ptr(), PW_VERSION_METADATA, 0);
            if metadata.is_null() {
                return;
            }
            self.metadata = metadata;
            let Some((m, m_object)) = methods::<PwMetadataMethods>(metadata) else {
                return;
            };
            if let Some(add) = m.add_listener {
                let data: *mut State = self;
                add(
                    m_object,
                    &raw mut *self.metadata_hook,
                    &raw const METADATA_EVENTS,
                    data.cast(),
                );
            }
        }
    }
}

/// The engine's side of a stream.
enum Handler {
    Input(Box<dyn AudioInputHandler>),
    Output(Box<dyn AudioOutputHandler>),
}

/// A stream's state, in the connection's [`State`].
struct StreamState {
    stream: Ptr,
    handler: Handler,
    rate: u32,
    channels: usize,
    /// Frames delivered or rendered so far.
    position: u64,
    /// For buffers whose samples aren't `f32`-aligned; allocated before the stream
    /// starts, so `process` never allocates.
    scratch: Vec<f32>,
    /// Set once PipeWire settled on the requested format.
    settled: bool,
    streaming: bool,
    /// Set once the handler has been told the stream failed, after a panic, or when the
    /// stream is being dropped: it isn't called again.
    failed: bool,
    /// The next packet follows a gap (a corrupted or empty buffer).
    discontinuity: bool,
    /// The node whose removal ends the stream.
    watch: Option<u32>,
    what: String,
    clock: MonotonicClock,
    fns: Fns,
}

impl StreamState {
    /// Tells the handler the stream failed, once.
    fn fail(&mut self, error: PlatformError) {
        if self.failed {
            return;
        }
        self.failed = true;
        match &mut self.handler {
            Handler::Input(h) => h.on_error(&error),
            Handler::Output(h) => h.on_error(&error),
        }
    }

    fn param_changed(&mut self, id: u32, param: *const SpaPod) {
        if id != crate::spa::PARAM_FORMAT || param.is_null() {
            return;
        }
        // SAFETY: PipeWire passes a valid pod for the call's duration: a header, then
        // `size` body bytes.
        let pod = unsafe {
            let size = (*param).size as usize;
            std::slice::from_raw_parts(param.cast::<u8>(), 8 + size)
        };
        let layout = spa_audio::layout_for(u32::try_from(self.channels).ok());
        match spa_audio::parse_format(pod) {
            Some(f) if spa_audio::is_requested(f, self.rate, layout) => self.settled = true,
            other => self.fail(PlatformError::Unsupported {
                what: format!(
                    "PipeWire's format for {} ({other:?}), not the f32 at {} Hz asked for",
                    self.what, self.rate
                ),
            }),
        }
    }

    fn state_changed(&mut self, state: c_int, error: *const c_char) {
        match state {
            STATE_STREAMING => self.streaming = true,
            STATE_ERROR => {
                // SAFETY: null or a C string valid for the call.
                let reason = unsafe { ffi::string(error) }.unwrap_or_default();
                self.fail(PlatformError::DeviceLost {
                    what: self.what.clone(),
                    reason: format!("PipeWire stream error: {reason}"),
                });
            }
            STATE_UNCONNECTED if self.streaming => self.fail(PlatformError::DeviceLost {
                what: self.what.clone(),
                reason: "the PipeWire stream disconnected".into(),
            }),
            _ => {}
        }
    }

    /// The stream's timing now: PipeWire's time report, or the clock with no delay
    /// before the first graph cycle.
    fn time(&self) -> (i64, i64, (u32, u32)) {
        let mut t = PwTime::default();
        // SAFETY: `pw_stream_get_time_n` fills at most `size_of::<PwTime>()` bytes of
        // `t`, on this stream, from its loop's thread.
        let rc =
            unsafe { (self.fns.stream_get_time_n)(self.stream, &raw mut t, size_of::<PwTime>()) };
        if rc < 0 || t.now <= 0 {
            let now = self.clock.now().ticks().saturating_mul(100);
            return (now, 0, (1, self.rate));
        }
        (t.now, t.delay, (t.rate.num, t.rate.denom))
    }

    fn process(&mut self) {
        // SAFETY: `pw_stream_dequeue_buffer` on this stream, on its loop's thread.
        let b = unsafe { (self.fns.stream_dequeue_buffer)(self.stream) };
        if b.is_null() {
            return;
        }
        if self.settled && !self.failed {
            // SAFETY: a dequeued `pw_buffer` and the `spa_buffer`, datas and chunk it
            // points to stay valid until it is queued back, below.
            unsafe {
                match self.handler {
                    Handler::Input(_) => self.record(b),
                    Handler::Output(_) => self.play(b),
                }
            }
        } else if matches!(self.handler, Handler::Output(_)) {
            // SAFETY: as above.
            unsafe { silence(b) };
        }
        // SAFETY: gives the buffer dequeued above back to its stream.
        unsafe { (self.fns.stream_queue_buffer)(self.stream, b) };
    }

    /// Hands the samples in `b` to the input handler.
    ///
    /// # Safety
    /// `b` is a dequeued buffer of this stream, not yet queued back.
    unsafe fn record(&mut self, b: *mut PwBuffer) {
        // SAFETY: the caller's contract; each pointer is checked before it is read.
        let Some((data, maxsize, chunk)) = (unsafe { first_data(b) }) else {
            return;
        };
        // SAFETY: as above.
        let c = unsafe { &*chunk };
        if c.flags & CHUNK_SKIP != 0 {
            self.discontinuity = true;
            return;
        }
        let stride = self.channels * 4;
        let offset = (c.offset as usize).min(maxsize);
        let size = (c.size as usize).min(maxsize - offset);
        let mut frames = size / stride;
        if frames == 0 {
            return;
        }
        // SAFETY: MAP_BUFFERS mapped `maxsize` bytes at `data` for the buffer's
        // lifetime, and `offset + frames * stride <= maxsize`.
        let start = unsafe { data.cast::<u8>().add(offset) };
        let samples: &[f32] = if start.align_offset(align_of::<f32>()) == 0 {
            // SAFETY: aligned, in bounds (above), and every bit pattern is an `f32`.
            unsafe { std::slice::from_raw_parts(start.cast::<f32>(), frames * self.channels) }
        } else {
            frames = frames.min(SCRATCH_FRAMES);
            let n = frames * self.channels;
            // SAFETY: `n * 4` bytes are in bounds at `start` (above) and fit `scratch`
            // (`SCRATCH_FRAMES` frames); the two don't overlap.
            unsafe {
                ptr::copy_nonoverlapping(start, self.scratch.as_mut_ptr().cast::<u8>(), n * 4);
            }
            &self.scratch[..n]
        };
        let (now, delay, rate) = self.time();
        let time = packet_hns(now, delay, rate, frames as u64, self.rate, true)
            .map(|t| MediaTime::new(t, self.clock.timebase()));
        let packet = AudioPacket {
            samples,
            device_position: self.position,
            time,
            flags: AudioPacketFlags {
                discontinuity: std::mem::take(&mut self.discontinuity),
                silent: false,
            },
        };
        if let Handler::Input(h) = &mut self.handler {
            h.on_packet(&packet);
        }
        self.position += frames as u64;
    }

    /// Fills `b` from the output handler.
    ///
    /// # Safety
    /// `b` is a dequeued buffer of this stream, not yet queued back.
    unsafe fn play(&mut self, b: *mut PwBuffer) {
        // SAFETY: the caller's contract; each pointer is checked before it is read.
        let Some((data, maxsize, chunk)) = (unsafe { first_data(b) }) else {
            return;
        };
        let stride = self.channels * 4;
        // SAFETY: the caller's contract.
        let requested = usize::try_from(unsafe { (*b).requested }).unwrap_or(usize::MAX);
        let quantum = (self.rate as usize / 100).max(1);
        let mut frames = if requested > 0 { requested } else { quantum }.min(maxsize / stride);
        let aligned = data.cast::<u8>().align_offset(align_of::<f32>()) == 0;
        if !aligned {
            frames = frames.min(SCRATCH_FRAMES);
        }
        let n = frames * self.channels;
        let (now, delay, rate) = self.time();
        let time = packet_hns(now, delay, rate, frames as u64, self.rate, false)
            .map(|t| MediaTime::new(t, self.clock.timebase()));
        let samples: &mut [f32] = if aligned {
            // SAFETY: aligned, `n * 4 <= maxsize` mapped bytes, and the buffer is ours
            // until it is queued back; any `f32` may be written.
            unsafe { std::slice::from_raw_parts_mut(data.cast::<f32>(), n) }
        } else {
            &mut self.scratch[..n]
        };
        let mut buffer = AudioRenderBuffer {
            samples,
            device_position: self.position,
            time,
        };
        if let Handler::Output(h) = &mut self.handler {
            h.render(&mut buffer);
        }
        if !aligned {
            // SAFETY: `n * 4` bytes from `scratch` into the mapping, in bounds (above),
            // not overlapping.
            unsafe {
                ptr::copy_nonoverlapping(
                    self.scratch.as_ptr().cast::<u8>(),
                    data.cast::<u8>(),
                    n * 4,
                );
            }
        }
        // SAFETY: the chunk and buffer are ours until queued back.
        unsafe {
            let c = &mut *chunk;
            c.offset = 0;
            c.stride = i32::try_from(stride).unwrap_or(i32::MAX);
            c.size = u32::try_from(n * 4).unwrap_or(0);
            (*b).size = frames as u64;
        }
        self.position += frames as u64;
    }
}

/// The first data block of `b`: its mapping, size and chunk, when they are there.
///
/// # Safety
/// `b` is a dequeued, not yet queued back, buffer.
unsafe fn first_data(b: *mut PwBuffer) -> Option<(Ptr, usize, *mut ffi::SpaChunk)> {
    // SAFETY: the function's contract; each pointer is checked before it is followed,
    // and `datas` holds `n_datas` entries.
    unsafe {
        let buf = (*b).buffer;
        if buf.is_null() || (*buf).n_datas == 0 || (*buf).datas.is_null() {
            return None;
        }
        let d = &*(*buf).datas;
        if d.data.is_null() || d.chunk.is_null() {
            return None;
        }
        Some((d.data, d.maxsize as usize, d.chunk))
    }
}

/// Marks a playback buffer empty, for a stream that has nothing to play.
///
/// # Safety
/// As for [`first_data`].
unsafe fn silence(b: *mut PwBuffer) {
    // SAFETY: the function's contract.
    if let Some((_, _, chunk)) = unsafe { first_data(b) } {
        // SAFETY: the chunk is ours until the buffer is queued back.
        unsafe {
            (*chunk).size = 0;
            (*b).size = 0;
        }
    }
}

/// Runs `f` on the connection's stream, if it has one (see [`with`]).
///
/// # Safety
/// As for [`with`].
unsafe fn with_stream(data: *mut c_void, f: impl FnOnce(&mut StreamState)) {
    // SAFETY: the function's contract.
    unsafe {
        with(data, |s| {
            if let Some(stream) = s.stream.as_mut() {
                f(stream);
            }
        });
    }
}

unsafe extern "C" fn on_stream_state(
    data: *mut c_void,
    _old: c_int,
    state: c_int,
    error: *const c_char,
) {
    // SAFETY: registered with the state (see `with`).
    unsafe { with_stream(data, |s| s.state_changed(state, error)) };
}

unsafe extern "C" fn on_stream_param(data: *mut c_void, id: u32, param: *const SpaPod) {
    // SAFETY: registered with the state (see `with`).
    unsafe { with_stream(data, |s| s.param_changed(id, param)) };
}

unsafe extern "C" fn on_stream_process(data: *mut c_void) {
    // SAFETY: registered with the state (see `with`).
    unsafe { with_stream(data, StreamState::process) };
}

static STREAM_EVENTS: PwStreamEvents = PwStreamEvents {
    version: PW_VERSION_STREAM_EVENTS,
    destroy: None,
    state_changed: Some(on_stream_state),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_stream_param),
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_stream_process),
    drained: None,
    command: None,
    trigger_done: None,
};

/// A running PipeWire audio stream. Dropping it disconnects the stream, stops its
/// thread and then drops the handler.
pub(crate) struct PwAudioStream {
    conn: Connection,
    stream: Ptr,
    hook: Box<SpaHook>,
    format: AudioFormat,
}

// SAFETY: as for `Connection`; the stream pointer is touched only under the loop's lock.
unsafe impl Send for PwAudioStream {}

impl std::fmt::Debug for PwAudioStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PwAudioStream")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl AudioStream for PwAudioStream {
    fn format(&self) -> AudioFormat {
        self.format
    }
}

/// A recording stream's or a playback stream's handler, before it is opened.
pub(crate) enum Open {
    Input(Box<dyn AudioInputHandler>),
    Output(Box<dyn AudioOutputHandler>),
}

impl PwAudioStream {
    /// Connects a stream on `conn` as `plan` says.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when PipeWire refuses a step.
    pub(crate) fn open(
        conn: Connection,
        plan: &Plan,
        handler: Open,
    ) -> Result<Self, PlatformError> {
        let format = plan.format()?;
        let mut this = Self {
            conn,
            stream: ptr::null_mut(),
            hook: SpaHook::boxed(),
            format,
        };
        let locked = this.conn.lock();
        let connected = this.connect_locked(plan, handler);
        drop(locked);
        connected.map(|()| this)
    }

    fn connect_locked(&mut self, plan: &Plan, handler: Open) -> Result<(), PlatformError> {
        let fns = self.conn.fns;
        let (direction, category, name, handler) = match handler {
            Open::Input(h) => (
                PW_DIRECTION_INPUT,
                "Capture",
                c"JStream input",
                Handler::Input(h),
            ),
            Open::Output(h) => (
                PW_DIRECTION_OUTPUT,
                "Playback",
                c"JStream output",
                Handler::Output(h),
            ),
        };
        let mut props: Vec<(&str, String)> = vec![
            ("media.type", "Audio".into()),
            ("media.category", category.into()),
            ("media.role", "Production".into()),
            // 10 ms, as the ALSA backend's periods.
            (
                "node.latency",
                format!("{}/{}", (plan.rate / 100).max(1), plan.rate),
            ),
        ];
        if let Some(target) = &plan.target {
            props.push(("target.object", target.clone()));
            // A named device that goes away ends the stream (DeviceLost) instead of
            // moving it to another one.
            props.push(("node.dont-reconnect", "true".into()));
            props.push(("node.dont-fallback", "true".into()));
        }
        if plan.capture_sink {
            props.push(("stream.capture.sink", "true".into()));
        }
        // SAFETY: an empty property string makes an empty set; the stream takes it.
        let pw_props = unsafe { (fns.properties_new_string)(c"".as_ptr()) };
        if pw_props.is_null() {
            return Err(pw_failed("pw_properties_new_string"));
        }
        for (key, value) in &props {
            let (Ok(k), Ok(v)) = (CString::new(*key), CString::new(value.as_str())) else {
                continue;
            };
            // SAFETY: two NUL-terminated strings, copied by the call.
            unsafe { (fns.properties_set)(pw_props, k.as_ptr(), v.as_ptr()) };
        }
        // SAFETY: `pw_stream_new` on our core, under the lock, with a static name; it
        // takes `pw_props`.
        self.stream = unsafe { (fns.stream_new)(self.conn.core, name.as_ptr(), pw_props) };
        if self.stream.is_null() {
            return Err(pw_failed("pw_stream_new"));
        }
        let channels = plan.layout.channel_count();
        let clock = MonotonicClock::new()?;
        // SAFETY: the lock is held, so the loop's thread isn't using the state.
        unsafe {
            (*self.conn.state).stream = Some(StreamState {
                stream: self.stream,
                handler,
                rate: plan.rate,
                channels,
                position: 0,
                scratch: vec![0.0; SCRATCH_FRAMES * channels],
                settled: false,
                streaming: false,
                failed: false,
                discontinuity: false,
                watch: plan.watch,
                what: plan.what.clone(),
                clock,
                fns,
            });
        }
        let data: *mut c_void = self.conn.state.cast();
        // SAFETY: links `hook` (boxed, outlives the stream, which `Drop` destroys first)
        // to the static `STREAM_EVENTS`, called with the state.
        unsafe {
            (fns.stream_add_listener)(
                self.stream,
                &raw mut *self.hook,
                &raw const STREAM_EVENTS,
                data,
            )
        };
        let offer = spa_audio::enum_format(plan.rate, plan.layout).ok_or_else(|| {
            PlatformError::Unsupported {
                what: format!("the channel layout {:?} through PipeWire", plan.layout),
            }
        })?;
        let mut params = [offer.as_ptr()];
        // SAFETY: `pw_stream_connect` with one pod that outlives the call (the stream
        // copies its params); the target is in the properties, so the id is "any".
        let rc = unsafe {
            (fns.stream_connect)(
                self.stream,
                direction,
                PW_ID_ANY,
                FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS,
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

impl Drop for PwAudioStream {
    fn drop(&mut self) {
        if self.stream.is_null() {
            return;
        }
        let fns = self.conn.fns;
        let _locked = self.conn.lock();
        // SAFETY: under the loop's lock: destroying the stream removes its listener, so
        // its callbacks never run again. The handler stays in the state until the
        // connection, dropped next, has stopped the loop. Disconnecting reports a state
        // change, which isn't the stream failing: the handler isn't told.
        unsafe {
            if let Some(stream) = (*self.conn.state).stream.as_mut() {
                stream.failed = true;
            }
            (fns.stream_disconnect)(self.stream);
            (fns.stream_destroy)(self.stream);
        }
    }
}
