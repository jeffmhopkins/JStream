//! CPU time and memory of this process and of CEF's subprocesses (its direct children:
//! GPU, renderer, utility and so on), so the report covers the whole browser, not just
//! the process that composites.

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::json::{self, Obj};

#[derive(Clone)]
pub struct Sample {
    pub pid: u32,
    pub name: String,
    /// User + kernel CPU seconds since the process started.
    pub cpu_s: f64,
    pub working_set: u64,
    pub private_bytes: u64,
}

/// This process and its direct children, now.
pub fn snapshot() -> Vec<Sample> {
    // SAFETY: plain call.
    let me = unsafe { GetCurrentProcessId() };
    let mut pids = vec![(me, "self".to_owned())];
    // SAFETY: plain call; the snapshot handle is closed below.
    if let Ok(snap) = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        // SAFETY: `snap` is a live snapshot and `entry` has its size set.
        let mut more = unsafe { Process32FirstW(snap, &mut entry) }.is_ok();
        while more {
            if entry.th32ParentProcessID == me {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                pids.push((
                    entry.th32ProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..len]),
                ));
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snap, &mut entry) }.is_ok();
        }
        // SAFETY: we own the snapshot handle and close it once.
        let _ = unsafe { CloseHandle(snap) };
    }
    pids.into_iter()
        .filter_map(|(pid, name)| sample(pid, name))
        .collect()
}

fn sample(pid: u32, name: String) -> Option<Sample> {
    // SAFETY: plain call; the handle is closed below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let result = read(handle, pid, name);
    // SAFETY: we own the handle and close it once.
    let _ = unsafe { CloseHandle(handle) };
    result
}

fn read(handle: HANDLE, pid: u32, name: String) -> Option<Sample> {
    let (mut creation, mut exit, mut kernel, mut user) = Default::default();
    // SAFETY: `handle` is a live process handle; the out-pointers point to valid locals.
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }.ok()?;
    let secs = |t: FILETIME| {
        ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as f64 / 1e7
    };
    let mut mem = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: `mem` is a PROCESS_MEMORY_COUNTERS_EX whose `cb` says so; the API accepts the
    // extended struct through the base pointer.
    let mem_ok = unsafe {
        GetProcessMemoryInfo(
            handle,
            std::ptr::from_mut(&mut mem).cast(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    }
    .is_ok();
    Some(Sample {
        pid,
        name,
        cpu_s: secs(kernel) + secs(user),
        working_set: if mem_ok { mem.WorkingSetSize as u64 } else { 0 },
        private_bytes: if mem_ok { mem.PrivateUsage as u64 } else { 0 },
    })
}

/// CPU over the window (as % of one core) and memory at its end, per process and in total.
pub fn json(start: &[Sample], end: &[Sample], seconds: f64) -> String {
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    let mut total_cpu = 0.0;
    let (mut total_ws, mut total_private) = (0u64, 0u64);
    let rows: Vec<String> = end
        .iter()
        .map(|e| {
            let before = start
                .iter()
                .find(|s| s.pid == e.pid)
                .map_or(0.0, |s| s.cpu_s);
            let cpu = (e.cpu_s - before) / seconds.max(f64::EPSILON) * 100.0;
            total_cpu += cpu;
            total_ws += e.working_set;
            total_private += e.private_bytes;
            Obj::new()
                .int("pid", e.pid)
                .str("name", &e.name)
                .num("cpu_percent_of_one_core", cpu)
                .num("working_set_mb", mb(e.working_set))
                .num("private_mb", mb(e.private_bytes))
                .build()
        })
        .collect();
    Obj::new()
        .int("count", rows.len() as u64)
        .num("total_cpu_percent_of_one_core", total_cpu)
        .num("total_working_set_mb", mb(total_ws))
        .num("total_private_mb", mb(total_private))
        .raw("processes", json::array(rows))
        .build()
}
