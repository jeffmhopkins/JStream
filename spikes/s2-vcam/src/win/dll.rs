//! The DLL's exports and class factory, plus the panic guard every COM entry point uses.
//!
//! R-UNSAFE-005: nothing may unwind into Frame Server. Every exported function and every
//! COM method body runs inside [`guard`], [`guard_quiet`] or [`guard_hr`], which turn a
//! panic into `E_UNEXPECTED` and log it.

use std::any::Any;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Once;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, E_UNEXPECTED, HMODULE, S_FALSE,
    S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows_core::{BOOL, GUID, HRESULT, IUnknown, Interface, PCWSTR, Ref, Result, implement};

use super::CLSID;
use super::activate::Activate;
use super::log::log;
use super::reg::{self, RegScope};

static OBJECTS: AtomicIsize = AtomicIsize::new(0);
static LOCKS: AtomicIsize = AtomicIsize::new(0);

/// Held by every COM object this DLL creates, for `DllCanUnloadNow`.
pub(crate) struct ObjectToken(());

impl ObjectToken {
    pub(crate) fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::AcqRel);
        Self(())
    }
}

impl Drop for ObjectToken {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn panic_text(p: &(dyn Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

/// Runs a COM method body: catches panics, and logs failures.
pub(crate) fn guard<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            log(format_args!(
                "{what} -> {:#010x} {}",
                e.code().0,
                e.message()
            ));
            Err(e)
        }
        Err(p) => {
            log(format_args!("{what} PANICKED: {}", panic_text(&*p)));
            Err(E_UNEXPECTED.into())
        }
    }
}

/// As [`guard`], but only panics are logged (for methods where errors are routine).
pub(crate) fn guard_quiet<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| {
        log(format_args!("{what} PANICKED: {}", panic_text(&*p)));
        Err(E_UNEXPECTED.into())
    })
}

/// As [`guard`], for exports that return a bare `HRESULT`.
fn guard_hr(what: &str, f: impl FnOnce() -> HRESULT) -> HRESULT {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| {
        log(format_args!("{what} PANICKED: {}", panic_text(&*p)));
        E_UNEXPECTED
    })
}

/// Logs, once per process, the first `DllGetClassObject` call. Who hosts the DLL is in
/// every log file's first line ([`super::log::identity`]).
pub(crate) fn log_load_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| log("DllGetClassObject: first call in this process"));
}

/// The full path of this DLL.
pub fn module_path() -> Result<PathBuf> {
    let mut module = HMODULE::default();
    let addr = DllGetClassObject as *const () as *const u16;
    // SAFETY: `addr` is an address inside this module; UNCHANGED_REFCOUNT means no
    // reference is taken, so nothing needs freeing; `module` is a valid out-pointer.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(addr),
            &mut module,
        )
    }?;
    let mut buf = [0u16; 1024];
    // SAFETY: `module` is this DLL's handle and `buf` is writable.
    let n = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    Ok(PathBuf::from(String::from_utf16_lossy(
        &buf[..n.min(buf.len())],
    )))
}

#[implement(IClassFactory)]
struct ClassFactory {
    _token: ObjectToken,
}

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> Result<()> {
        guard("ClassFactory::CreateInstance", || {
            if ppv.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: `ppv` is non-null and points to the caller's out-pointer.
            unsafe { *ppv = std::ptr::null_mut() };
            if !outer.is_null() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            // SAFETY: `riid` is non-null and points to the caller's GUID.
            let iid = unsafe { *riid };
            log(format_args!("ClassFactory::CreateInstance riid={iid:?}"));
            let activate = Activate::create()?;
            let unknown: IUnknown = activate.to_interface();
            // SAFETY: `riid` and `ppv` are valid caller pointers (checked above).
            let hr = unsafe { unknown.query(riid, ppv) };
            if hr.is_ok() {
                return Ok(());
            }
            // Not an interface of the activator: the caller wants the media source itself.
            activate.activate(riid, ppv)
        })
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        guard("ClassFactory::LockServer", || {
            if lock.as_bool() {
                LOCKS.fetch_add(1, Ordering::AcqRel);
            } else {
                LOCKS.fetch_sub(1, Ordering::AcqRel);
            }
            Ok(())
        })
    }
}

/// COM entry point: hands out the class factory for [`CLSID`].
///
/// # Safety
/// Called by COM with valid (or null) pointers, per the `DllGetClassObject` contract.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    guard_hr("DllGetClassObject", || {
        if ppv.is_null() || rclsid.is_null() || riid.is_null() {
            return E_POINTER;
        }
        // SAFETY: `ppv` is non-null and points to the caller's out-pointer.
        unsafe { *ppv = std::ptr::null_mut() };
        log_load_once();
        // SAFETY: `rclsid` is non-null and points to the caller's GUID.
        let clsid = unsafe { *rclsid };
        if clsid != CLSID {
            log(format_args!("DllGetClassObject: unknown CLSID {clsid:?}"));
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = ClassFactory {
            _token: ObjectToken::new(),
        }
        .into();
        // SAFETY: `riid` and `ppv` are valid caller pointers (checked above).
        unsafe { factory.query(riid, ppv) }
    })
}

/// COM entry point: whether the DLL can be unloaded.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    guard_hr("DllCanUnloadNow", || {
        if OBJECTS.load(Ordering::Acquire) == 0 && LOCKS.load(Ordering::Acquire) == 0 {
            S_OK
        } else {
            S_FALSE
        }
    })
}

/// `regsvr32 s2_vcam.dll`: registers machine-wide (`HKLM`), which needs an elevated
/// prompt. The exe's `--register hkcu|hklm` does the same without regsvr32.
#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    guard_hr("DllRegisterServer", || {
        let result = module_path().and_then(|p| reg::register(RegScope::Machine, &p));
        log(format_args!("DllRegisterServer (hklm): {result:?}"));
        result.map_or_else(|e| e.code(), |()| S_OK)
    })
}

/// `regsvr32 /u s2_vcam.dll`: removes the registration from `HKLM` and `HKCU`.
#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    guard_hr("DllUnregisterServer", || {
        let user = reg::unregister(RegScope::User);
        let machine = reg::unregister(RegScope::Machine);
        log(format_args!(
            "DllUnregisterServer: hkcu={user:?} hklm={machine:?}"
        ));
        match (user, machine) {
            (Err(e), _) | (_, Err(e)) => e.code(),
            _ => S_OK,
        }
    })
}
