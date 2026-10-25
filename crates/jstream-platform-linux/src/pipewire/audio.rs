//! PipeWire audio (ADR-0060 §4): a connection to the user's PipeWire daemon that reads
//! the registry into a [`Graph`], and the audio streams that run on it.
//!
//! Each [`Connection`] has its own `pw_thread_loop`, PipeWire's own thread, and its
//! callbacks (the core's, the registry's, the default metadata's, the nodes' and the
//! stream's state and format) run there, with the loop's lock held, on the one
//! [`State`] the connection owns. So the state has a single user at a time: the loop's
//! thread, or this crate's thread while it holds the lock.
//!
//! A stream is a connection plus one `pw_stream`, connected with `RT_PROCESS`: its
//! `process` callback runs on the connection's context's data loop, PipeWire's
//! real-time thread (given real-time priority by `module-rt`, when the system allows),
//! which is the "one audio thread per stream" of docs/ARCHITECTURE.md#threading. It
//! never touches [`State`]: it uses the stream's [`Realtime`], which it shares with the
//! thread loop through atomics, and runs the engine's handler without allocating,
//! locking or waiting (R-RT-001). Every callback catches panics (R-UNSAFE-005).

use std::cell::UnsafeCell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use jstream_platform::media::{AudioFormat, MediaTime};
use jstream_platform::{
    AudioInputHandler, AudioOutputHandler, AudioPacket, AudioPacketFlags, AudioRenderBuffer,
    AudioStream, MediaClock, PlatformError,
};

use super::ffi::{
    self, CHUNK_CORRUPTED, CHUNK_EMPTY, FLAG_AUTOCONNECT, FLAG_MAP_BUFFERS, FLAG_RT_PROCESS, Fns,
    PW_DIRECTION_INPUT, PW_DIRECTION_OUTPUT, PW_ID_ANY, PW_ID_CORE, PW_VERSION_CORE_EVENTS,
    PW_VERSION_METADATA, PW_VERSION_METADATA_EVENTS, PW_VERSION_NODE, PW_VERSION_NODE_EVENTS,
    PW_VERSION_REGISTRY, PW_VERSION_REGISTRY_EVENTS, PW_VERSION_STREAM_EVENTS, Ptr, PwBuffer,
    PwCoreEvents, PwCoreInfo, PwCoreMethods, PwMetadataEvents, PwMetadataMethods, PwNodeEvents,
    PwNodeInfo, PwNodeMethods, PwRegistryEvents, PwRegistryMethods, PwStreamEvents, PwTime,
    STATE_ERROR, STATE_STREAMING, STATE_UNCONNECTED, SpaDict, SpaHook, SpaInterface, SpaPod,
    TYPE_CLIENT, TYPE_METADATA, TYPE_NODE,
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
    /// The bound device nodes, whose info carries their channel count: each global id,
    /// proxy and listener's hook. Destroyed when the node's global goes, or in
    /// `Connection::drop`.
    nodes: Vec<(u32, Ptr, Box<SpaHook>)>,
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
            nodes: Vec::new(),
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
        // The first round trip delivers the globals, and binds the default metadata and
        // each device node; the second delivers the metadata's properties and the
        // nodes' info (their channel counts).
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
        // the bound device node proxies, the metadata and registry proxies, the core,
        // the context, the loop. Null
        // pointers are the steps `open` never reached. The state (and a stream's
        // handler in it) is freed last, after its last callback has returned.
        unsafe {
            (fns.thread_loop_stop)(self.thread_loop);
            let state = &mut *self.state;
            for (_, node, _) in &state.nodes {
                (fns.proxy_destroy)(*node);
            }
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
/// fails the stream: its handler is told once, through a guarded `on_error`, and isn't
/// called again.
///
/// # Safety
/// `data` is the connection's state pointer, and the caller is the loop's thread
/// running a listener, so no other reference to the state exists.
unsafe fn with(data: *mut c_void, f: impl FnOnce(&mut State)) {
    // SAFETY: the function's contract.
    let state = unsafe { &mut *data.cast::<State>() };
    if catch_unwind(AssertUnwindSafe(|| f(&mut *state))).is_err()
        && let Some(stream) = state.stream.as_ref()
    {
        // `fail` marks the stream failed before it calls the handler, so a panic in
        // `on_error` still leaves it stopped.
        let _ = catch_unwind(AssertUnwindSafe(|| stream.fail(&panicked())));
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
            if let Some(stream) = s.stream.as_ref() {
                stream.fail(&PlatformError::DeviceLost {
                    what: stream.rt().what.clone(),
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
                if s.graph.is_device(id) {
                    s.bind_node(id);
                }
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
            s.unbind_node(id);
            if let Some(stream) = s.stream.as_ref()
                && stream.watch == Some(id)
            {
                stream.fail(&PlatformError::DeviceLost {
                    what: stream.rt().what.clone(),
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

unsafe extern "C" fn on_node_info(data: *mut c_void, info: *const PwNodeInfo) {
    // SAFETY: registered with the state (see `with`); `info` and its dict are valid for
    // the call.
    unsafe {
        with(data, |s| {
            let Some(info) = info.as_ref() else { return };
            let props = ffi::dict(info.props);
            s.graph.set_node_info(info.id, &props);
        });
    }
}

static NODE_EVENTS: PwNodeEvents = PwNodeEvents {
    version: PW_VERSION_NODE_EVENTS,
    info: Some(on_node_info),
    param: None,
};

impl State {
    /// Binds device node `id` for its info (its channel count; see `graph`). On the
    /// loop's thread, from the registry's `global` event, so its first info arrives
    /// before the round trip that follows.
    fn bind_node(&mut self, id: u32) {
        let Ok(ty) = CString::new(TYPE_NODE) else {
            return;
        };
        // SAFETY: the registry is a live proxy whose methods are `pw_registry_methods`;
        // `pw_registry_bind` returns a new proxy (freed in `Connection::drop`), whose
        // methods are `pw_node_methods`. The hook is boxed in the state and the listener
        // data is the state, both alive until the proxy is destroyed.
        unsafe {
            let Some((reg, object)) = methods::<PwRegistryMethods>(self.registry) else {
                return;
            };
            let Some(bind) = reg.bind else { return };
            let node = bind(object, id, ty.as_ptr(), PW_VERSION_NODE, 0);
            if node.is_null() {
                return;
            }
            let mut hook = SpaHook::boxed();
            if let Some((n, n_object)) = methods::<PwNodeMethods>(node)
                && let Some(add) = n.add_listener
            {
                let data: *mut State = self;
                add(
                    n_object,
                    &raw mut *hook,
                    &raw const NODE_EVENTS,
                    data.cast(),
                );
            }
            self.nodes.push((id, node, hook));
        }
    }

    /// Destroys the proxy bound to device node `id`, once its global is gone, so a
    /// device that comes and goes during a stream leaves nothing behind. On the loop's
    /// thread, from the registry's `global_remove` event.
    fn unbind_node(&mut self, id: u32) {
        let Some(at) = self.nodes.iter().position(|(n, _, _)| *n == id) else {
            return;
        };
        let (_, node, mut hook) = self.nodes.swap_remove(at);
        // SAFETY: on the loop's thread, which owns the proxy's listener list: the hook
        // is unlinked from it before it is freed, then the proxy (bound in `bind_node`,
        // not yet destroyed) is destroyed once.
        unsafe {
            hook.remove();
            (self.fns.proxy_destroy)(node);
        }
    }

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

impl Handler {
    fn on_error(&mut self, error: &PlatformError) {
        match self {
            Handler::Input(h) => h.on_error(error),
            Handler::Output(h) => h.on_error(error),
        }
    }
}

/// Who may use a stream's [`Exclusive`]: `process` on the data loop, between
/// [`Gate::enter`] and [`Gate::leave`], until the stream fails; then, once, whichever
/// thread failed it (to tell the handler).
#[derive(Debug, Default)]
struct Gate(AtomicU8);

impl Gate {
    /// A `process` call is between `enter` and `leave`.
    const IN_PROCESS: u8 = 1 << 0;
    /// The stream failed, or is being dropped. The handler has been (or is being) told,
    /// and `enter` refuses from now on.
    const FAILED: u8 = 1 << 1;

    /// From `process`: true when the stream hasn't failed, and `process` now has
    /// [`Exclusive`] until it calls [`Gate::leave`]. Never waits.
    fn enter(&self) -> bool {
        self.0
            .compare_exchange(0, Self::IN_PROCESS, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// From `process`, after a true [`Gate::enter`]: gives [`Exclusive`] back.
    fn leave(&self) {
        self.0.fetch_and(!Self::IN_PROCESS, Ordering::Release);
    }

    /// Marks the stream failed. True when this call is the one that did. From
    /// `process` (between `enter` and `leave`, so it keeps [`Exclusive`]), or through
    /// [`Gate::stop`].
    fn fail(&self) -> bool {
        self.0.fetch_or(Self::FAILED, Ordering::AcqRel) & Self::FAILED == 0
    }

    /// Off the data loop: marks the stream failed and waits for a running `process`
    /// call to leave. True when this call marked it, so the caller now has
    /// [`Exclusive`] to itself and is the one to tell the handler. The wait is at most
    /// one handler callback, which doesn't block (R-RT-001).
    fn stop(&self) -> bool {
        let first = self.fail();
        while self.0.load(Ordering::Acquire) & Self::IN_PROCESS != 0 {
            std::thread::yield_now();
        }
        first
    }
}

/// The error a handler is told when one of its callbacks panicked.
fn panicked() -> PlatformError {
    PlatformError::InvalidState {
        what: "the audio stream",
        state: "its audio callback panicked, so it was stopped",
    }
}

/// What a stream's `process` callback uses. The stream is connected with
/// `PW_STREAM_FLAG_RT_PROCESS`, so `process` runs on PipeWire's real-time data loop
/// while the stream's other callbacks run on the connection's thread loop, at the same
/// time. They share this, and only through `&`: the fields are read-only or atomic, and
/// [`Exclusive`] is used by one thread at a time, as its [`Gate`] says.
struct Realtime {
    stream: Ptr,
    output: bool,
    rate: u32,
    channels: usize,
    what: String,
    clock: MonotonicClock,
    fns: Fns,
    gate: Gate,
    /// Set once PipeWire settled on the requested format.
    settled: AtomicBool,
    exclusive: UnsafeCell<Exclusive>,
}

/// The part of [`Realtime`] one thread at a time uses (see [`Gate`]).
struct Exclusive {
    handler: Handler,
    /// Frames delivered or rendered so far.
    position: u64,
    /// For buffers whose samples aren't `f32`-aligned; allocated before the stream
    /// starts, so `process` never allocates.
    scratch: Vec<f32>,
    /// The next packet follows a gap (a corrupted or empty buffer).
    discontinuity: bool,
}

impl Realtime {
    /// Stops the stream without telling the handler: `process` won't call it again.
    fn stop(&self) {
        self.gate.stop();
    }

    /// Tells the handler the stream failed, once. Off the data loop.
    fn fail(&self, error: &PlatformError) {
        if self.gate.stop() {
            // SAFETY: this call failed the stream and `process` has left, so nothing
            // else uses `exclusive` (see `Gate`).
            unsafe { (*self.exclusive.get()).handler.on_error(error) };
        }
    }

    /// The stream's timing now: PipeWire's time report, or the clock with no delay
    /// before the first graph cycle.
    fn time(&self) -> (i64, i64, (u32, u32)) {
        let mut t = PwTime::default();
        // SAFETY: `pw_stream_get_time_n` fills at most `size_of::<PwTime>()` bytes of
        // `t`, on this stream; it is real-time safe and may be called from `process`.
        let rc =
            unsafe { (self.fns.stream_get_time_n)(self.stream, &raw mut t, size_of::<PwTime>()) };
        if rc < 0 || t.now <= 0 {
            let now = self.clock.now().ticks().saturating_mul(100);
            return (now, 0, (1, self.rate));
        }
        (t.now, t.delay, (t.rate.num, t.rate.denom))
    }

    /// The `process` callback, on the data loop.
    fn process(&self) {
        // SAFETY: `pw_stream_dequeue_buffer` on this stream, from `process`.
        let b = unsafe { (self.fns.stream_dequeue_buffer)(self.stream) };
        if b.is_null() {
            return;
        }
        let mut filled = false;
        if self.settled.load(Ordering::Acquire) && self.gate.enter() {
            // SAFETY: entered, so `exclusive` is ours until `leave` (see `Gate`).
            let ex = unsafe { &mut *self.exclusive.get() };
            let run = catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: a dequeued `pw_buffer` and the `spa_buffer`, datas and chunk
                // it points to stay valid until it is queued back, below.
                unsafe {
                    if self.output {
                        self.play(ex, b);
                    } else {
                        self.record(ex, b);
                    }
                }
            }));
            filled = run.is_ok();
            // A panic fails the stream (R-UNSAFE-005). We haven't left, so the handler
            // is still ours; if another thread failed the stream meanwhile, it waits for
            // us to leave and tells the handler itself.
            if run.is_err() && self.gate.fail() {
                let _ = catch_unwind(AssertUnwindSafe(|| ex.handler.on_error(&panicked())));
            }
            self.gate.leave();
        }
        if !filled && self.output {
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
    unsafe fn record(&self, ex: &mut Exclusive, b: *mut PwBuffer) {
        // SAFETY: the caller's contract; each pointer is checked before it is read.
        let Some((data, maxsize, chunk)) = (unsafe { first_data(b) }) else {
            return;
        };
        // SAFETY: as above.
        let c = unsafe { &*chunk };
        let stride = self.channels * 4;
        if c.flags & CHUNK_CORRUPTED != 0 {
            // The frames are lost but still count, so the next packet's position jumps
            // (when the chunk says how many there were).
            ex.position += (c.size as usize / stride) as u64;
            ex.discontinuity = true;
            return;
        }
        // An empty chunk (nothing played into the source) is silence, not a loss: it is
        // delivered as a silent packet, so the engine keeps getting times.
        let silent = c.flags & CHUNK_EMPTY != 0;
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
                ptr::copy_nonoverlapping(start, ex.scratch.as_mut_ptr().cast::<u8>(), n * 4);
            }
            &ex.scratch[..n]
        };
        let (now, delay, rate) = self.time();
        let time = packet_hns(now, delay, rate, frames as u64, self.rate, true)
            .map(|t| MediaTime::new(t, self.clock.timebase()));
        let packet = AudioPacket {
            samples,
            device_position: ex.position,
            time,
            flags: AudioPacketFlags {
                discontinuity: std::mem::take(&mut ex.discontinuity),
                silent,
            },
        };
        if let Handler::Input(h) = &mut ex.handler {
            h.on_packet(&packet);
        }
        ex.position += frames as u64;
    }

    /// Fills `b` from the output handler.
    ///
    /// # Safety
    /// `b` is a dequeued buffer of this stream, not yet queued back.
    unsafe fn play(&self, ex: &mut Exclusive, b: *mut PwBuffer) {
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
            &mut ex.scratch[..n]
        };
        let mut buffer = AudioRenderBuffer {
            samples,
            device_position: ex.position,
            time,
        };
        if let Handler::Output(h) = &mut ex.handler {
            h.render(&mut buffer);
        }
        if !aligned {
            // SAFETY: `n * 4` bytes from `scratch` into the mapping, in bounds (above),
            // not overlapping.
            unsafe {
                ptr::copy_nonoverlapping(
                    ex.scratch.as_ptr().cast::<u8>(),
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
        ex.position += frames as u64;
    }
}

/// A stream's state on the connection's thread loop, in its [`State`].
struct StreamState {
    /// From `Box::into_raw`, freed on drop: the data loop uses it through a raw pointer
    /// too, so no `Box` (which would claim it is unaliased) holds it.
    rt: *mut Realtime,
    streaming: bool,
    /// The node whose removal ends the stream.
    watch: Option<u32>,
}

impl Drop for StreamState {
    fn drop(&mut self) {
        // SAFETY: from `Box::into_raw`, freed once. The state is dropped after the
        // context, and with it the data loop that ran `process`, was destroyed, so
        // nothing else uses it.
        drop(unsafe { Box::from_raw(self.rt) });
    }
}

impl StreamState {
    fn rt(&self) -> &Realtime {
        // SAFETY: alive until `self` drops, and only ever used through `&`.
        unsafe { &*self.rt }
    }

    /// Tells the handler the stream failed, once.
    fn fail(&self, error: &PlatformError) {
        self.rt().fail(error);
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
        let rt = self.rt();
        let layout = spa_audio::layout_for(u32::try_from(rt.channels).ok());
        match spa_audio::parse_format(pod) {
            Some(f) if spa_audio::is_requested(f, rt.rate, layout) => {
                rt.settled.store(true, Ordering::Release);
            }
            other => self.fail(&PlatformError::Unsupported {
                what: format!(
                    "PipeWire's format for {} ({other:?}), not the f32 at {} Hz asked for",
                    rt.what, rt.rate
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
                self.fail(&PlatformError::DeviceLost {
                    what: self.rt().what.clone(),
                    reason: format!("PipeWire stream error: {reason}"),
                });
            }
            STATE_UNCONNECTED if self.streaming => self.fail(&PlatformError::DeviceLost {
                what: self.rt().what.clone(),
                reason: "the PipeWire stream disconnected".into(),
            }),
            _ => {}
        }
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
    // SAFETY: registered with the state (see `with`), on the thread loop.
    unsafe { with_stream(data, |s| s.state_changed(state, error)) };
}

unsafe extern "C" fn on_stream_param(data: *mut c_void, id: u32, param: *const SpaPod) {
    // SAFETY: registered with the state (see `with`), on the thread loop.
    unsafe { with_stream(data, |s| s.param_changed(id, param)) };
}

/// The stream's events on the connection's thread loop: everything but `process`.
static STREAM_EVENTS: PwStreamEvents = PwStreamEvents {
    version: PW_VERSION_STREAM_EVENTS,
    destroy: None,
    state_changed: Some(on_stream_state),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_stream_param),
    add_buffer: None,
    remove_buffer: None,
    process: None,
    drained: None,
    command: None,
    trigger_done: None,
};

unsafe extern "C" fn on_stream_process(data: *mut c_void) {
    // SAFETY: registered with the stream's `Realtime`, which outlives the stream, and
    // only used through `&` (see `Realtime`). `process` catches the handler's panics;
    // this catches the rest (R-UNSAFE-005).
    let rt = unsafe { &*data.cast::<Realtime>() };
    let _ = catch_unwind(AssertUnwindSafe(|| rt.process()));
}

/// The stream's `process` event alone, on PipeWire's data loop (`RT_PROCESS`).
static PROCESS_EVENTS: PwStreamEvents = PwStreamEvents {
    version: PW_VERSION_STREAM_EVENTS,
    destroy: None,
    state_changed: None,
    control_info: None,
    io_changed: None,
    param_changed: None,
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
    /// The `process` listener's hook (its data is the stream's `Realtime`).
    rt_hook: Box<SpaHook>,
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
            rt_hook: SpaHook::boxed(),
            format,
        };
        let locked = this.conn.lock();
        let connected = this.connect_locked(plan, handler);
        drop(locked);
        connected.map(|()| this)
    }

    fn connect_locked(&mut self, plan: &Plan, handler: Open) -> Result<(), PlatformError> {
        let fns = self.conn.fns;
        let (direction, category, name, handler, output) = match handler {
            Open::Input(h) => (
                PW_DIRECTION_INPUT,
                "Capture",
                c"JStream input",
                Handler::Input(h),
                false,
            ),
            Open::Output(h) => (
                PW_DIRECTION_OUTPUT,
                "Playback",
                c"JStream output",
                Handler::Output(h),
                true,
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
        let rt = Box::into_raw(Box::new(Realtime {
            stream: self.stream,
            output,
            rate: plan.rate,
            channels,
            what: plan.what.clone(),
            clock,
            fns,
            gate: Gate::default(),
            settled: AtomicBool::new(false),
            exclusive: UnsafeCell::new(Exclusive {
                handler,
                position: 0,
                scratch: vec![0.0; SCRATCH_FRAMES * channels],
                discontinuity: false,
            }),
        }));
        // SAFETY: the lock is held, so the loop's thread isn't using the state; the
        // stream state owns `rt` from here.
        unsafe {
            (*self.conn.state).stream = Some(StreamState {
                rt,
                streaming: false,
                watch: plan.watch,
            });
        }
        let data: *mut c_void = self.conn.state.cast();
        // SAFETY: links `hook` (boxed, outlives the stream, which `Drop` destroys first)
        // to the static `STREAM_EVENTS`, called with the state on the thread loop; and
        // `rt_hook` to `PROCESS_EVENTS`, called with `rt` (freed only with the state,
        // after the stream is destroyed) on the data loop.
        unsafe {
            (fns.stream_add_listener)(
                self.stream,
                &raw mut *self.hook,
                &raw const STREAM_EVENTS,
                data,
            );
            (fns.stream_add_listener)(
                self.stream,
                &raw mut *self.rt_hook,
                &raw const PROCESS_EVENTS,
                rt.cast(),
            );
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
                FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS | FLAG_RT_PROCESS,
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
        // SAFETY: under the loop's lock, so the thread loop isn't using the state.
        // Stopping first means `process` won't call the handler again, and that
        // disconnecting (a state change, not the stream failing) doesn't tell it.
        // Destroying the stream removes its listeners. The handler and the stream's
        // `Realtime` stay in the state until the connection, dropped next, has stopped
        // the thread loop and destroyed the context, which stops its data loop.
        unsafe {
            if let Some(stream) = (*self.conn.state).stream.as_ref() {
                stream.rt().stop();
            }
            (fns.stream_disconnect)(self.stream);
            (fns.stream_destroy)(self.stream);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::Gate;

    #[test]
    fn process_enters_until_the_stream_fails() {
        let gate = Gate::default();
        assert!(gate.enter());
        gate.leave();
        assert!(gate.enter());
        gate.leave();
        assert!(gate.stop(), "the first to fail it tells the handler");
        assert!(!gate.enter());
        assert!(!gate.stop() && !gate.fail(), "only once");
    }

    #[test]
    fn a_panicking_process_fails_the_stream_from_inside() {
        let gate = Gate::default();
        assert!(gate.enter());
        assert!(gate.fail(), "process tells the handler itself");
        gate.leave();
        assert!(!gate.stop(), "and nobody else does");
        assert!(!gate.enter());
    }

    #[test]
    fn stopping_waits_for_a_running_process_to_leave() {
        let gate = Arc::new(Gate::default());
        let left = Arc::new(AtomicBool::new(false));
        assert!(gate.enter());
        let stopper = {
            let (gate, left) = (Arc::clone(&gate), Arc::clone(&left));
            std::thread::spawn(move || {
                let first = gate.stop();
                (first, left.load(Ordering::SeqCst))
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !stopper.is_finished(),
            "stop returned while process was inside"
        );
        left.store(true, Ordering::SeqCst);
        gate.leave();
        let (first, saw_leave) = stopper.join().expect("the stopping thread");
        assert!(first && saw_leave);
        assert!(!gate.enter());
    }
}
