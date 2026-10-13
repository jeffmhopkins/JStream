//! The Windows run: start the streams, sample them once a second into the CSV, print
//! progress, and assemble the report.

mod audio;
mod sys;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::Args;
use crate::err::{Context, Result};
use crate::report::{self, Report, StreamReport};
use crate::stream::{Role, Snapshot};
use crate::track::Stat;

use audio::{Capture, Source};

pub fn run(args: &Args) -> Result<Report> {
    let _com = sys::Com::init()?;
    sys::install_ctrl_handler();
    let windows = sys::windows_version();
    let stop = Arc::new(AtomicBool::new(false));
    let settle_s = args.settle_s();
    let total_s = args.seconds();
    let mut warnings = Vec::new();

    // Silence first, so loopback has packets from its first moment.
    let (keepalive, keepalive_error) = if args.keepalive {
        match audio::spawn_keepalive(Arc::clone(&stop)) {
            Ok(k) => (Some(k), None),
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (None, None)
    };

    let origin_hns = sys::qpc_now_hns()?;
    let open =
        |source| audio::spawn_capture(source, args.rate, origin_hns, settle_s, Arc::clone(&stop));
    let mic = open(Source::Mic);
    let loopback = open(match args.process {
        Some(pid) => Source::Process(pid),
        None => Source::Loopback,
    });
    let (mic, loopback) = match (mic, loopback) {
        (Ok(m), Ok(l)) => (m, l),
        (m, l) => {
            stop.store(true, Ordering::SeqCst);
            let mut why = Vec::new();
            for r in [m, l] {
                match r {
                    Ok(c) => {
                        let _ = c.join.join();
                    }
                    Err(e) => why.push(e.to_string()),
                }
            }
            if let Some(k) = keepalive {
                let _ = k.join.join();
            }
            return Err(crate::err::Error::msg(why.join("; ")));
        }
    };
    eprintln!(
        "S3: mic = {} ({} Hz, {} ch), {} = {} ({} Hz, {} ch); {} min, residual statistics after {:.0} s",
        mic.info.device_name,
        mic.info.rate,
        mic.info.channels,
        loopback.info.role.key(),
        loopback.info.device_name,
        loopback.info.rate,
        loopback.info.channels,
        args.minutes,
        settle_s
    );
    if args.keepalive {
        eprintln!(
            "S3: play some audio (music or a video) for the whole run; Ctrl+C stops early with a report"
        );
    }

    let cpu0 = sys::process_cpu_hns();
    let started = Instant::now();
    let mut csv =
        BufWriter::new(File::create(&args.csv).context(format!("create {}", args.csv.display()))?);
    writeln!(csv, "{}", report::CSV_HEADER).context("write CSV")?;
    let mut rows = 0u64;
    let mut relative = Stat::default();
    let mut next_row = 1.0f64;
    let mut next_progress = args.progress_s;
    let mut interrupted = false;

    loop {
        let t = started.elapsed().as_secs_f64();
        if sys::ctrl_c_pressed() {
            interrupted = true;
            break;
        }
        if t >= total_s {
            break;
        }
        if mic.join.is_finished() || loopback.join.is_finished() {
            warnings.push(format!("a capture stream stopped early, at {t:.1} s"));
            break;
        }
        if t >= next_row {
            let (m, l) = (latest(&mic), latest(&loopback));
            writeln!(csv, "{}", report::csv_row(t, &m, &l)).context("write CSV")?;
            rows += 1;
            if rows.is_multiple_of(10) {
                csv.flush().context("flush CSV")?;
            }
            if t >= settle_s
                && let Some(r) = report::relative_residual(&m, &l)
            {
                relative.add(r);
            }
            if t >= next_progress {
                eprintln!("{}", report::progress_line(t, total_s, &m, &l));
                next_progress += args.progress_s;
            }
            next_row += 1.0;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let seconds_measured = started.elapsed().as_secs_f64();
    stop.store(true, Ordering::SeqCst);
    csv.flush().context("flush CSV")?;

    let mic_report = finish(mic);
    let loop_report = finish(loopback);
    let keepalive_device = keepalive.as_ref().map(|k| k.device.clone());
    let keepalive_error = match keepalive {
        Some(k) => k.join.join().ok().flatten().or(keepalive_error),
        None => keepalive_error,
    };
    let cpu_s = (sys::process_cpu_hns().saturating_sub(cpu0)) as f64 / 1e7;
    let logical_cpus = thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let one_core = 100.0 * cpu_s / seconds_measured.max(f64::EPSILON);

    for (name, r) in [("mic", &mic_report), ("loopback", &loop_report)] {
        if r.snap.packets == 0 {
            warnings.push(format!("{name}: no packets at all"));
        }
        if r.snap.fit_points < 2 {
            warnings.push(format!("{name}: too few timestamped packets to fit drift"));
        }
        if r.wait_timeouts > 0 {
            warnings.push(format!(
                "{name}: {} event waits timed out (no event for 200 ms)",
                r.wait_timeouts
            ));
        }
    }
    if loop_report
        .info
        .as_ref()
        .is_some_and(|i| i.role == Role::Loopback)
        && !args.keepalive
    {
        warnings
            .push("keep-alive off: loopback only delivers packets while something plays".into());
    }

    Ok(Report {
        windows,
        seconds_requested: total_s,
        seconds_measured,
        interrupted,
        settle_s,
        engine_rate: args.rate,
        keepalive: keepalive_device,
        keepalive_error,
        mic: mic_report,
        loopback: loop_report,
        relative_residual: relative,
        cpu_percent_of_one_core: one_core,
        cpu_percent_of_machine: one_core / f64::from(logical_cpus.max(1)),
        logical_cpus,
        csv_path: args.csv.display().to_string(),
        csv_rows: rows,
        warnings,
    })
}

/// The stream's latest published snapshot.
fn latest(c: &Capture) -> Snapshot {
    c.latest.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Joins a capture thread.
fn finish(c: Capture) -> StreamReport {
    let Capture { info, latest, join } = c;
    match join.join() {
        Ok(o) => StreamReport {
            info: Some(info),
            snap: o.snap,
            mmcss: o.mmcss,
            wait_timeouts: o.wait_timeouts,
            error: o.error,
        },
        Err(_) => StreamReport {
            info: Some(info),
            snap: latest.lock().map(|g| g.clone()).unwrap_or_default(),
            mmcss: false,
            wait_timeouts: 0,
            error: Some("capture thread panicked".into()),
        },
    }
}
