//! Crash diagnostics: a stage trace and a fault report on stderr.
//!
//! The first Windows run died with an access violation (0xC0000005) and no output, so
//! the spike now says on stderr which stage it reached, and on a fault prints the
//! exception, the module it happened in and a backtrace before Windows ends the process.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::core::PCWSTR;

static START: OnceLock<Instant> = OnceLock::new();
static REPORTED: AtomicU32 = AtomicU32::new(0);

/// Prints `s1 stage +<ms>: <what>` on stderr, so a crash shows the last stage reached.
pub fn stage(what: impl std::fmt::Display) {
    let ms = START.get_or_init(Instant::now).elapsed().as_millis();
    eprintln!("s1 stage +{ms}ms: {what}");
}

/// Installs the fault reporter. Call once, first thing.
pub fn install() {
    START.get_or_init(Instant::now);
    // SAFETY: `on_exception` has the signature Windows expects and stays valid for the
    // life of the process (it is a plain function).
    let _ = unsafe { AddVectoredExceptionHandler(1, Some(on_exception)) };
}

/// Codes worth a report: access violation, illegal instruction, stack overflow, heap
/// corruption, divide by zero. C++ and RPC exceptions that drivers throw and catch are
/// left alone.
const SERIOUS: [u32; 5] = [
    0xC000_0005,
    0xC000_001D,
    0xC000_00FD,
    0xC000_0374,
    0xC000_0094,
];

unsafe extern "system" fn on_exception(info: *mut EXCEPTION_POINTERS) -> i32 {
    const CONTINUE_SEARCH: i32 = 0;
    // SAFETY: Windows passes valid exception pointers to a vectored handler.
    let Some(record) =
        (unsafe { info.as_ref() }).and_then(|i| unsafe { i.ExceptionRecord.as_ref() })
    else {
        return CONTINUE_SEARCH;
    };
    let code = record.ExceptionCode.0 as u32;
    // A first-chance handler also sees faults that are handled later; report at most three.
    if !SERIOUS.contains(&code) || REPORTED.fetch_add(1, Ordering::Relaxed) >= 3 {
        return CONTINUE_SEARCH;
    }
    let address = record.ExceptionAddress as usize;
    let (module, offset) = module_of(address);
    let mut detail = String::new();
    if code == 0xC000_0005 && record.NumberParameters >= 2 {
        let kind = match record.ExceptionInformation[0] {
            0 => "reading",
            1 => "writing",
            8 => "executing",
            _ => "accessing",
        };
        detail = format!(" ({kind} {:#x})", record.ExceptionInformation[1]);
    }
    let thread = std::thread::current();
    eprintln!(
        "s1 FAULT: exception {code:#010x}{detail} at {address:#x} = {module}+{offset:#x} \
         on thread {:?} ({})",
        thread.id(),
        thread.name().unwrap_or("unnamed"),
    );
    eprintln!("{}", std::backtrace::Backtrace::force_capture());
    CONTINUE_SEARCH
}

/// The file name of the module containing `address`, and the offset into it.
fn module_of(address: usize) -> (String, usize) {
    let mut module = HMODULE::default();
    // SAFETY: with FROM_ADDRESS the "name" is an address inside the module, and the
    // out-pointer is a valid local. UNCHANGED_REFCOUNT means nothing to release.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(address as *const u16),
            &mut module,
        )
    };
    if found.is_err() {
        return ("<no module>".into(), address);
    }
    let mut buf = [0u16; 260];
    // SAFETY: `module` is a loaded module handle and `buf` a writable buffer.
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    let path = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
    let name = path.rsplit('\\').next().unwrap_or(&path).to_owned();
    (name, address.wrapping_sub(module.0 as usize))
}
