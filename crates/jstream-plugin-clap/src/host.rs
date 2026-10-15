//! The host side of the ABI: the `clap_host` a plugin calls back into, and the host
//! extensions it can ask for. Every callback catches panics (R-UNSAFE-005) and only
//! sets a flag, so it is safe from any thread, the audio thread included.

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, clap_host_params, clap_param_clear_flags, clap_param_rescan_flags,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::version::CLAP_VERSION;

/// What a plugin asked of the host since the host last looked. Each flag is taken
/// (read and cleared) by the host.
#[derive(Debug, Default)]
pub(crate) struct Requests {
    /// `request_restart`: deactivate and reactivate.
    pub(crate) restart: AtomicBool,
    /// `request_process`: start processing (ignored: the host always processes).
    pub(crate) process: AtomicBool,
    /// `request_callback`: call `on_main_thread`.
    pub(crate) callback: AtomicBool,
    /// `clap_host_latency.changed`: the latency changed (applies on reactivation).
    pub(crate) latency: AtomicBool,
    /// `clap_host_params.rescan`: parameter values or info changed.
    pub(crate) params: AtomicBool,
    /// `clap_host_params.request_flush`: send parameter changes through `flush`.
    pub(crate) flush: AtomicBool,
    /// `clap_host_state.mark_dirty`: the state changed and should be saved.
    pub(crate) dirty: AtomicBool,
}

/// The `clap_host` handed to one plugin instance, with its requests.
#[derive(Debug)]
pub(crate) struct HostData {
    pub(crate) clap: clap_host,
    pub(crate) requests: Requests,
}

/// Owns a [`HostData`] on the heap through a raw pointer, so the address the plugin
/// keeps stays valid however the owner moves (a moved `Box` would re-assert unique
/// ownership of it). Only shared access is given out: after construction, every field
/// the plugin can reach is read-only or atomic.
#[derive(Debug)]
pub(crate) struct HostBox(NonNull<HostData>);

impl HostBox {
    pub(crate) fn new() -> Self {
        let data = NonNull::from(Box::leak(Box::new(HostData {
            clap: clap_host {
                clap_version: CLAP_VERSION,
                host_data: std::ptr::null_mut(),
                name: c"JStream".as_ptr(),
                vendor: c"JStream".as_ptr(),
                url: std::ptr::null(),
                version: c"0".as_ptr(),
                get_extension: Some(get_extension),
                request_restart: Some(request_restart),
                request_process: Some(request_process),
                request_callback: Some(request_callback),
            },
            requests: Requests::default(),
        })));
        // SAFETY: `data` is a fresh, unshared allocation; this is its only write.
        unsafe { (*data.as_ptr()).clap.host_data = data.as_ptr().cast() };
        Self(data)
    }

    /// The `clap_host` to hand the plugin, derived from the owning pointer.
    pub(crate) fn clap(&self) -> *const clap_host {
        // SAFETY: the allocation is live while `self` is.
        unsafe { &raw const (*self.0.as_ptr()).clap }
    }
}

impl std::ops::Deref for HostBox {
    type Target = HostData;
    fn deref(&self) -> &HostData {
        // SAFETY: live until `drop`, and never mutably borrowed after `new`.
        unsafe { self.0.as_ref() }
    }
}

impl Drop for HostBox {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::leak` in `new` and is freed once, here.
        // The owner drops it only after the plugin is destroyed.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

/// The requests of the host `host` belongs to; `None` for a pointer the plugin mangled.
///
/// # Safety
/// `host` is null or the `clap` field of a live [`HostData`].
unsafe fn requests<'a>(host: *const clap_host) -> Option<&'a Requests> {
    // SAFETY: the caller guarantees `host` is null or ours, and ours point back at
    // their `HostData` through `host_data`.
    unsafe {
        let host = host.as_ref()?;
        let data = host.host_data.cast::<HostData>().cast_const().as_ref()?;
        Some(&data.requests)
    }
}

/// Sets one request flag for `host`, catching any panic.
fn flag(host: *const clap_host, pick: fn(&Requests) -> &AtomicBool) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: plugins call back with the host pointer they were given.
        if let Some(r) = unsafe { requests(host) } {
            pick(r).store(true, Ordering::Release);
        }
    }));
}

unsafe extern "C" fn request_restart(host: *const clap_host) {
    flag(host, |r| &r.restart);
}

unsafe extern "C" fn request_process(host: *const clap_host) {
    flag(host, |r| &r.process);
}

unsafe extern "C" fn request_callback(host: *const clap_host) {
    flag(host, |r| &r.callback);
}

unsafe extern "C" fn latency_changed(host: *const clap_host) {
    flag(host, |r| &r.latency);
}

unsafe extern "C" fn params_rescan(host: *const clap_host, _flags: clap_param_rescan_flags) {
    flag(host, |r| &r.params);
}

unsafe extern "C" fn params_clear(
    _host: *const clap_host,
    _param_id: clap_id,
    _flags: clap_param_clear_flags,
) {
    // The host keeps no automation or modulation to clear.
}

unsafe extern "C" fn params_request_flush(host: *const clap_host) {
    flag(host, |r| &r.flush);
}

unsafe extern "C" fn state_mark_dirty(host: *const clap_host) {
    flag(host, |r| &r.dirty);
}

static HOST_LATENCY: clap_host_latency = clap_host_latency {
    changed: Some(latency_changed),
};

static HOST_PARAMS: clap_host_params = clap_host_params {
    rescan: Some(params_rescan),
    clear: Some(params_clear),
    request_flush: Some(params_request_flush),
};

static HOST_STATE: clap_host_state = clap_host_state {
    mark_dirty: Some(state_mark_dirty),
};

/// The host extensions JStream offers: latency, params and state. Null for anything
/// else (the plugin then does without it), including the GUI, which comes with the
/// editor window.
unsafe extern "C" fn get_extension(_host: *const clap_host, id: *const c_char) -> *const c_void {
    catch_unwind(|| {
        if id.is_null() {
            return std::ptr::null();
        }
        // SAFETY: a non-null extension id is a NUL-terminated string (CLAP contract).
        let id = unsafe { CStr::from_ptr(id) };
        if id == CLAP_EXT_LATENCY {
            (&raw const HOST_LATENCY).cast()
        } else if id == CLAP_EXT_PARAMS {
            (&raw const HOST_PARAMS).cast()
        } else if id == CLAP_EXT_STATE {
            (&raw const HOST_STATE).cast()
        } else {
            std::ptr::null()
        }
    })
    .unwrap_or(std::ptr::null())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callbacks_set_their_flags() {
        let host = HostBox::new();
        let h = host.clap();
        // SAFETY: `h` is a live host, as a plugin would hold it.
        unsafe {
            request_restart(h);
            request_callback(h);
            latency_changed(h);
        }
        assert!(host.requests.restart.load(Ordering::Acquire));
        assert!(host.requests.callback.load(Ordering::Acquire));
        assert!(host.requests.latency.load(Ordering::Acquire));
        assert!(!host.requests.dirty.load(Ordering::Acquire));
    }

    #[test]
    fn a_mangled_host_pointer_is_ignored() {
        // SAFETY: null is what a broken plugin might pass; it is checked.
        unsafe { request_restart(std::ptr::null()) };
    }

    #[test]
    fn offers_latency_params_and_state_only() {
        let host = HostBox::new();
        let get = host.clap.get_extension.unwrap();
        // SAFETY: literals are NUL-terminated.
        unsafe {
            assert!(!get(&host.clap, CLAP_EXT_LATENCY.as_ptr()).is_null());
            assert!(!get(&host.clap, CLAP_EXT_PARAMS.as_ptr()).is_null());
            assert!(!get(&host.clap, CLAP_EXT_STATE.as_ptr()).is_null());
            assert!(get(&host.clap, c"clap.gui".as_ptr()).is_null());
            assert!(get(&host.clap, std::ptr::null()).is_null());
        }
    }
}
