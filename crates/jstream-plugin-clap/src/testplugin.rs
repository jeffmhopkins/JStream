//! A CLAP plugin file built into the test binary: a factory with a stereo gain effect,
//! an instrument and a mono effect, written against the raw ABI the way a C plugin
//! would be, so the wrapper is tested against the ABI rather than against itself.
//!
//! The gain effect: parameter 7 ("Gain", 0 to 2, default 1) scales the audio; for every
//! change of parameter 7 it receives, it reports parameter 8 with the same value (the
//! way a plugin's own editor reports a change); it reports 32 frames of latency; its
//! state is the gain's 8 bytes, written in two parts (a gain of exactly 42 writes 3 MiB
//! instead, to test the size limit); at `init` it asks the host for its latency
//! extension and for a main-thread callback.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_VALUE, clap_event_header, clap_event_param_value,
};
use clap_sys::ext::audio_ports::{
    CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, clap_param_info, clap_plugin_params,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::{CLAP_PROCESS_CONTINUE, clap_process, clap_process_status};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;

pub(crate) const GAIN_ID: &str = "org.jstream.test.gain";
pub(crate) const SYNTH_ID: &str = "org.jstream.test.synth";
pub(crate) const MONO_ID: &str = "org.jstream.test.mono";

/// Calls to `on_main_thread`, across every instance.
pub(crate) static MAIN_THREAD_CALLS: AtomicU32 = AtomicU32::new(0);
/// Calls to `reset`, across every instance.
pub(crate) static RESETS: AtomicU32 = AtomicU32::new(0);
/// Calls to `stop_processing`, across every instance.
pub(crate) static STOPS: AtomicU32 = AtomicU32::new(0);

/// A null-terminated list of feature strings.
struct Features<const N: usize>([*const c_char; N]);
// SAFETY: pointers to static string literals, never written.
unsafe impl<const N: usize> Sync for Features<N> {}

static EFFECT_FEATURES: Features<3> = Features([
    c"audio-effect".as_ptr(),
    c"stereo".as_ptr(),
    std::ptr::null(),
]);
static SYNTH_FEATURES: Features<2> = Features([c"instrument".as_ptr(), std::ptr::null()]);

const fn descriptor(
    id: &'static CStr,
    name: &'static CStr,
    features: *const *const c_char,
) -> clap_plugin_descriptor {
    clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: id.as_ptr(),
        name: name.as_ptr(),
        vendor: c"JStream tests".as_ptr(),
        url: std::ptr::null(),
        manual_url: std::ptr::null(),
        support_url: std::ptr::null(),
        version: c"1.0".as_ptr(),
        description: std::ptr::null(),
        features,
    }
}

static DESCRIPTORS: [clap_plugin_descriptor; 3] = [
    descriptor(
        c"org.jstream.test.gain",
        c"Test gain",
        EFFECT_FEATURES.0.as_ptr(),
    ),
    descriptor(
        c"org.jstream.test.synth",
        c"Test synth",
        SYNTH_FEATURES.0.as_ptr(),
    ),
    descriptor(
        c"org.jstream.test.mono",
        c"Test mono",
        EFFECT_FEATURES.0.as_ptr(),
    ),
];

pub(crate) static ENTRY: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(factory_descriptor),
    create_plugin: Some(factory_create),
};

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}

unsafe extern "C" fn entry_deinit() {}

unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    // SAFETY: the host passes a NUL-terminated id.
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        (&raw const FACTORY).cast()
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
    DESCRIPTORS.len() as u32
}

unsafe extern "C" fn factory_descriptor(
    _f: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    DESCRIPTORS
        .get(index as usize)
        .map_or(std::ptr::null(), |d| d)
}

/// One instance: the `clap_plugin` the host holds, first, and its state.
#[repr(C)]
struct Instance {
    clap: clap_plugin,
    host: *const clap_host,
    channels: u32,
    gain: f64,
}

unsafe extern "C" fn factory_create(
    _f: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    // SAFETY: the host passes a NUL-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    let Some(desc) = DESCRIPTORS
        .iter()
        // SAFETY: descriptor ids are static literals.
        .find(|d| unsafe { CStr::from_ptr(d.id) } == id)
    else {
        return std::ptr::null();
    };
    let instance = Box::new(Instance {
        clap: clap_plugin {
            desc,
            plugin_data: std::ptr::null_mut(),
            init: Some(plugin_init),
            destroy: Some(plugin_destroy),
            activate: Some(plugin_activate),
            deactivate: Some(plugin_deactivate),
            start_processing: Some(plugin_start),
            stop_processing: Some(plugin_stop),
            reset: Some(plugin_reset),
            process: Some(plugin_process),
            get_extension: Some(plugin_extension),
            on_main_thread: Some(plugin_main_thread),
        },
        host,
        channels: if id.to_bytes() == MONO_ID.as_bytes() {
            1
        } else {
            2
        },
        gain: 1.0,
    });
    // `clap` is the first field of a `repr(C)` struct, so the instance's address is
    // the `clap_plugin`'s.
    Box::into_raw(instance).cast()
}

/// The instance behind `plugin`.
///
/// # Safety
/// `plugin` came from `factory_create` and isn't destroyed.
unsafe fn instance<'a>(plugin: *const clap_plugin) -> &'a mut Instance {
    // SAFETY: per the caller; the host never calls one instance from two threads at
    // once in these tests.
    unsafe { &mut *plugin.cast::<Instance>().cast_mut() }
}

unsafe extern "C" fn plugin_init(plugin: *const clap_plugin) -> bool {
    // SAFETY: called by the host on a live instance with the host it was created with.
    unsafe {
        let host = &*instance(plugin).host;
        let get = host.get_extension.expect("host offers get_extension");
        assert!(!get(host, CLAP_EXT_LATENCY.as_ptr()).is_null());
        (host.request_callback.expect("host offers request_callback"))(host);
    }
    true
}

unsafe extern "C" fn plugin_destroy(plugin: *const clap_plugin) {
    // SAFETY: the instance came from `Box::into_raw` and is destroyed once.
    drop(unsafe { Box::from_raw(plugin.cast::<Instance>().cast_mut()) });
}

unsafe extern "C" fn plugin_activate(
    _plugin: *const clap_plugin,
    rate: f64,
    min: u32,
    max: u32,
) -> bool {
    rate > 0.0 && min >= 1 && max >= min
}

unsafe extern "C" fn plugin_deactivate(_plugin: *const clap_plugin) {}

unsafe extern "C" fn plugin_start(_plugin: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn plugin_stop(_plugin: *const clap_plugin) {
    STOPS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn plugin_reset(_plugin: *const clap_plugin) {
    RESETS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn plugin_main_thread(_plugin: *const clap_plugin) {
    MAIN_THREAD_CALLS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn plugin_process(
    plugin: *const clap_plugin,
    process: *const clap_process,
) -> clap_process_status {
    // SAFETY: the host passes a valid process block for a live, active instance.
    unsafe {
        let me = instance(plugin);
        let p = &*process;
        let events = &*p.in_events;
        let out_events = &*p.out_events;
        for i in 0..(events.size.expect("size"))(events) {
            let header = &*(events.get.expect("get"))(events, i);
            if header.space_id == CLAP_CORE_EVENT_SPACE_ID && header.type_ == CLAP_EVENT_PARAM_VALUE
            {
                let ev = &*(header as *const clap_event_header).cast::<clap_event_param_value>();
                if ev.param_id == 7 {
                    me.gain = ev.value;
                    let mut echo = *ev;
                    echo.param_id = 8;
                    (out_events.try_push.expect("try_push"))(out_events, &echo.header);
                }
            }
        }
        let input: &clap_audio_buffer = &*p.audio_inputs;
        let output: &clap_audio_buffer = &*p.audio_outputs;
        for c in 0..2 {
            let src = *input.data32.add(c);
            let dst = *output.data32.add(c);
            for n in 0..p.frames_count as usize {
                *dst.add(n) = (f64::from(*src.add(n)) * me.gain) as f32;
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

static PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(ports_count),
    get: Some(ports_get),
};
static LATENCY: clap_plugin_latency = clap_plugin_latency {
    get: Some(latency_get),
};
static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_info),
    get_value: Some(params_value),
    value_to_text: None,
    text_to_value: None,
    flush: None,
};
static STATE: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};

unsafe extern "C" fn plugin_extension(
    _plugin: *const clap_plugin,
    id: *const c_char,
) -> *const c_void {
    // SAFETY: the host passes a NUL-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_AUDIO_PORTS {
        (&raw const PORTS).cast()
    } else if id == CLAP_EXT_LATENCY {
        (&raw const LATENCY).cast()
    } else if id == CLAP_EXT_PARAMS {
        (&raw const PARAMS).cast()
    } else if id == CLAP_EXT_STATE {
        (&raw const STATE).cast()
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn ports_count(_plugin: *const clap_plugin, _is_input: bool) -> u32 {
    1
}

unsafe extern "C" fn ports_get(
    plugin: *const clap_plugin,
    index: u32,
    _is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    if index != 0 {
        return false;
    }
    // SAFETY: a live instance and a writable info, from the host.
    unsafe {
        let info = &mut *info;
        info.id = 0;
        info.flags = CLAP_AUDIO_PORT_IS_MAIN;
        info.channel_count = instance(plugin).channels;
        info.port_type = std::ptr::null();
    }
    true
}

unsafe extern "C" fn latency_get(_plugin: *const clap_plugin) -> u32 {
    32
}

unsafe extern "C" fn params_count(_plugin: *const clap_plugin) -> u32 {
    1
}

unsafe extern "C" fn params_info(
    _plugin: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    if index != 0 {
        return false;
    }
    // SAFETY: a writable info, from the host.
    let info = unsafe { &mut *info };
    info.id = 7;
    info.flags = CLAP_PARAM_IS_AUTOMATABLE;
    for (dst, src) in info.name.iter_mut().zip(b"Gain\0") {
        *dst = *src as c_char;
    }
    info.module[0] = 0;
    info.min_value = 0.0;
    info.max_value = 2.0;
    info.default_value = 1.0;
    true
}

unsafe extern "C" fn params_value(plugin: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    if id != 7 {
        return false;
    }
    // SAFETY: a live instance and a writable value, from the host.
    unsafe { *out = instance(plugin).gain };
    true
}

unsafe extern "C" fn state_save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    // SAFETY: a live instance and a valid stream, from the host.
    unsafe {
        let s = &*stream;
        let write = s.write.expect("write");
        let gain = instance(plugin).gain;
        if gain == 42.0 {
            let mib = vec![0u8; 1024 * 1024];
            for _ in 0..3 {
                if write(s, mib.as_ptr().cast(), mib.len() as u64) < 0 {
                    return false;
                }
            }
            return true;
        }
        let bytes = gain.to_le_bytes();
        write(s, bytes.as_ptr().cast(), 4) == 4 && write(s, bytes[4..].as_ptr().cast(), 4) == 4
    }
}

unsafe extern "C" fn state_load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    // SAFETY: a live instance and a valid stream, from the host.
    unsafe {
        let s = &*stream;
        let read = s.read.expect("read");
        let mut bytes = [0u8; 8];
        let mut got = 0;
        while got < 8 {
            let n = read(s, bytes[got..].as_mut_ptr().cast(), (8 - got) as u64);
            if n <= 0 {
                return false;
            }
            got += n as usize;
        }
        instance(plugin).gain = f64::from_le_bytes(bytes);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClapError, Library, ParamEvent, Plugin};

    fn gain() -> Plugin {
        let lib = Library::from_static(&ENTRY).unwrap();
        Plugin::new(&lib, GAIN_ID).unwrap()
    }

    #[test]
    fn only_stereo_audio_effects_are_hosted() {
        let lib = Library::from_static(&ENTRY).unwrap();
        assert_eq!(
            Plugin::new(&lib, SYNTH_ID).unwrap_err(),
            ClapError::NotAnEffect
        );
        let mut mono = Plugin::new(&lib, MONO_ID).unwrap();
        assert_eq!(
            mono.activate(48_000, 256).unwrap_err(),
            ClapError::NotStereo
        );
        assert!(!mono.is_active());
    }

    #[test]
    fn reads_params_and_latency() {
        let mut plugin = gain();
        let params = plugin.params();
        assert_eq!(params.len(), 1);
        let p = &params[0];
        assert_eq!((p.id, p.name.as_str()), (7, "Gain"));
        assert_eq!((p.min, p.max, p.default), (0.0, 2.0, 1.0));
        assert!(!p.stepped && !p.hidden && !p.read_only);
        assert_eq!(plugin.param_value(7), Some(1.0));
        assert_eq!(plugin.param_value(99), None);
        // Latency is only answered while active.
        assert!(plugin.latency().is_err());
        let processor = plugin.activate(48_000, 256).unwrap();
        assert_eq!(plugin.latency(), Ok(32));
        drop(processor);
        plugin.deactivate().unwrap();
    }

    #[test]
    fn processes_blocks_with_parameter_changes() {
        let mut plugin = gain();
        let mut processor = plugin.activate(48_000, 64).unwrap();
        let input: Vec<f32> = (0..128).map(|i| i as f32).collect();
        let mut output = vec![0.0; 128];
        // Unity gain: a copy, and nothing reported.
        let reported = processor.process(&input, &mut output, &[]).unwrap();
        assert!(reported.is_empty());
        assert_eq!(output, input);
        // Gain 0.5 from this block on; the plugin reports parameter 8.
        let change = ParamEvent { id: 7, value: 0.5 };
        let reported = processor
            .process(&input, &mut output, &[change])
            .unwrap()
            .to_vec();
        assert_eq!(reported, [ParamEvent { id: 8, value: 0.5 }]);
        assert!(output.iter().zip(&input).all(|(o, i)| *o == i * 0.5));
        // Channels stay apart: left is even samples, right odd.
        assert_eq!((output[0], output[1]), (0.0, 0.5));
        // Shorter blocks are fine; longer, odd or uneven ones are refused.
        assert!(
            processor
                .process(&input[..32], &mut output[..32], &[])
                .is_ok()
        );
        let big = vec![0.0; 130];
        assert!(matches!(
            processor.process(&big, &mut big.clone(), &[]),
            Err(ClapError::BlockSize {
                frames: 65,
                max: 64
            })
        ));
        assert!(
            processor
                .process(&input[..3], &mut output[..3], &[])
                .is_err()
        );
        assert!(processor.process(&input, &mut output[..64], &[]).is_err());
        assert!(processor.process(&[], &mut [], &[]).is_err());
        // The value is the plugin's own, read on the main thread.
        assert_eq!(plugin.param_value(7), Some(0.5));
        let stops = STOPS.load(Ordering::Relaxed);
        let resets = RESETS.load(Ordering::Relaxed);
        processor.reset();
        assert!(RESETS.load(Ordering::Relaxed) > resets);
        // Busy until the processor is gone, which stops processing.
        assert_eq!(plugin.deactivate(), Err(ClapError::Busy));
        drop(processor);
        assert!(STOPS.load(Ordering::Relaxed) > stops);
        plugin.deactivate().unwrap();
        assert!(!plugin.is_active());
        // And it can be activated again.
        assert!(plugin.activate(44_100, 128).is_ok());
    }

    #[test]
    fn processing_does_not_allocate() {
        let mut plugin = gain();
        let mut processor = plugin.activate(48_000, 16).unwrap();
        let input = vec![1.0; 32];
        let mut output = vec![0.0; 32];
        // More events than a block carries: only MAX_BLOCK_EVENTS go in, so at most
        // that many echoes come back, and the lists don't grow.
        let flood: Vec<ParamEvent> = (0..crate::MAX_BLOCK_EVENTS + 10)
            .map(|_| ParamEvent { id: 7, value: 1.0 })
            .collect();
        // The first block starts processing; time the ones after it.
        processor.process(&input, &mut output, &[]).unwrap();
        let before = assert_no_alloc::violation_count();
        let reported = assert_no_alloc::assert_no_alloc(|| {
            processor
                .process(&input, &mut output, &flood)
                .map(<[ParamEvent]>::len)
        });
        assert_eq!(reported, Ok(crate::MAX_BLOCK_EVENTS));
        assert_eq!(assert_no_alloc::violation_count(), before);
    }

    #[test]
    fn saves_and_restores_state() {
        let mut plugin = gain();
        let mut processor = plugin.activate(48_000, 16).unwrap();
        processor
            .process(
                &[0.0; 32],
                &mut [0.0; 32],
                &[ParamEvent { id: 7, value: 1.25 }],
            )
            .unwrap();
        drop(processor);
        let state = plugin.save_state().unwrap();
        assert_eq!(state, 1.25f64.to_le_bytes());
        let mut other = gain();
        other.load_state(&state).unwrap();
        assert_eq!(other.param_value(7), Some(1.25));
        // Too short for the plugin, and too long for the host.
        assert_eq!(other.load_state(&state[..5]), Err(ClapError::State));
        assert_eq!(
            other.load_state(&vec![0; crate::MAX_STATE_BYTES + 1]),
            Err(ClapError::StateTooLarge)
        );
    }

    #[test]
    fn a_state_over_the_limit_is_refused() {
        let mut plugin = gain();
        plugin.load_state(&42.0f64.to_le_bytes()).unwrap();
        assert_eq!(plugin.save_state(), Err(ClapError::StateTooLarge));
    }

    #[test]
    fn main_thread_callbacks_run_when_requested() {
        let mut plugin = gain();
        // `init` asked for a callback.
        let before = MAIN_THREAD_CALLS.load(Ordering::Relaxed);
        plugin.run_main_thread_callback();
        assert!(MAIN_THREAD_CALLS.load(Ordering::Relaxed) > before);
        // The request is cleared until the plugin asks again.
        assert!(!plugin.inner_requests_callback());
        assert!(!plugin.take_restart_request());
        assert!(!plugin.take_state_dirty());
    }

    #[test]
    fn a_processor_can_move_to_another_thread() {
        let mut plugin = gain();
        let mut processor = plugin.activate(48_000, 16).unwrap();
        let processor = std::thread::spawn(move || {
            let mut out = [0.0; 32];
            processor.process(&[1.0; 32], &mut out, &[]).unwrap();
            assert_eq!(out, [1.0; 32]);
            processor
        })
        .join()
        .unwrap();
        drop(processor);
        plugin.deactivate().unwrap();
    }
}
