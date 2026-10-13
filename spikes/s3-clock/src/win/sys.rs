//! Small Windows helpers: COM, QPC, process CPU time, the OS build, Ctrl+C and MMCSS.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{ERROR_SUCCESS, FILETIME, HANDLE};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentProcess,
    GetProcessTimes,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::clock::qpc_to_hns;
use crate::err::{Context, Result};

/// Initialises COM (multithreaded) on this thread for as long as it lives.
pub struct Com;

impl Com {
    pub fn init() -> Result<Self> {
        // SAFETY: plain call; balanced by CoUninitialize in Drop. S_FALSE (already
        // initialised in the same mode) is a success and also needs balancing.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .context("CoInitializeEx")?;
        Ok(Self)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx in `init` on this thread.
        unsafe { CoUninitialize() };
    }
}

/// The current QPC reading in 100 ns units, the unit of `pu64QPCPosition`.
pub fn qpc_now_hns() -> Result<i64> {
    let mut ticks = 0i64;
    let mut freq = 0i64;
    // SAFETY: both pointers are valid for the duration of the calls.
    unsafe {
        QueryPerformanceCounter(&mut ticks).context("QueryPerformanceCounter")?;
        QueryPerformanceFrequency(&mut freq).context("QueryPerformanceFrequency")?;
    }
    qpc_to_hns(ticks, freq).context("QPC frequency is not positive")
}

/// User plus kernel CPU time of this process, in 100 ns units.
pub fn process_cpu_hns() -> u64 {
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the pseudo-handle needs no closing; all out-pointers are valid locals.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok.is_err() {
        return 0;
    }
    let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    ft(k) + ft(u)
}

const CURRENT_VERSION: PCWSTR = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");

fn reg_string(name: &str) -> Option<String> {
    let name = HSTRING::from(name);
    let mut size = 0u32;
    // SAFETY: a size query: no data pointer, `size` receives the byte count.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            CURRENT_VERSION,
            &name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if rc != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `bytes` bytes; RegGetValueW null-terminates within it.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            CURRENT_VERSION,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn reg_dword(name: &str) -> Option<u32> {
    let name = HSTRING::from(name);
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: `value` is a 4-byte buffer and `size` says so.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            CURRENT_VERSION,
            &name,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    (rc == ERROR_SUCCESS).then_some(value)
}

/// "Windows 11 Pro 24H2 (build 26100.4061)", from the registry. `ProductName` still
/// says "Windows 10" on Windows 11, so the major version is derived from the build.
pub fn windows_version() -> String {
    let build = reg_string("CurrentBuildNumber").unwrap_or_else(|| "?".into());
    let ubr = reg_dword("UBR").map_or(String::new(), |u| format!(".{u}"));
    let product = reg_string("ProductName").unwrap_or_else(|| "Windows".into());
    let product = match build.parse::<u32>() {
        Ok(b) if b >= 22_000 => product.replacen("Windows 10", "Windows 11", 1),
        _ => product,
    };
    let display = reg_string("DisplayVersion").unwrap_or_default();
    format!("{product} {display} (build {build}{ubr})").replace("  ", " ")
}

static CTRL_C: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_ctrl(kind: u32) -> BOOL {
    // CTRL_C_EVENT (0) and CTRL_BREAK_EVENT (1): stop gracefully and report.
    if kind <= 1 {
        CTRL_C.store(true, Ordering::SeqCst);
        BOOL(1)
    } else {
        BOOL(0)
    }
}

/// Makes Ctrl+C end the run early with a report instead of killing the process.
pub fn install_ctrl_handler() {
    // SAFETY: `on_ctrl` is a valid handler for the life of the process; it only touches
    // an atomic.
    let _ = unsafe { SetConsoleCtrlHandler(Some(on_ctrl), true) };
}

pub fn ctrl_c_pressed() -> bool {
    CTRL_C.load(Ordering::SeqCst)
}

/// Registers this thread with MMCSS as "Pro Audio" (ADR-0014) until dropped.
pub struct Mmcss(Option<HANDLE>);

impl Mmcss {
    pub fn pro_audio() -> Self {
        let mut index = 0u32;
        // SAFETY: `index` is a valid out-pointer; the task name is a static string.
        let h = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) };
        Self(h.ok())
    }

    pub fn active(&self) -> bool {
        self.0.is_some()
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            // SAFETY: `h` came from AvSetMmThreadCharacteristicsW on this thread.
            let _ = unsafe { AvRevertMmThreadCharacteristics(h) };
        }
    }
}
