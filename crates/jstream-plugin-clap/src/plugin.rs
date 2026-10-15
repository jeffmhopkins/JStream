//! One plugin instance on the host's main thread.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap_sys::ext::audio_ports::{
    CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_IS_BYPASS, CLAP_PARAM_IS_ENUM, CLAP_PARAM_IS_HIDDEN,
    CLAP_PARAM_IS_READONLY, CLAP_PARAM_IS_STEPPED, clap_param_info, clap_plugin_params,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::plugin::clap_plugin;
use clap_sys::stream::{clap_istream, clap_ostream};

use crate::host::HostData;
use crate::library::c_id;
use crate::process::Processor;
use crate::{ClapError, Library, MAX_STATE_BYTES, PluginInfo, strings};

/// The most parameters read from one plugin. Only bounds a broken plugin.
const MAX_PARAMS: u32 = 16_384;

/// One of a plugin's parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    /// The plugin's id for it, which [`ParamEvent`](crate::ParamEvent)s carry.
    pub id: u32,
    /// Its name.
    pub name: String,
    /// The group it belongs to (`"Filter/Envelope"`), or empty.
    pub module: String,
    /// Its lowest value.
    pub min: f64,
    /// Its highest value.
    pub max: f64,
    /// Its default value.
    pub default: f64,
    /// Whole steps only.
    pub stepped: bool,
    /// One of a set of named values (stepped too).
    pub is_enum: bool,
    /// Not shown to the user.
    pub hidden: bool,
    /// Read-only: the plugin reports it, nobody sets it.
    pub read_only: bool,
    /// The plugin's own bypass switch.
    pub bypass: bool,
}

/// The plugin instance itself, shared by the [`Plugin`] and its [`Processor`].
pub(crate) struct Inner {
    pub(crate) plugin: *const clap_plugin,
    /// Whether `activate` succeeded and `deactivate` hasn't run.
    pub(crate) active: AtomicBool,
    pub(crate) host: Box<HostData>,
    latency: Option<*const clap_plugin_latency>,
    params: Option<*const clap_plugin_params>,
    state: Option<*const clap_plugin_state>,
    ports: Option<*const clap_plugin_audio_ports>,
    /// Kept loaded until the instance is destroyed.
    _library: Arc<Library>,
}

// SAFETY: CLAP plugins are called from a main thread and an audio thread; `Inner` only
// holds the instance and its extension tables, which are plain pointers valid until
// `destroy`. The `Plugin` keeps main-thread calls on its thread (it is not `Sync`), and
// the `Processor` makes the audio-thread calls.
unsafe impl Send for Inner {}
// SAFETY: as for `Send`: the shared fields are pointers to tables the plugin owns and
// atomics; which thread calls what is kept by `Plugin` and `Processor`.
unsafe impl Sync for Inner {}

impl Inner {
    pub(crate) fn raw(&self) -> &clap_plugin {
        // SAFETY: `plugin` is non-null (checked at creation) and valid until `destroy`,
        // which only `Drop` calls.
        unsafe { &*self.plugin }
    }

    /// Extension `id` as a `T` table, if the plugin has it.
    fn extension<T>(plugin: &clap_plugin, id: &std::ffi::CStr) -> Option<*const T> {
        let get = plugin.get_extension?;
        // SAFETY: `get_extension` takes the instance and a NUL-terminated id.
        let ext = unsafe { get(plugin, id.as_ptr()) };
        (!ext.is_null()).then_some(ext.cast::<T>())
    }

    /// Deactivates the plugin if it is active. Main thread, with no processor left.
    fn deactivate(&self) {
        if self.active.swap(false, Ordering::AcqRel)
            && let Some(deactivate) = self.raw().deactivate
        {
            // SAFETY: the plugin is active and nothing processes it any more.
            unsafe { deactivate(self.plugin) };
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.deactivate();
        if let Some(destroy) = self.raw().destroy {
            // SAFETY: the last reference to the instance: destroy it once.
            unsafe { destroy(self.plugin) };
        }
    }
}

/// A plugin instance, on the host's main thread: create it, read its parameters and
/// state, then [`activate`](Self::activate) it to get its [`Processor`].
///
/// Dropping it destroys the instance once its [`Processor`] is gone too. Not `Sync`:
/// CLAP's main-thread calls stay on the thread that owns it.
pub struct Plugin {
    inner: Arc<Inner>,
    info: PluginInfo,
    /// Keeps `Plugin` `!Sync` (main-thread calls from one thread at a time).
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("id", &self.info.id)
            .field("active", &self.is_active())
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// Creates and initialises plugin `id` from `library`.
    ///
    /// # Errors
    /// [`ClapError::NoFactory`] or [`ClapError::NoSuchPlugin`] when the file doesn't
    /// offer it, [`ClapError::NotAnEffect`] when it isn't an audio effect, and
    /// [`ClapError::Create`] when the factory or the plugin's `init` fails.
    pub fn new(library: &Arc<Library>, id: &str) -> Result<Self, ClapError> {
        let info = library.find(id)?;
        if !info.is_audio_effect() {
            return Err(ClapError::NotAnEffect);
        }
        let factory = library.factory().ok_or(ClapError::NoFactory)?;
        let create = factory.create_plugin.ok_or(ClapError::Create)?;
        let host = HostData::new();
        let id_c = c_id(id)?;
        // SAFETY: the factory is thread-safe; `host` is boxed and outlives the instance
        // (both live in `Inner`); `id_c` is NUL-terminated.
        let plugin = unsafe { create(factory, &host.clap, id_c.as_ptr()) };
        // SAFETY: a non-null result is a live instance until `destroy`.
        let raw = unsafe { plugin.as_ref() }.ok_or(ClapError::Create)?;
        let destroy = |raw: &clap_plugin| {
            if let Some(destroy) = raw.destroy {
                // SAFETY: the instance was created and is destroyed once, here.
                unsafe { destroy(raw) };
            }
        };
        let Some(init) = raw.init else {
            destroy(raw);
            return Err(ClapError::Create);
        };
        // SAFETY: `init` is the first call after creation, on the main thread.
        if !unsafe { init(raw) } {
            destroy(raw);
            return Err(ClapError::Create);
        }
        let inner = Inner {
            latency: Inner::extension(raw, CLAP_EXT_LATENCY),
            params: Inner::extension(raw, CLAP_EXT_PARAMS),
            state: Inner::extension(raw, CLAP_EXT_STATE),
            ports: Inner::extension(raw, CLAP_EXT_AUDIO_PORTS),
            plugin,
            active: AtomicBool::new(false),
            host,
            _library: Arc::clone(library),
        };
        Ok(Self {
            inner: Arc::new(inner),
            info,
            _not_sync: std::marker::PhantomData,
        })
    }

    /// The plugin's descriptor.
    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    /// Whether it is active.
    pub fn is_active(&self) -> bool {
        self.inner.active.load(Ordering::Acquire)
    }

    /// Activates the plugin at `sample_rate` for blocks of 1 to `max_frames` frames, and
    /// returns its [`Processor`] for the audio thread. Checks first that its main input
    /// and main output are stereo.
    ///
    /// # Errors
    /// [`ClapError::WrongState`] when it is already active, [`ClapError::NotStereo`],
    /// and [`ClapError::Activate`] when the plugin refuses.
    pub fn activate(&mut self, sample_rate: u32, max_frames: u32) -> Result<Processor, ClapError> {
        if self.is_active() {
            return Err(ClapError::WrongState("already active"));
        }
        if !self.stereo_main_ports() {
            return Err(ClapError::NotStereo);
        }
        let activate = self.inner.raw().activate.ok_or(ClapError::Activate)?;
        if max_frames == 0 {
            return Err(ClapError::Activate);
        }
        // SAFETY: main thread, inactive, with a positive block size.
        if !unsafe { activate(self.inner.plugin, f64::from(sample_rate), 1, max_frames) } {
            return Err(ClapError::Activate);
        }
        self.inner.active.store(true, Ordering::Release);
        Ok(Processor::new(Arc::clone(&self.inner), max_frames as usize))
    }

    /// Deactivates the plugin. Its [`Processor`] must be dropped first (on the audio
    /// thread, which stops processing).
    ///
    /// # Errors
    /// [`ClapError::Busy`] while the processor exists.
    pub fn deactivate(&mut self) -> Result<(), ClapError> {
        if Arc::strong_count(&self.inner) > 1 {
            return Err(ClapError::Busy);
        }
        self.inner.deactivate();
        Ok(())
    }

    /// The plugin's latency, frames: 0 when it has no latency extension. CLAP only
    /// answers while the plugin is active.
    ///
    /// # Errors
    /// [`ClapError::WrongState`] when it isn't active.
    pub fn latency(&self) -> Result<u32, ClapError> {
        if !self.is_active() {
            return Err(ClapError::WrongState("not active"));
        }
        let Some(get) = self
            .inner
            .latency
            // SAFETY: the table is valid until `destroy`.
            .and_then(|ext| unsafe { (*ext).get })
        else {
            return Ok(0);
        };
        // SAFETY: main thread, active.
        Ok(unsafe { get(self.inner.plugin) })
    }

    /// The plugin's parameters, in its order. Empty when it has none.
    pub fn params(&self) -> Vec<ParamInfo> {
        let Some(ext) = self.inner.params else {
            return Vec::new();
        };
        // SAFETY: the table is valid until `destroy`.
        let ext = unsafe { &*ext };
        let (Some(count), Some(get_info)) = (ext.count, ext.get_info) else {
            return Vec::new();
        };
        // SAFETY: main thread.
        let n = unsafe { count(self.inner.plugin) }.min(MAX_PARAMS);
        (0..n)
            .filter_map(|i| {
                // SAFETY: an all-zero `clap_param_info` is valid (null cookie, empty
                // names, zero numbers); the plugin overwrites it.
                let mut info: clap_param_info = unsafe { std::mem::zeroed() };
                // SAFETY: `i` is below the count; `info` is writable.
                if !unsafe { get_info(self.inner.plugin, i, &mut info) } {
                    return None;
                }
                let flag = |f| info.flags & f != 0;
                Some(ParamInfo {
                    id: info.id,
                    name: strings::from_array(&info.name),
                    module: strings::from_array(&info.module),
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                    stepped: flag(CLAP_PARAM_IS_STEPPED),
                    is_enum: flag(CLAP_PARAM_IS_ENUM),
                    hidden: flag(CLAP_PARAM_IS_HIDDEN),
                    read_only: flag(CLAP_PARAM_IS_READONLY),
                    bypass: flag(CLAP_PARAM_IS_BYPASS),
                })
            })
            .collect()
    }

    /// Parameter `id`'s current value, if the plugin has it.
    pub fn param_value(&self, id: u32) -> Option<f64> {
        // SAFETY: the table is valid until `destroy`.
        let get = self
            .inner
            .params
            .and_then(|ext| unsafe { (*ext).get_value })?;
        let mut value = 0.0;
        // SAFETY: main thread; `value` is writable.
        unsafe { get(self.inner.plugin, id, &mut value) }.then_some(value)
    }

    /// The plugin's saved state, at most [`MAX_STATE_BYTES`].
    ///
    /// # Errors
    /// [`ClapError::NoState`] without the state extension, [`ClapError::StateTooLarge`]
    /// past the limit, and [`ClapError::State`] when the plugin fails.
    pub fn save_state(&self) -> Result<Vec<u8>, ClapError> {
        // SAFETY: the table is valid until `destroy`.
        let save = self
            .inner
            .state
            .and_then(|ext| unsafe { (*ext).save })
            .ok_or(ClapError::NoState)?;
        let mut sink = Sink {
            bytes: Vec::new(),
            overflow: false,
        };
        let stream = clap_ostream {
            ctx: (&raw mut sink).cast(),
            write: Some(sink_write),
        };
        // SAFETY: main thread; `stream` and `sink` outlive the call.
        let ok = unsafe { save(self.inner.plugin, &stream) };
        if sink.overflow {
            return Err(ClapError::StateTooLarge);
        }
        if !ok {
            return Err(ClapError::State);
        }
        Ok(sink.bytes)
    }

    /// Restores a state [`save_state`](Self::save_state) returned, from this plugin or
    /// an earlier run of it.
    ///
    /// # Errors
    /// [`ClapError::NoState`] without the state extension, [`ClapError::StateTooLarge`]
    /// for more than [`MAX_STATE_BYTES`], and [`ClapError::State`] when the plugin
    /// rejects it.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), ClapError> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(ClapError::StateTooLarge);
        }
        // SAFETY: the table is valid until `destroy`.
        let load = self
            .inner
            .state
            .and_then(|ext| unsafe { (*ext).load })
            .ok_or(ClapError::NoState)?;
        let mut source = Source { bytes, read: 0 };
        let stream = clap_istream {
            ctx: (&raw mut source).cast(),
            read: Some(source_read),
        };
        // SAFETY: main thread; `stream` and `source` outlive the call.
        if unsafe { load(self.inner.plugin, &stream) } {
            Ok(())
        } else {
            Err(ClapError::State)
        }
    }

    /// Runs the plugin's `on_main_thread` if it asked for it since the last call. The
    /// host's main loop calls this.
    pub fn run_main_thread_callback(&mut self) {
        if self
            .inner
            .host
            .requests
            .callback
            .swap(false, Ordering::AcqRel)
            && let Some(cb) = self.inner.raw().on_main_thread
        {
            // SAFETY: main thread, as requested.
            unsafe { cb(self.inner.plugin) };
        }
    }

    /// Whether the plugin asked to be restarted (deactivated and activated again, for a
    /// new latency or port layout) since the last call.
    pub fn take_restart_request(&self) -> bool {
        let r = &self.inner.host.requests;
        let restart = r.restart.swap(false, Ordering::AcqRel);
        let latency = r.latency.swap(false, Ordering::AcqRel);
        restart || latency
    }

    /// Whether the plugin marked its state changed since the last call.
    pub fn take_state_dirty(&self) -> bool {
        self.inner.host.requests.dirty.swap(false, Ordering::AcqRel)
    }

    #[cfg(test)]
    pub(crate) fn inner_requests_callback(&self) -> bool {
        self.inner.host.requests.callback.load(Ordering::Acquire)
    }

    /// Whether the main input and main output both exist and have two channels.
    fn stereo_main_ports(&self) -> bool {
        let Some(ext) = self.inner.ports else {
            return false;
        };
        // SAFETY: the table is valid until `destroy`.
        let ext = unsafe { &*ext };
        let (Some(count), Some(get)) = (ext.count, ext.get) else {
            return false;
        };
        let main_is_stereo = |is_input: bool| {
            // SAFETY: main thread, inactive.
            if unsafe { count(self.inner.plugin, is_input) } == 0 {
                return false;
            }
            // SAFETY: an all-zero port info is valid (null port type, zero numbers).
            let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
            // SAFETY: index 0 is below the count; `info` is writable.
            let got = unsafe { get(self.inner.plugin, 0, is_input, &mut info) };
            got && info.flags & CLAP_AUDIO_PORT_IS_MAIN != 0 && info.channel_count == 2
        };
        main_is_stereo(true) && main_is_stereo(false)
    }
}

/// Where `save_state` collects the plugin's bytes.
struct Sink {
    bytes: Vec<u8>,
    overflow: bool,
}

unsafe extern "C" fn sink_write(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `ctx` is the `Sink` `save_state` passed, alive for the call.
        let Some(sink) =
            (unsafe { stream.as_ref() }).and_then(|s| unsafe { s.ctx.cast::<Sink>().as_mut() })
        else {
            return -1;
        };
        let Ok(size) = usize::try_from(size) else {
            return -1;
        };
        if sink.bytes.len() + size > MAX_STATE_BYTES {
            sink.overflow = true;
            return -1;
        }
        if size == 0 {
            return 0;
        }
        if buffer.is_null() {
            return -1;
        }
        // SAFETY: the plugin gives `size` readable bytes at `buffer`.
        let bytes = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size) };
        sink.bytes.extend_from_slice(bytes);
        size as i64
    }))
    .unwrap_or(-1)
}

/// Where `load_state` feeds the plugin from.
struct Source<'a> {
    bytes: &'a [u8],
    read: usize,
}

unsafe extern "C" fn source_read(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `ctx` is the `Source` `load_state` passed, alive for the call.
        let Some(source) = (unsafe { stream.as_ref() })
            .and_then(|s| unsafe { s.ctx.cast::<Source<'_>>().as_mut() })
        else {
            return -1;
        };
        let left = &source.bytes[source.read..];
        let n = left.len().min(usize::try_from(size).unwrap_or(usize::MAX));
        if n == 0 {
            return 0;
        }
        if buffer.is_null() {
            return -1;
        }
        // SAFETY: the plugin gives `size` writable bytes at `buffer`, and `n <= size`.
        unsafe { std::ptr::copy_nonoverlapping(left.as_ptr(), buffer.cast::<u8>(), n) };
        source.read += n;
        n as i64
    }))
    .unwrap_or(-1)
}
