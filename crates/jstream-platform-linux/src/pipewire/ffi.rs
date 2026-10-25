//! The parts of libpipewire's and libspa's C API that the video and audio streams and
//! the registry share: struct layouts from the headers, and the function table resolved
//! from the loaded library (ADR-0060 §8: hand-written, no `bindgen`).
//!
//! The layouts are checked against the headers' sizes and offsets in this module's
//! tests, and the constants against their enums.

use std::ffi::{c_char, c_int, c_void};

use jstream_platform::PlatformError;

use crate::dl::SystemLibrary;

/// `struct spa_list` in <spa/utils/list.h>.
#[repr(C)]
pub(crate) struct SpaList {
    next: *mut SpaList,
    prev: *mut SpaList,
}

/// `struct spa_hook` in <spa/utils/hook.h>: a listener's storage, which PipeWire links
/// into an object's listener list. Zeroed before an `add_listener` call fills it.
#[repr(C)]
pub(crate) struct SpaHook {
    link: SpaList,
    funcs: *const c_void,
    data: *mut c_void,
    removed: Option<unsafe extern "C" fn(*mut SpaHook)>,
    priv_: *mut c_void,
}

impl SpaHook {
    /// A zeroed hook in a box, so it doesn't move once linked.
    pub(crate) fn boxed() -> Box<Self> {
        Box::new(Self {
            link: SpaList {
                next: std::ptr::null_mut(),
                prev: std::ptr::null_mut(),
            },
            funcs: std::ptr::null(),
            data: std::ptr::null_mut(),
            removed: None,
            priv_: std::ptr::null_mut(),
        })
    }

    /// Unlinks the hook from its listener list, as `spa_hook_remove` does, so it can be
    /// freed while the object it listens to lives on. A hook never linked is left as is.
    ///
    /// # Safety
    /// The hook is linked into a live list (or not linked at all), and nothing iterates
    /// that list meanwhile: the caller holds the loop that owns it.
    pub(crate) unsafe fn remove(&mut self) {
        if !self.link.next.is_null() {
            // SAFETY: the function's contract: `next` and `prev` are the list's live
            // neighbours of this link.
            unsafe {
                (*self.link.prev).next = self.link.next;
                (*self.link.next).prev = self.link.prev;
            }
            self.link.next = std::ptr::null_mut();
            self.link.prev = std::ptr::null_mut();
        }
        if let Some(removed) = self.removed {
            // SAFETY: the callback the listener's owner registered for this hook.
            unsafe { removed(self) };
        }
    }
}

/// `struct spa_callbacks` in <spa/utils/hook.h>.
#[repr(C)]
pub(crate) struct SpaCallbacks {
    pub(crate) funcs: *const c_void,
    pub(crate) data: *mut c_void,
}

/// `struct spa_interface` in <spa/utils/hook.h>: what a proxy (the core, the registry,
/// a bound metadata object) points to. Its methods are `cb.funcs`, a methods struct,
/// called with `cb.data` as their first argument: what libspa's inline
/// `spa_interface_call` macros do.
#[repr(C)]
pub(crate) struct SpaInterface {
    pub(crate) type_: *const c_char,
    pub(crate) version: u32,
    pub(crate) cb: SpaCallbacks,
}

/// `struct spa_dict_item` in <spa/utils/dict.h>.
#[repr(C)]
pub(crate) struct SpaDictItem {
    pub(crate) key: *const c_char,
    pub(crate) value: *const c_char,
}

/// `struct spa_dict` in <spa/utils/dict.h>.
#[repr(C)]
pub(crate) struct SpaDict {
    pub(crate) flags: u32,
    pub(crate) n_items: u32,
    pub(crate) items: *const SpaDictItem,
}

/// `struct pw_buffer` in <pipewire/stream.h>, up to `requested` (0.3.49). The `time`
/// field after it is from 1.0.5, newer than ADR-0060's floor, so it is never read.
#[repr(C)]
pub(crate) struct PwBuffer {
    pub(crate) buffer: *mut SpaBuffer,
    pub(crate) user_data: *mut c_void,
    /// Set by a playback stream: the frames it queued.
    pub(crate) size: u64,
    /// For playback, the frames PipeWire suggests; 0 for no suggestion.
    pub(crate) requested: u64,
}

/// `struct spa_buffer` in <spa/buffer/buffer.h>.
#[repr(C)]
pub(crate) struct SpaBuffer {
    pub(crate) n_metas: u32,
    pub(crate) n_datas: u32,
    pub(crate) metas: *mut SpaMeta,
    pub(crate) datas: *mut SpaData,
}

/// `struct spa_meta` in <spa/buffer/meta.h>.
#[repr(C)]
pub(crate) struct SpaMeta {
    pub(crate) type_: u32,
    pub(crate) size: u32,
    pub(crate) data: *mut c_void,
}

/// `struct spa_data` in <spa/buffer/buffer.h>.
#[repr(C)]
pub(crate) struct SpaData {
    pub(crate) type_: u32,
    pub(crate) flags: u32,
    pub(crate) fd: i64,
    pub(crate) mapoffset: u32,
    pub(crate) maxsize: u32,
    pub(crate) data: *mut c_void,
    pub(crate) chunk: *mut SpaChunk,
}

/// `struct spa_chunk` in <spa/buffer/buffer.h>.
#[repr(C)]
pub(crate) struct SpaChunk {
    pub(crate) offset: u32,
    pub(crate) size: u32,
    pub(crate) stride: i32,
    pub(crate) flags: i32,
}

/// `struct spa_pod` in <spa/pod/pod.h>.
#[repr(C)]
pub(crate) struct SpaPod {
    pub(crate) size: u32,
    pub(crate) type_: u32,
}

/// `struct spa_fraction` in <spa/utils/defs.h>.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SpaFraction {
    pub(crate) num: u32,
    pub(crate) denom: u32,
}

/// `struct pw_time` in <pipewire/stream.h>, up to `avail_buffers` (0.3.50). Its size
/// is passed to `pw_stream_get_time_n`, which fills only that much, so the 1.0.5
/// `size` field after it is left out.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PwTime {
    /// When this was updated, in `CLOCK_MONOTONIC` nanoseconds; 0 before the first
    /// graph cycle.
    pub(crate) now: i64,
    /// The unit of `ticks` and `delay`, usually 1 / the graph rate.
    pub(crate) rate: SpaFraction,
    pub(crate) ticks: u64,
    /// For capture, how long a sample took from the device; for playback, how long the
    /// next sample takes to reach it. In `rate` units.
    pub(crate) delay: i64,
    pub(crate) queued: u64,
    pub(crate) buffered: u64,
    pub(crate) queued_buffers: u32,
    pub(crate) avail_buffers: u32,
}

/// `struct pw_stream_events` in <pipewire/stream.h>, version 2.
#[repr(C)]
pub(crate) struct PwStreamEvents {
    pub(crate) version: u32,
    pub(crate) destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub(crate) state_changed:
        Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    pub(crate) control_info: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    pub(crate) io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    pub(crate) param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const SpaPod)>,
    pub(crate) add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub(crate) remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub(crate) process: Option<unsafe extern "C" fn(*mut c_void)>,
    pub(crate) drained: Option<unsafe extern "C" fn(*mut c_void)>,
    pub(crate) command: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
    pub(crate) trigger_done: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// `struct pw_core_info` in <pipewire/core.h>.
#[repr(C)]
pub(crate) struct PwCoreInfo {
    pub(crate) id: u32,
    pub(crate) cookie: u32,
    pub(crate) user_name: *const c_char,
    pub(crate) host_name: *const c_char,
    pub(crate) version: *const c_char,
    pub(crate) name: *const c_char,
    pub(crate) change_mask: u64,
    pub(crate) props: *const SpaDict,
}

/// `struct pw_core_events` in <pipewire/core.h>, version 0 (the fields up to
/// `remove_mem`; version 1 adds `bound_props`, which this crate doesn't take).
#[repr(C)]
pub(crate) struct PwCoreEvents {
    pub(crate) version: u32,
    pub(crate) info: Option<unsafe extern "C" fn(*mut c_void, *const PwCoreInfo)>,
    pub(crate) done: Option<unsafe extern "C" fn(*mut c_void, u32, c_int)>,
    pub(crate) ping: Option<unsafe extern "C" fn(*mut c_void, u32, c_int)>,
    pub(crate) error: Option<unsafe extern "C" fn(*mut c_void, u32, c_int, c_int, *const c_char)>,
    pub(crate) remove_id: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    pub(crate) bound_id: Option<unsafe extern "C" fn(*mut c_void, u32, u32)>,
    pub(crate) add_mem: Option<unsafe extern "C" fn(*mut c_void, u32, u32, c_int, u32)>,
    pub(crate) remove_mem: Option<unsafe extern "C" fn(*mut c_void, u32)>,
}

/// `struct pw_core_methods` in <pipewire/core.h>, version 0.
#[repr(C)]
pub(crate) struct PwCoreMethods {
    pub(crate) version: u32,
    pub(crate) add_listener: Option<
        unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const PwCoreEvents, *mut c_void) -> c_int,
    >,
    pub(crate) hello: Option<unsafe extern "C" fn(*mut c_void, u32) -> c_int>,
    pub(crate) sync: Option<unsafe extern "C" fn(*mut c_void, u32, c_int) -> c_int>,
    pub(crate) pong: Option<unsafe extern "C" fn(*mut c_void, u32, c_int) -> c_int>,
    pub(crate) error:
        Option<unsafe extern "C" fn(*mut c_void, u32, c_int, c_int, *const c_char) -> c_int>,
    pub(crate) get_registry: Option<unsafe extern "C" fn(*mut c_void, u32, usize) -> Ptr>,
    pub(crate) create_object: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const c_char,
            u32,
            *const SpaDict,
            usize,
        ) -> Ptr,
    >,
    pub(crate) destroy: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
}

/// `struct pw_registry_events` in <pipewire/core.h>, version 0.
#[repr(C)]
pub(crate) struct PwRegistryEvents {
    pub(crate) version: u32,
    pub(crate) global:
        Option<unsafe extern "C" fn(*mut c_void, u32, u32, *const c_char, u32, *const SpaDict)>,
    pub(crate) global_remove: Option<unsafe extern "C" fn(*mut c_void, u32)>,
}

/// `struct pw_registry_methods` in <pipewire/core.h>, version 0.
#[repr(C)]
pub(crate) struct PwRegistryMethods {
    pub(crate) version: u32,
    pub(crate) add_listener: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut SpaHook,
            *const PwRegistryEvents,
            *mut c_void,
        ) -> c_int,
    >,
    pub(crate) bind:
        Option<unsafe extern "C" fn(*mut c_void, u32, *const c_char, u32, usize) -> Ptr>,
    pub(crate) destroy: Option<unsafe extern "C" fn(*mut c_void, u32) -> c_int>,
}

/// `struct pw_metadata_events` in <pipewire/extensions/metadata.h>, version 0.
#[repr(C)]
pub(crate) struct PwMetadataEvents {
    pub(crate) version: u32,
    pub(crate) property: Option<
        unsafe extern "C" fn(
            *mut c_void,
            u32,
            *const c_char,
            *const c_char,
            *const c_char,
        ) -> c_int,
    >,
}

/// `struct pw_metadata_methods` in <pipewire/extensions/metadata.h>, version 0 (only
/// `add_listener` is called).
#[repr(C)]
pub(crate) struct PwMetadataMethods {
    pub(crate) version: u32,
    pub(crate) add_listener: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut SpaHook,
            *const PwMetadataEvents,
            *mut c_void,
        ) -> c_int,
    >,
}

/// `struct pw_node_info` in <pipewire/node.h>, up to `props` (the fields after it are
/// never read).
#[repr(C)]
pub(crate) struct PwNodeInfo {
    pub(crate) id: u32,
    pub(crate) max_input_ports: u32,
    pub(crate) max_output_ports: u32,
    pub(crate) change_mask: u64,
    pub(crate) n_input_ports: u32,
    pub(crate) n_output_ports: u32,
    pub(crate) state: c_int,
    pub(crate) error: *const c_char,
    pub(crate) props: *const SpaDict,
}

/// `struct pw_node_events` in <pipewire/node.h>, version 0.
#[repr(C)]
pub(crate) struct PwNodeEvents {
    pub(crate) version: u32,
    pub(crate) info: Option<unsafe extern "C" fn(*mut c_void, *const PwNodeInfo)>,
    pub(crate) param:
        Option<unsafe extern "C" fn(*mut c_void, c_int, u32, u32, u32, *const SpaPod)>,
}

/// `struct pw_node_methods` in <pipewire/node.h>, version 0 (only `add_listener` is
/// called).
#[repr(C)]
pub(crate) struct PwNodeMethods {
    pub(crate) version: u32,
    pub(crate) add_listener: Option<
        unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const PwNodeEvents, *mut c_void) -> c_int,
    >,
}

/// `PW_VERSION_STREAM_EVENTS`.
pub(crate) const PW_VERSION_STREAM_EVENTS: u32 = 2;
/// `PW_VERSION_CORE_EVENTS`'s version 0 (see [`PwCoreEvents`]).
pub(crate) const PW_VERSION_CORE_EVENTS: u32 = 0;
/// `PW_VERSION_REGISTRY_EVENTS`.
pub(crate) const PW_VERSION_REGISTRY_EVENTS: u32 = 0;
/// `PW_VERSION_METADATA_EVENTS`.
pub(crate) const PW_VERSION_METADATA_EVENTS: u32 = 0;
/// `PW_VERSION_REGISTRY`.
pub(crate) const PW_VERSION_REGISTRY: u32 = 3;
/// `PW_VERSION_NODE_EVENTS`.
pub(crate) const PW_VERSION_NODE_EVENTS: u32 = 0;
/// `PW_VERSION_NODE`.
pub(crate) const PW_VERSION_NODE: u32 = 3;
/// `PW_VERSION_METADATA`.
pub(crate) const PW_VERSION_METADATA: u32 = 3;
/// `PW_ID_CORE`.
pub(crate) const PW_ID_CORE: u32 = 0;
/// `PW_ID_ANY`.
pub(crate) const PW_ID_ANY: u32 = u32::MAX;
/// `PW_TYPE_INTERFACE_Node`.
pub(crate) const TYPE_NODE: &str = "PipeWire:Interface:Node";
/// `PW_TYPE_INTERFACE_Client`.
pub(crate) const TYPE_CLIENT: &str = "PipeWire:Interface:Client";
/// `PW_TYPE_INTERFACE_Metadata`.
pub(crate) const TYPE_METADATA: &str = "PipeWire:Interface:Metadata";
/// `PW_DIRECTION_INPUT` (`SPA_DIRECTION_INPUT`): a stream that receives, such as a
/// capture.
pub(crate) const PW_DIRECTION_INPUT: c_int = 0;
/// `PW_DIRECTION_OUTPUT`: a stream that produces, such as a playback.
pub(crate) const PW_DIRECTION_OUTPUT: c_int = 1;
/// `PW_STREAM_FLAG_AUTOCONNECT`.
pub(crate) const FLAG_AUTOCONNECT: u32 = 1 << 0;
/// `PW_STREAM_FLAG_MAP_BUFFERS`.
pub(crate) const FLAG_MAP_BUFFERS: u32 = 1 << 2;
/// `PW_STREAM_FLAG_RT_PROCESS`: `process` runs on the context's real-time data loop.
pub(crate) const FLAG_RT_PROCESS: u32 = 1 << 4;
/// `PW_STREAM_STATE_ERROR`.
pub(crate) const STATE_ERROR: c_int = -1;
/// `PW_STREAM_STATE_UNCONNECTED`.
pub(crate) const STATE_UNCONNECTED: c_int = 0;
/// `PW_STREAM_STATE_STREAMING`.
pub(crate) const STATE_STREAMING: c_int = 3;
/// `SPA_CHUNK_FLAG_CORRUPTED`: the chunk's data may be corrupt.
pub(crate) const CHUNK_CORRUPTED: i32 = 1 << 0;
/// `SPA_CHUNK_FLAG_EMPTY`: the chunk holds media-specific neutral data, such as silence.
pub(crate) const CHUNK_EMPTY: i32 = 1 << 1;
/// `SPA_CHUNK_FLAG_CORRUPTED | SPA_CHUNK_FLAG_EMPTY`: a video frame to skip.
pub(crate) const CHUNK_SKIP: i32 = CHUNK_CORRUPTED | CHUNK_EMPTY;

pub(crate) type Ptr = *mut c_void;

/// The libpipewire functions the streams and the registry call, with their C
/// declarations.
#[derive(Clone, Copy)]
pub(crate) struct Fns {
    /// `void pw_init(int *argc, char **argv[])`
    pub(crate) init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    /// `struct pw_thread_loop *pw_thread_loop_new(const char *name, const struct spa_dict *props)`
    pub(crate) thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> Ptr,
    /// `struct pw_loop *pw_thread_loop_get_loop(struct pw_thread_loop *loop)`
    pub(crate) thread_loop_get_loop: unsafe extern "C" fn(Ptr) -> Ptr,
    /// `int pw_thread_loop_start(struct pw_thread_loop *loop)`
    pub(crate) thread_loop_start: unsafe extern "C" fn(Ptr) -> c_int,
    /// `void pw_thread_loop_stop(struct pw_thread_loop *loop)`
    pub(crate) thread_loop_stop: unsafe extern "C" fn(Ptr),
    /// `void pw_thread_loop_lock(struct pw_thread_loop *loop)`
    pub(crate) thread_loop_lock: unsafe extern "C" fn(Ptr),
    /// `void pw_thread_loop_unlock(struct pw_thread_loop *loop)`
    pub(crate) thread_loop_unlock: unsafe extern "C" fn(Ptr),
    /// `int pw_thread_loop_timed_wait(struct pw_thread_loop *loop, int wait_max_sec)`
    pub(crate) thread_loop_timed_wait: unsafe extern "C" fn(Ptr, c_int) -> c_int,
    /// `void pw_thread_loop_signal(struct pw_thread_loop *loop, bool wait_for_accept)`
    pub(crate) thread_loop_signal: unsafe extern "C" fn(Ptr, bool),
    /// `void pw_thread_loop_destroy(struct pw_thread_loop *loop)`
    pub(crate) thread_loop_destroy: unsafe extern "C" fn(Ptr),
    /// `struct pw_context *pw_context_new(struct pw_loop *main_loop, struct pw_properties *props, size_t user_data_size)`
    pub(crate) context_new: unsafe extern "C" fn(Ptr, Ptr, usize) -> Ptr,
    /// `void pw_context_destroy(struct pw_context *context)`
    pub(crate) context_destroy: unsafe extern "C" fn(Ptr),
    /// `struct pw_core *pw_context_connect(struct pw_context *context, struct pw_properties *properties, size_t user_data_size)`
    pub(crate) context_connect: unsafe extern "C" fn(Ptr, Ptr, usize) -> Ptr,
    /// `struct pw_core *pw_context_connect_fd(struct pw_context *context, int fd, struct pw_properties *properties, size_t user_data_size)`
    pub(crate) context_connect_fd: unsafe extern "C" fn(Ptr, c_int, Ptr, usize) -> Ptr,
    /// `int pw_core_disconnect(struct pw_core *core)`
    pub(crate) core_disconnect: unsafe extern "C" fn(Ptr) -> c_int,
    /// `void pw_proxy_destroy(struct pw_proxy *proxy)`
    pub(crate) proxy_destroy: unsafe extern "C" fn(Ptr),
    /// `struct pw_properties *pw_properties_new_string(const char *args)`
    pub(crate) properties_new_string: unsafe extern "C" fn(*const c_char) -> Ptr,
    /// `int pw_properties_set(struct pw_properties *properties, const char *key, const char *value)`
    pub(crate) properties_set: unsafe extern "C" fn(Ptr, *const c_char, *const c_char) -> c_int,
    /// `struct pw_stream *pw_stream_new(struct pw_core *core, const char *name, struct pw_properties *props)`
    pub(crate) stream_new: unsafe extern "C" fn(Ptr, *const c_char, Ptr) -> Ptr,
    /// `void pw_stream_add_listener(struct pw_stream *stream, struct spa_hook *listener, const struct pw_stream_events *events, void *data)`
    pub(crate) stream_add_listener:
        unsafe extern "C" fn(Ptr, *mut SpaHook, *const PwStreamEvents, Ptr),
    /// `int pw_stream_connect(struct pw_stream *stream, enum pw_direction direction, uint32_t target_id, enum pw_stream_flags flags, const struct spa_pod **params, uint32_t n_params)`
    pub(crate) stream_connect:
        unsafe extern "C" fn(Ptr, c_int, u32, u32, *mut *const c_void, u32) -> c_int,
    /// `int pw_stream_update_params(struct pw_stream *stream, const struct spa_pod **params, uint32_t n_params)`
    pub(crate) stream_update_params: unsafe extern "C" fn(Ptr, *mut *const c_void, u32) -> c_int,
    /// `struct pw_buffer *pw_stream_dequeue_buffer(struct pw_stream *stream)`
    pub(crate) stream_dequeue_buffer: unsafe extern "C" fn(Ptr) -> *mut PwBuffer,
    /// `int pw_stream_queue_buffer(struct pw_stream *stream, struct pw_buffer *buffer)`
    pub(crate) stream_queue_buffer: unsafe extern "C" fn(Ptr, *mut PwBuffer) -> c_int,
    /// `int pw_stream_get_time_n(struct pw_stream *stream, struct pw_time *time, size_t size)`
    pub(crate) stream_get_time_n: unsafe extern "C" fn(Ptr, *mut PwTime, usize) -> c_int,
    /// `int pw_stream_disconnect(struct pw_stream *stream)`
    pub(crate) stream_disconnect: unsafe extern "C" fn(Ptr) -> c_int,
    /// `void pw_stream_destroy(struct pw_stream *stream)`
    pub(crate) stream_destroy: unsafe extern "C" fn(Ptr),
}

impl Fns {
    pub(crate) fn load(lib: &SystemLibrary) -> Result<Self, PlatformError> {
        // SAFETY: each type below is the C declaration in its field's doc comment, from
        // <pipewire/pipewire.h>, <pipewire/thread-loop.h>, <pipewire/context.h>,
        // <pipewire/core.h>, <pipewire/proxy.h>, <pipewire/properties.h> and
        // <pipewire/stream.h>; the caller keeps `lib` beside the table so they stay
        // callable.
        unsafe {
            Ok(Self {
                init: lib.function("pw_init")?,
                thread_loop_new: lib.function("pw_thread_loop_new")?,
                thread_loop_get_loop: lib.function("pw_thread_loop_get_loop")?,
                thread_loop_start: lib.function("pw_thread_loop_start")?,
                thread_loop_stop: lib.function("pw_thread_loop_stop")?,
                thread_loop_lock: lib.function("pw_thread_loop_lock")?,
                thread_loop_unlock: lib.function("pw_thread_loop_unlock")?,
                thread_loop_timed_wait: lib.function("pw_thread_loop_timed_wait")?,
                thread_loop_signal: lib.function("pw_thread_loop_signal")?,
                thread_loop_destroy: lib.function("pw_thread_loop_destroy")?,
                context_new: lib.function("pw_context_new")?,
                context_destroy: lib.function("pw_context_destroy")?,
                context_connect: lib.function("pw_context_connect")?,
                context_connect_fd: lib.function("pw_context_connect_fd")?,
                core_disconnect: lib.function("pw_core_disconnect")?,
                proxy_destroy: lib.function("pw_proxy_destroy")?,
                properties_new_string: lib.function("pw_properties_new_string")?,
                properties_set: lib.function("pw_properties_set")?,
                stream_new: lib.function("pw_stream_new")?,
                stream_add_listener: lib.function("pw_stream_add_listener")?,
                stream_connect: lib.function("pw_stream_connect")?,
                stream_update_params: lib.function("pw_stream_update_params")?,
                stream_dequeue_buffer: lib.function("pw_stream_dequeue_buffer")?,
                stream_queue_buffer: lib.function("pw_stream_queue_buffer")?,
                stream_get_time_n: lib.function("pw_stream_get_time_n")?,
                stream_disconnect: lib.function("pw_stream_disconnect")?,
                stream_destroy: lib.function("pw_stream_destroy")?,
            })
        }
    }
}

/// Reads a C string PipeWire handed over, or `None` for null. Lossy: PipeWire's
/// strings are UTF-8 in practice, and a name is only shown.
///
/// # Safety
/// `s` is null or a NUL-terminated string valid for the call.
pub(crate) unsafe fn string(s: *const c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: the function's contract.
    Some(
        unsafe { std::ffi::CStr::from_ptr(s) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// A dictionary's items as owned pairs; an empty list for null.
///
/// # Safety
/// `dict` is null or a valid `spa_dict` whose `items` holds `n_items` entries of
/// NUL-terminated strings (or nulls), all valid for the call.
pub(crate) unsafe fn dict(dict: *const SpaDict) -> Vec<(String, String)> {
    if dict.is_null() {
        return Vec::new();
    }
    // SAFETY: the function's contract.
    let d = unsafe { &*dict };
    if d.items.is_null() {
        return Vec::new();
    }
    // SAFETY: `items` holds `n_items` entries.
    let items = unsafe { std::slice::from_raw_parts(d.items, d.n_items as usize) };
    items
        .iter()
        .filter_map(|i| {
            // SAFETY: each key and value is null or a valid C string.
            let (k, v) = unsafe { (string(i.key)?, string(i.value)?) };
            Some((k, v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    /// A removed hook leaves its list linked around it (here, empty again) and itself
    /// unlinked; removing it again, or a hook never linked, does nothing.
    #[test]
    fn a_removed_hook_is_unlinked_from_its_list() {
        let mut head = Box::new(SpaList {
            next: std::ptr::null_mut(),
            prev: std::ptr::null_mut(),
        });
        let mut hook = SpaHook::boxed();
        let h: *mut SpaList = &raw mut *head;
        let l: *mut SpaList = &raw mut hook.link;
        // Linked as `spa_list_prepend` does into an empty list.
        head.next = l;
        head.prev = l;
        hook.link.next = h;
        hook.link.prev = h;
        // SAFETY: the hook is linked into `head`, which nothing else uses.
        unsafe { hook.remove() };
        assert_eq!((head.next, head.prev), (h, h));
        assert!(hook.link.next.is_null() && hook.link.prev.is_null());
        // SAFETY: not linked any more.
        unsafe { hook.remove() };
        let mut unlinked = SpaHook::boxed();
        // SAFETY: never linked.
        unsafe { unlinked.remove() };
        assert!(unlinked.link.next.is_null());
    }

    /// The layouts match libspa's and libpipewire's on 64-bit Linux, as a C program
    /// printed them with `sizeof` and `offsetof` against PipeWire 1.0.5's headers.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn layouts_match_the_c_headers() {
        assert_eq!(size_of::<SpaHook>(), 48);
        assert_eq!(size_of::<SpaInterface>(), 32);
        assert_eq!(offset_of!(SpaInterface, cb), 16);
        assert_eq!(size_of::<SpaDict>(), 16);
        assert_eq!(size_of::<SpaDictItem>(), 16);
        assert_eq!(offset_of!(PwBuffer, requested), 24);
        assert_eq!(size_of::<SpaBuffer>(), 24);
        assert_eq!(size_of::<SpaMeta>(), 16);
        assert_eq!(size_of::<SpaData>(), 40);
        assert_eq!(offset_of!(SpaData, data), 24);
        assert_eq!(offset_of!(SpaData, chunk), 32);
        assert_eq!(size_of::<SpaChunk>(), 16);
        assert_eq!(offset_of!(PwTime, delay), 24);
        assert_eq!(offset_of!(PwTime, avail_buffers), 52);
        assert_eq!(size_of::<PwTime>(), 56);
        assert_eq!(offset_of!(PwStreamEvents, process), 64);
        assert_eq!(size_of::<PwStreamEvents>(), 96);
        assert_eq!(offset_of!(PwCoreInfo, props), 48);
        assert_eq!(offset_of!(PwCoreEvents, done), 16);
        assert_eq!(offset_of!(PwCoreEvents, error), 32);
        assert_eq!(size_of::<PwCoreEvents>(), 72);
        assert_eq!(offset_of!(PwCoreMethods, sync), 24);
        assert_eq!(offset_of!(PwCoreMethods, get_registry), 48);
        assert_eq!(offset_of!(PwRegistryEvents, global_remove), 16);
        assert_eq!(offset_of!(PwRegistryMethods, bind), 16);
        assert_eq!(offset_of!(PwMetadataEvents, property), 8);
        assert_eq!(offset_of!(PwMetadataMethods, add_listener), 8);
        assert_eq!(offset_of!(PwNodeInfo, change_mask), 16);
        assert_eq!(offset_of!(PwNodeInfo, props), 48);
        assert_eq!(offset_of!(PwNodeEvents, info), 8);
        assert_eq!(size_of::<PwNodeEvents>(), 24);
        assert_eq!(offset_of!(PwNodeMethods, add_listener), 8);
    }

    #[test]
    fn dicts_and_strings_read_back() {
        let k = c"node.name";
        let v = c"alsa_output.pci";
        let items = [
            SpaDictItem {
                key: k.as_ptr(),
                value: v.as_ptr(),
            },
            SpaDictItem {
                key: k.as_ptr(),
                value: std::ptr::null(),
            },
        ];
        let d = SpaDict {
            flags: 0,
            n_items: 2,
            items: items.as_ptr(),
        };
        // SAFETY: a valid dict of valid strings, alive for the call.
        let pairs = unsafe { dict(&d) };
        assert_eq!(pairs, [("node.name".into(), "alsa_output.pci".into())]);
        // SAFETY: null is allowed.
        assert!(unsafe { dict(std::ptr::null()) }.is_empty());
        // SAFETY: null is allowed.
        assert_eq!(unsafe { string(std::ptr::null()) }, None);
    }
}
