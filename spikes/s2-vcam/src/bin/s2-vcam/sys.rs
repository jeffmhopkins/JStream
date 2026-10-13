//! Facts about the machine and the registration, for the report and `--status`.

use std::path::{Path, PathBuf};
use std::process::Command;

use s2_vcam::evidence::LoadedBy;
use s2_vcam::win::log::FILE_PREFIX;
use s2_vcam::win::reg::{self, RegScope};
use s2_vcam::win::shared::log_dir;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::err::{Context, Result};
use crate::json::Json;

/// The DLL's file name next to the exe (cargo's cdylib output for `[lib] name = "s2_vcam"`).
pub const DLL_NAME: &str = "s2_vcam.dll";

/// `s2_vcam.dll` in the exe's folder.
pub fn dll_next_to_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let dir = exe.parent().context("exe has no parent folder")?;
    Ok(dir.join(DLL_NAME))
}

/// Windows version facts from the registry (`RtlGetVersion` would need the WDK bindings).
pub fn windows_version() -> Json {
    let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let s = |v: &str| Json::opt_str(reg::read_string(HKEY_LOCAL_MACHINE, key, Some(v)));
    Json::Obj(vec![
        ("product", s("ProductName")),
        ("display_version", s("DisplayVersion")),
        ("build", s("CurrentBuild")),
        (
            "ubr",
            reg::read_dword(HKEY_LOCAL_MACHINE, key, "UBR").map_or(Json::Null, Json::num),
        ),
        ("edition", s("EditionID")),
    ])
}

/// Whether this process runs elevated (needed for `HKLM`).
pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: `token` is a valid out-pointer; the pseudo-handle needs no closing.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let token = s2_vcam::win::shared::OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: the buffer is a TOKEN_ELEVATION of the size passed; `len` is valid.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    ok.is_ok() && elevation.TokenIsElevated != 0
}

/// What is registered where, as JSON. `mode` is `hkcu`, `hklm`, `both` or `none`.
pub fn registration() -> Json {
    let user = reg::registered_path(RegScope::User);
    let machine = reg::registered_path(RegScope::Machine);
    let mode = match (&user, &machine) {
        (Some(_), Some(_)) => "both",
        (Some(_), None) => "hkcu",
        (None, Some(_)) => "hklm",
        (None, None) => "none",
    };
    let here = dll_next_to_exe().ok();
    let points_here = |p: &Option<String>| match (p, &here) {
        (Some(p), Some(h)) => Json::Bool(Path::new(p) == h.as_path()),
        _ => Json::Null,
    };
    let exists = |p: &Option<String>| {
        p.as_ref()
            .map_or(Json::Null, |p| Json::Bool(Path::new(p).exists()))
    };
    Json::Obj(vec![
        ("mode", Json::str(mode)),
        ("clsid", Json::str(s2_vcam::proto::CLSID_STR)),
        ("hkcu_path", Json::opt_str(user.clone())),
        ("hkcu_points_here", points_here(&user)),
        ("hkcu_file_exists", exists(&user)),
        ("hklm_path", Json::opt_str(machine.clone())),
        ("hklm_points_here", points_here(&machine)),
        ("hklm_file_exists", exists(&machine)),
        (
            "dll_next_to_exe",
            Json::opt_str(here.as_ref().map(|p| p.display().to_string())),
        ),
    ])
}

/// Grants LOCAL SERVICE (Frame Server's account) and ALL APPLICATION PACKAGES read and
/// execute on the DLL file itself, with `icacls`. Frame Server bypasses traverse checks,
/// so the file's own ACL is what matters, even inside a user profile. Returns icacls'
/// output, or an error with it.
pub fn grant_dll_access(dll: &Path) -> Result<String> {
    let out = Command::new("icacls")
        .arg(dll)
        .args(["/grant", "*S-1-5-19:(RX)", "/grant", "*S-1-15-2-1:(RX)"])
        .output()
        .context("running icacls")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    if out.status.success() {
        Ok(text)
    } else {
        crate::err::bail!("icacls failed ({}): {text}", out.status)
    }
}

/// Deletes old DLL logs so the report only shows this run. Returns files that couldn't be
/// deleted.
pub fn clear_dll_logs() -> Vec<String> {
    let mut stuck = Vec::new();
    for path in dll_logs() {
        if let Err(e) = std::fs::remove_file(&path) {
            stuck.push(format!("{}: {e}", path.display()));
        }
    }
    stuck
}

fn dll_logs() -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(log_dir()) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = dir
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(FILE_PREFIX) && n.ends_with(".log"))
        })
        .collect();
    files.sort();
    files
}

/// The DLL's log lines from this run, and who hosts it (from its `LOADED` lines).
pub struct DllLog {
    pub lines: Vec<String>,
    pub hosts: Vec<LoadedBy>,
}

/// Keeps at most this many lines per log file (the first and last halves).
const MAX_LINES_PER_FILE: usize = 120;

pub fn read_dll_logs() -> DllLog {
    let mut log = DllLog {
        lines: Vec::new(),
        hosts: Vec::new(),
    };
    for path in dll_logs() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for host in lines.iter().filter_map(|l| LoadedBy::parse(l)) {
            if !log.hosts.contains(&host) {
                log.hosts.push(host);
            }
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if lines.len() > MAX_LINES_PER_FILE {
            let half = MAX_LINES_PER_FILE / 2;
            log.lines
                .extend(lines[..half].iter().map(|l| format!("{name}: {l}")));
            log.lines.push(format!(
                "{name}: ... {} lines omitted ...",
                lines.len() - 2 * half
            ));
            log.lines.extend(
                lines[lines.len() - half..]
                    .iter()
                    .map(|l| format!("{name}: {l}")),
            );
        } else {
            log.lines
                .extend(lines.iter().map(|l| format!("{name}: {l}")));
        }
    }
    log
}
