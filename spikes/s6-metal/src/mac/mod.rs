//! One run of the pipeline under one synchronisation mode.
//!
//! Each tick, for frame `n` in ring slot `s`:
//! 1. **Produce:** the producer device renders frame `n` into slot `s`'s BGRA IOSurface,
//!    with code `code(n)` in the import probe block.
//! 2. **Import:** the engine device reads that IOSurface through its own wrapping of it, and
//!    converts it into slot `s`'s NV12 IOSurface: the import probe is copied exactly, and
//!    the export probe gets the engine's own `code(n)`.
//! 3. **Hand over:** the consumer thread waits for the engine's work, reads both probes on
//!    the CPU, and gives the pixel buffer to VideoToolbox. The slot is free again once
//!    VideoToolbox's output callback has run for it.
//!
//! `--sync` decides what orders step 2 after step 1, and step 3 after step 2:
//! - `none`: nothing. The control: it should show stale or garbled probes.
//! - `completion`: the tick thread waits on the CPU for the producer's submission to
//!   complete before submitting the engine's, and the consumer waits for the engine's.
//! - `event`: the producer's submit signals `MTLSharedEvent` `in` to `n + 1`, the
//!   engine's submit waits for it on the GPU and signals `out` to `n + 1`, and the consumer
//!   waits for `out` on the CPU (`waitUntilSignaledValue:timeoutMS:`). The waits and signals
//!   are staged with wgpu-hal's `add_wait_event` and `add_signal_event`.
//! - `event-strict`: as `event`, with `enable_strict_event_sync` on the engine's queue, which
//!   gates every command buffer of the submit on the wait rather than only the next one.

mod encode;
mod gpu;
mod surface;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use objc2_metal::MTLSharedEvent;
use wgpu::PollType;

use crate::cli::{Args, Mode};
use crate::err::{Context, Result};
use crate::stats::{Report, Samples};

use gpu::{Converter, Gpu, PROBE, Producer, SharedEvent};
use surface::{BGRA, NV12, Surface};

/// Ring slots. Enough for two frames on the GPU and VideoToolbox's three-frame delay.
const RING: usize = 8;
/// How long any one wait may take before it counts as a sync timeout.
const WAIT: Duration = Duration::from_secs(2);

/// The probe code for frame `n`: 3 to 252, so 0 (cleared), 255 and near-black never match
/// a frame by accident.
fn code(n: u64) -> u8 {
    (n % 250) as u8 + 3
}

/// Whether `v` is some frame's code.
fn is_code(v: u8) -> bool {
    (3..=252).contains(&v)
}

/// The shared events, which the tick and consumer threads both use.
struct Events {
    input: SharedEvent,
    output: SharedEvent,
}

// SAFETY: `MTLSharedEvent` is thread-safe (Metal documents shared events for signalling
// across threads, queues and processes); the spike only signals, waits on and reads them.
unsafe impl Send for Events {}
// SAFETY: as above.
unsafe impl Sync for Events {}

/// One ring slot: the producer's BGRA surface and the engine's NV12 target, with the
/// textures and views wrapping them on each device.
struct Slot {
    /// Kept alive for the textures that wrap it.
    _source: Surface,
    target: Surface,
    producer_view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    y: wgpu::TextureView,
    uv: wgpu::TextureView,
    _textures: Vec<wgpu::Texture>,
}

impl Slot {
    fn new(
        engine: &Gpu,
        producer: &Gpu,
        converter: &Converter,
        (w, h): (u32, u32),
    ) -> Result<Self> {
        let source = Surface::new(w, h, BGRA)?;
        let target = Surface::new(w, h, NV12)?;
        let produced = producer.import(
            &source.surface,
            0,
            wgpu::TextureFormat::Bgra8Unorm,
            (w, h),
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            "source (producer)",
        )?;
        let read = engine.import(
            &source.surface,
            0,
            wgpu::TextureFormat::Bgra8Unorm,
            (w, h),
            wgpu::TextureUsages::TEXTURE_BINDING,
            "source (engine)",
        )?;
        let y = engine.import(
            &target.surface,
            0,
            wgpu::TextureFormat::R8Unorm,
            (w, h),
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            "target Y",
        )?;
        let uv = engine.import(
            &target.surface,
            1,
            wgpu::TextureFormat::Rg8Unorm,
            (w / 2, h / 2),
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            "target CbCr",
        )?;
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self {
            producer_view: view(&produced),
            bind_group: converter.bind(&engine.device, &view(&read)),
            y: view(&y),
            uv: view(&uv),
            _source: source,
            target,
            _textures: vec![produced, read, y, uv],
        })
    }
}

/// A frame on its way to the consumer.
struct Job {
    n: u64,
    slot: usize,
    index: wgpu::SubmissionIndex,
    start: Instant,
}

/// What the consumer counted.
#[derive(Default)]
struct Checked {
    frames: u64,
    stale_import: u64,
    stale_export: u64,
    garbled: u64,
    timeouts: u64,
    latency: Samples,
    warnings: Vec<String>,
}

fn stage(what: &str) {
    eprintln!("s6 stage: {what}");
}

/// Runs `mode` for `args.seconds` and reports.
pub fn run(args: &Args, mode: Mode) -> Result<Report> {
    stage(&format!("{}: devices", mode.name()));
    let (info, engine, producer) = gpu::open()?;
    if mode == Mode::EventStrict {
        engine.enable_strict_event_sync()?;
    }
    let events = Events {
        input: engine.shared_event()?,
        output: engine.shared_event()?,
    };
    stage("surfaces");
    let producer_pipe = Producer::new(&producer.device);
    let converter = Converter::new(&engine.device);
    let slots = (0..RING)
        .map(|_| Slot::new(&engine, &producer, &converter, args.size))
        .collect::<Result<Vec<_>>>()?;
    let (free_tx, free_rx) = mpsc::channel::<usize>();
    for s in 0..RING {
        free_tx.send(s).context("filling the ring")?;
    }
    stage("encoder");
    let encoder = encode::Encoder::new(args.size, args.fps, args.bitrate_kbps, free_tx.clone())?;
    let hardware = encoder.hardware;
    let (job_tx, job_rx) = mpsc::channel::<Job>();

    let mut ticks = 0u64;
    let mut submitted = 0u64;
    let mut late = 0u64;
    let mut ring_full = 0u64;
    let mut tick_timeouts = 0u64;
    let mut tick_work = Samples::default();
    let cpu_start = cpu_seconds();
    let started = Instant::now();
    stage("running");
    let checked = std::thread::scope(|scope| -> Result<Checked> {
        let consumer = scope.spawn(|| consume(mode, &engine, &events, &slots, &encoder, job_rx));
        let period = Duration::from_secs_f64(1.0 / f64::from(args.fps));
        let end = started + Duration::from_secs_f64(args.seconds);
        let mut deadline = started;
        let mut n = 0u64;
        while Instant::now() < end {
            deadline += period;
            let now = Instant::now();
            if now < deadline {
                std::thread::sleep(deadline - now);
            } else if now > deadline + period {
                late += 1;
                deadline = now;
                continue;
            }
            ticks += 1;
            producer.device.poll(PollType::Poll).ok();
            engine.device.poll(PollType::Poll).ok();
            let Ok(slot) = free_rx.try_recv() else {
                ring_full += 1;
                continue;
            };
            let t0 = Instant::now();
            let s = &slots[slot];
            let c = code(n);
            let cb = producer_pipe.record(&producer, &s.producer_view, c, args.load, n as u32);
            if mode.uses_events() {
                producer.stage_signal(&events.input, n + 1)?;
            }
            let produced = producer.queue.submit([cb]);
            if mode == Mode::Completion
                && producer
                    .device
                    .poll(PollType::Wait {
                        submission_index: Some(produced),
                        timeout: Some(WAIT),
                    })
                    .is_err()
            {
                tick_timeouts += 1;
            }
            let cb = converter.record(&engine, &s.bind_group, &s.y, &s.uv, c);
            if mode.uses_events() {
                engine.stage_wait(&events.input, n + 1)?;
                engine.stage_signal(&events.output, n + 1)?;
            }
            let index = engine.queue.submit([cb]);
            tick_work.push(t0.elapsed().as_secs_f64() * 1000.0);
            job_tx
                .send(Job {
                    n,
                    slot,
                    index,
                    start: t0,
                })
                .context("the consumer stopped")?;
            submitted += 1;
            n += 1;
        }
        drop(job_tx);
        consumer
            .join()
            .map_err(|_| crate::err::Error::msg("the consumer thread panicked"))?
    })?;
    let measured = started.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu_start;
    stage("draining");
    let (stream, encoded, failed) = encoder.finish()?;
    let out = args.out_for(mode);
    std::fs::write(&out, &stream).with_context(|| format!("writing {}", out.display()))?;

    let mut warnings = checked.warnings;
    let same = engine.mtl_address() == producer.mtl_address();
    if failed > 0 {
        warnings.push(format!("VideoToolbox failed or dropped {failed} frame(s)"));
    }
    if tick_timeouts > 0 {
        warnings.push(format!(
            "{tick_timeouts} producer wait(s) timed out after {} s",
            WAIT.as_secs()
        ));
    }
    Ok(Report {
        mode: mode.name(),
        macos: command("sw_vers", &["-productVersion"]),
        machine: format!(
            "{} ({})",
            command("sysctl", &["-n", "hw.model"]),
            command("sysctl", &["-n", "machdep.cpu.brand_string"])
        ),
        adapter_name: info.name,
        wgpu_driver: format!("{} {}", info.driver, info.driver_info),
        same_mtl_device: same,
        encoder_hardware: hardware,
        size: args.size,
        target_fps: args.fps,
        load: args.load,
        seconds_requested: args.seconds,
        seconds_measured: measured,
        ticks,
        frames_submitted: submitted,
        frames_checked: checked.frames,
        stale_import: checked.stale_import,
        stale_export: checked.stale_export,
        garbled: checked.garbled,
        sync_timeouts: checked.timeouts + tick_timeouts,
        dropped_late_ticks: late,
        dropped_ring_full: ring_full,
        frames_encoded: encoded,
        latency_ms: checked.latency.summary(),
        tick_work_ms: tick_work.summary(),
        cpu_percent_of_one_core: if measured > 0.0 {
            100.0 * cpu / measured
        } else {
            f64::NAN
        },
        output_path: out.display().to_string(),
        output_bytes: stream.len() as u64,
        warnings,
    })
}

/// The consumer thread: waits for each frame as `mode` says, checks its probes and
/// encodes it.
fn consume(
    mode: Mode,
    engine: &Gpu,
    events: &Events,
    slots: &[Slot],
    encoder: &encode::Encoder,
    jobs: mpsc::Receiver<Job>,
) -> Result<Checked> {
    let mut c = Checked::default();
    let wait_ms = WAIT.as_millis() as u64;
    for job in jobs {
        let ready = match mode {
            Mode::None => true,
            Mode::Completion => engine
                .device
                .poll(PollType::Wait {
                    submission_index: Some(job.index),
                    timeout: Some(WAIT),
                })
                .is_ok(),
            Mode::Event | Mode::EventStrict => events
                .output
                .waitUntilSignaledValue_timeoutMS(job.n + 1, wait_ms),
        };
        if !ready {
            c.timeouts += 1;
        }
        c.latency.push(job.start.elapsed().as_secs_f64() * 1000.0);
        let target = &slots[job.slot].target;
        let mut import = [0u8; 1];
        let mut export = [0u8; 1];
        let half = PROBE as usize / 2;
        target.read(0, half, half, &mut import)?;
        target.read(0, PROBE as usize + half, half, &mut export)?;
        let want = code(job.n);
        c.frames += 1;
        // The export probe says whether this is the engine's frame `n` at all; only then
        // does the import probe say anything about the producer's.
        if export[0] != want {
            if is_code(export[0]) {
                c.stale_export += 1;
            } else {
                c.garbled += 1;
            }
        } else if import[0] != want {
            if is_code(import[0]) {
                c.stale_import += 1;
            } else {
                c.garbled += 1;
            }
        }
        if let Err(e) = encoder.encode(&slots[job.slot].target.buffer, job.n, job.slot) {
            if c.warnings.len() < 8 {
                c.warnings.push(format!("frame {}: {e}", job.n));
            }
            // VideoToolbox won't call back for it, so give the slot back here.
            encoder.release(job.slot);
        }
    }
    Ok(c)
}

/// The process's user and system CPU time so far, in seconds.
fn cpu_seconds() -> f64 {
    // SAFETY: `getrusage` fills the zeroed struct it is given.
    let usage = unsafe {
        let mut usage = std::mem::zeroed::<libc::rusage>();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let secs = |t: libc::timeval| t.tv_sec as f64 + f64::from(t.tv_usec) / 1e6;
    secs(usage.ru_utime) + secs(usage.ru_stime)
}

/// A command's first line of output, or `unknown`.
fn command(program: &str, args: &[&str]) -> String {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}
