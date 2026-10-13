//! The four commands: register, unregister, status, and run (produce + camera + report).

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use s2_vcam::proto::{CLSID_STR, FPS, FRIENDLY_NAME, HEIGHT, WIDTH};
use s2_vcam::win::reg::{self, RegScope};
use s2_vcam::win::shared::{ensure_log_dir, log_dir};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFVirtualCamera, MF_VERSION, MFCreateVirtualCamera, MFSTARTUP_FULL,
    MFShutdown, MFStartup, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows_core::HSTRING;

use crate::cli::{Args, Command, Scope};
use crate::err::{Context, Result, bail};
use crate::json::Json;
use crate::producer::Producer;
use crate::sys;

pub fn main(args: &Args) -> Result<ExitCode> {
    match args.command {
        Command::Register { scope, acl } => register(scope, acl),
        Command::Unregister => unregister(),
        Command::Status => status(args.json),
        Command::Run { seconds, fps, vcam } => run(seconds, fps, vcam, args.json),
    }
}

fn reg_scope(s: Scope) -> RegScope {
    match s {
        Scope::User => RegScope::User,
        Scope::Machine => RegScope::Machine,
    }
}

fn register(scope: Scope, acl: bool) -> Result<ExitCode> {
    let scope = reg_scope(scope);
    let dll = sys::dll_next_to_exe()?;
    if !dll.exists() {
        bail!(
            "{} not found; run `cargo build --release` in spikes\\s2-vcam first",
            dll.display()
        );
    }
    if scope == RegScope::Machine && !sys::is_elevated() {
        bail!(
            "--register hklm needs an elevated prompt (right-click the terminal, Run as administrator)"
        );
    }
    reg::register(scope, &dll)
        .with_context(|| format!("writing the {} registration", scope.as_str()))?;
    println!(
        "registered {CLSID_STR} in {} -> {}",
        scope.as_str(),
        dll.display()
    );
    for other in RegScope::ALL.into_iter().filter(|s| *s != scope) {
        if let Some(p) = reg::registered_path(other) {
            println!(
                "warning: also registered in {} ({p}); run --unregister first to test one mode at a time",
                other.as_str()
            );
        }
    }
    if acl {
        match sys::grant_dll_access(&dll) {
            Ok(out) => println!("granted LOCAL SERVICE read/execute on the DLL:\n{out}"),
            Err(e) => println!("warning: could not grant LOCAL SERVICE access to the DLL: {e}"),
        }
    } else {
        println!("--no-acl: the DLL's ACL was left unchanged");
    }
    println!("re-run this after every `cargo build`: a rebuilt DLL loses the grant");
    Ok(ExitCode::SUCCESS)
}

fn unregister() -> Result<ExitCode> {
    let mut code = ExitCode::SUCCESS;
    for scope in RegScope::ALL {
        match reg::unregister(scope) {
            Ok(true) => println!("{}: removed", scope.as_str()),
            Ok(false) => println!("{}: nothing registered", scope.as_str()),
            Err(e) => {
                let hint = if scope == RegScope::Machine && !sys::is_elevated() {
                    " (run from an elevated prompt to remove the HKLM entry)"
                } else {
                    ""
                };
                println!("{}: failed: {e}{hint}", scope.as_str());
                code = ExitCode::FAILURE;
            }
        }
    }
    Ok(code)
}

fn status(json: bool) -> Result<ExitCode> {
    let report = Json::Obj(vec![
        ("registration", sys::registration()),
        ("elevated", Json::Bool(sys::is_elevated())),
        ("windows", sys::windows_version()),
        ("log_dir", Json::str(log_dir().display().to_string())),
    ]);
    if json {
        println!("{report}");
    } else {
        for scope in RegScope::ALL {
            println!(
                "{}: {}",
                scope.as_str(),
                reg::registered_path(scope).unwrap_or_else(|| "(not registered)".into())
            );
        }
        if let Ok(dll) = sys::dll_next_to_exe() {
            println!(
                "DLL next to this exe: {} ({})",
                dll.display(),
                if dll.exists() { "present" } else { "missing" }
            );
        }
        println!("DLL logs: {}", log_dir().display());
    }
    Ok(ExitCode::SUCCESS)
}

/// The outcome of one Media Foundation call, for the report.
fn outcome<T>(r: &windows_core::Result<T>) -> Json {
    match r {
        Ok(_) => Json::Obj(vec![("hr", Json::hr(0)), ("error", Json::Null)]),
        Err(e) => Json::Obj(vec![
            ("hr", Json::hr(e.code().0)),
            ("error", Json::str(e.message())),
        ]),
    }
}

fn run(seconds: Option<f64>, fps: u32, use_vcam: bool, json: bool) -> Result<ExitCode> {
    // SAFETY: called once on this thread before any COM use; "already initialised" is fine.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // SAFETY: plain call; paired with MFShutdown below.
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.context("MFStartup")?;
    let result = run_inner(seconds, fps, use_vcam, json);
    // SAFETY: paired with MFStartup; every MF object was dropped in run_inner.
    let _ = unsafe { MFShutdown() };
    result
}

fn run_inner(seconds: Option<f64>, fps: u32, use_vcam: bool, json: bool) -> Result<ExitCode> {
    let started = Instant::now();
    let mut errors: Vec<String> = Vec::new();
    let registration = sys::registration();
    if reg::registered_path(RegScope::User).is_none()
        && reg::registered_path(RegScope::Machine).is_none()
    {
        let msg = "the media source is not registered; run --register hkcu (or hklm) first";
        eprintln!("warning: {msg}");
        errors.push(msg.into());
    }
    if let Err(e) = ensure_log_dir() {
        errors.push(format!(
            "creating the log folder {}: {e}",
            log_dir().display()
        ));
    }
    let stuck_logs = sys::clear_dll_logs();

    let producer = Producer::new()?;
    let mut frame = 1u64;
    producer.render(frame)?;
    eprintln!(
        "producer: {}x{} BGRA ring in the {} namespace on {} (control block {})",
        WIDTH,
        HEIGHT,
        producer.setup.scope.as_str(),
        producer.setup.adapter,
        producer.setup.control_name
    );

    // The camera. Session lifetime: it disappears when this process exits.
    let (create, start, vcam) = if use_vcam {
        // SAFETY: plain call with valid wide strings; no categories (system default).
        let create = unsafe {
            MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                &HSTRING::from(FRIENDLY_NAME),
                &HSTRING::from(CLSID_STR),
                None,
            )
        };
        let (start, vcam) = match &create {
            Ok(v) => {
                // SAFETY: plain call on a live COM object; no callback means synchronous.
                let s = unsafe { v.Start(None::<&IMFAsyncCallback>) };
                (Some(s), Some(v.clone()))
            }
            Err(_) => (None, None),
        };
        (Some(create.map(|_| ())), start, vcam)
    } else {
        (None, None, None)
    };
    match (&create, &start) {
        (Some(Err(e)), _) => eprintln!("MFCreateVirtualCamera failed: {e}"),
        (_, Some(Err(e))) => eprintln!("IMFVirtualCamera::Start failed: {e}"),
        (Some(Ok(())), Some(Ok(()))) => eprintln!(
            "camera \"{FRIENDLY_NAME}\" started. Open the Camera app (or a browser camera test) and \
             pick it. The producer's picture is dark teal with a moving white bar; colour bars \
             mean the DLL runs but can't reach the producer."
        ),
        _ => {}
    }

    // Feed frames until Enter or --seconds.
    let stop = Arc::new(AtomicBool::new(false));
    match seconds {
        Some(s) => eprintln!("running for {s} s"),
        None => {
            eprintln!("press Enter to stop");
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                stop.store(true, Ordering::Release);
            });
        }
    }
    let period = Duration::from_secs_f64(1.0 / f64::from(fps));
    let deadline = seconds.map(|s| started + Duration::from_secs_f64(s));
    let mut next = Instant::now() + period;
    let mut next_progress = Instant::now() + Duration::from_secs(2);
    let mut render_error = None;
    while !stop.load(Ordering::Acquire) && deadline.is_none_or(|d| Instant::now() < d) {
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        next += period;
        frame += 1;
        if let Err(e) = producer.render(frame) {
            render_error = Some(e.to_string());
            break;
        }
        if Instant::now() >= next_progress {
            next_progress += Duration::from_secs(2);
            let c = producer.control();
            eprintln!(
                "frame {frame} | DLL: attaches {} served {} (shared {}, pattern {}) last_hr {:#010x}",
                c.consumer_attaches.load(Ordering::Acquire),
                c.frames_served.load(Ordering::Acquire),
                c.frames_shared.load(Ordering::Acquire),
                c.frames_pattern.load(Ordering::Acquire),
                c.consumer_last_hr.load(Ordering::Acquire) as u32,
            );
        }
    }
    if let Some(e) = &render_error {
        errors.push(format!("render: {e}"));
    }
    producer.stop();

    let (stop_r, shutdown_r) = match &vcam {
        Some(v) => (Some(stop_camera(v)), Some(shutdown_camera(v))),
        None => (None, None),
    };
    drop(vcam);

    let elapsed = started.elapsed().as_secs_f64();
    let c = producer.control();
    let dll_log = sys::read_dll_logs();
    if let Some(Err(e)) = &create {
        errors.push(format!("MFCreateVirtualCamera: {e}"));
    }
    if let Some(Err(e)) = &start {
        errors.push(format!("IMFVirtualCamera::Start: {e}"));
    }
    for s in &stuck_logs {
        errors.push(format!("could not clear an old DLL log: {s}"));
    }
    let setup = &producer.setup;
    let report = Json::Obj(vec![
        ("spike", Json::str("s2-vcam")),
        ("windows", sys::windows_version()),
        ("elevated", Json::Bool(sys::is_elevated())),
        ("registration", registration),
        (
            "shared",
            Json::Obj(vec![
                ("scope", Json::str(setup.scope.as_str())),
                ("global_error", Json::opt_str(setup.global_error.clone())),
                ("control_name", Json::str(setup.control_name.clone())),
                ("control_existed", Json::Bool(setup.control_existed)),
                ("session_id", Json::num(setup.session_id)),
                ("adapter", Json::str(setup.adapter.clone())),
                (
                    "adapter_luid",
                    Json::str(format!("{:#x}", setup.adapter_luid)),
                ),
            ]),
        ),
        (
            "vcam",
            Json::Obj(vec![
                ("attempted", Json::Bool(use_vcam)),
                ("create", create.as_ref().map_or(Json::Null, outcome)),
                ("start", start.as_ref().map_or(Json::Null, outcome)),
                ("stop", stop_r.as_ref().map_or(Json::Null, outcome)),
                ("shutdown", shutdown_r.as_ref().map_or(Json::Null, outcome)),
            ]),
        ),
        (
            "producer",
            Json::Obj(vec![
                ("fps_target", Json::num(fps)),
                ("frames", Json::num(frame as f64)),
                ("seconds", Json::num(elapsed)),
                ("camera_fps", Json::num(FPS)),
            ]),
        ),
        (
            "consumer",
            Json::Obj(vec![
                ("pid", Json::num(c.consumer_pid.load(Ordering::Acquire))),
                (
                    "attaches",
                    Json::num(c.consumer_attaches.load(Ordering::Acquire)),
                ),
                ("gpu_opens", Json::num(c.gpu_opens.load(Ordering::Acquire))),
                (
                    "frames_served",
                    Json::num(c.frames_served.load(Ordering::Acquire) as f64),
                ),
                (
                    "frames_shared",
                    Json::num(c.frames_shared.load(Ordering::Acquire) as f64),
                ),
                (
                    "frames_pattern",
                    Json::num(c.frames_pattern.load(Ordering::Acquire) as f64),
                ),
                (
                    "last_hr",
                    Json::hr(c.consumer_last_hr.load(Ordering::Acquire)),
                ),
            ]),
        ),
        (
            "dll_log",
            Json::Obj(vec![
                ("dir", Json::str(log_dir().display().to_string())),
                (
                    "frame_server_loaded",
                    Json::Bool(dll_log.frame_server_loaded),
                ),
                (
                    "loaded_by",
                    Json::Arr(dll_log.loaded_by.into_iter().map(Json::Str).collect()),
                ),
                (
                    "lines",
                    Json::Arr(dll_log.lines.into_iter().map(Json::Str).collect()),
                ),
            ]),
        ),
        (
            "errors",
            Json::Arr(errors.iter().cloned().map(Json::Str).collect()),
        ),
    ]);

    if json {
        println!("{report}");
    } else {
        println!(
            "camera: create {} start {}",
            create.as_ref().map_or(Json::Null, outcome),
            start.as_ref().map_or(Json::Null, outcome)
        );
        println!(
            "DLL loaded by Frame Server: {} | samples served {} (from the producer {})",
            dll_log.frame_server_loaded,
            c.frames_served.load(Ordering::Acquire),
            c.frames_shared.load(Ordering::Acquire)
        );
        for e in &errors {
            println!("error: {e}");
        }
        println!("run again with --json for the full report to paste back");
    }
    Ok(if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn stop_camera(v: &IMFVirtualCamera) -> windows_core::Result<()> {
    // SAFETY: plain call on a live COM object.
    unsafe { v.Stop() }
}

fn shutdown_camera(v: &IMFVirtualCamera) -> windows_core::Result<()> {
    // SAFETY: plain call on a live COM object; nothing uses it afterwards.
    unsafe { v.Shutdown() }
}
