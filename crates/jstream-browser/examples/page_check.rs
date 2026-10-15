//! The browser source's hardware check (Windows, feature `cef`): opens a page through
//! the CEF backend on the engine's own GPU device and media clock, as the daemon will,
//! and reports what arrives.
//!
//! ```text
//! cargo run --release -p jstream-browser --features cef --example page_check -- [--seconds 20] [--size 1920x1080] [--fps 60] [--url https://…]
//! ```
//!
//! The built-in test page is transparent, moves a box on every frame, and plays 440 Hz
//! on the left and 660 Hz on the right at -12 dBFS. Halfway through, the check gives the
//! page new custom CSS that sets `--jstream-tone: 880` on `:root`; the page reads that
//! property and moves its right channel to 880 Hz, which shows the CSS reached it. Then
//! the page is closed and the backend dropped, which shuts CEF down.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows_check::main()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "the page check runs on Windows only");
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
mod windows_check {
    use std::io::Write as _;
    use std::process::ExitCode;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use jstream_browser::{
        BrowserBackend, CefBackend, CefConfig, DEFAULT_CSS, PageLocation, PageSpec,
    };
    use jstream_media::{FrameSize, MediaTime, PixelFormat};
    use jstream_platform::{AudioInputHandler, AudioPacket, GpuInterop, PlatformError};
    use jstream_platform_windows::{QpcClock, WindowsGpu};

    const TEST_PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>JStream page check</title></head>
<body>
<div id="box" style="position:absolute;width:120px;height:120px;background:rgba(255,0,0,0.5)"></div>
<script>
const box = document.getElementById('box');
function frame(t) {
  box.style.left = (Math.sin(t / 500) * 0.4 + 0.5) * (innerWidth - 120) + 'px';
  box.style.top = (Math.cos(t / 700) * 0.4 + 0.5) * (innerHeight - 120) + 'px';
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
const ctx = new AudioContext({ sampleRate: 48000 });
const merger = ctx.createChannelMerger(2);
const gain = ctx.createGain();
gain.gain.value = 0.25;
const left = ctx.createOscillator();
left.frequency.value = 440;
left.connect(merger, 0, 0);
const right = ctx.createOscillator();
right.frequency.value = 660;
right.connect(merger, 0, 1);
merger.connect(gain).connect(ctx.destination);
left.start();
right.start();
setInterval(() => {
  const tone = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--jstream-tone'));
  if (tone > 0) right.frequency.value = tone;
}, 100);
</script>
</body></html>
"#;

    struct Args {
        seconds: f64,
        size: (u32, u32),
        fps: u32,
        url: Option<String>,
    }

    fn parse() -> Result<Args, String> {
        let mut args = Args {
            seconds: 20.0,
            size: (1920, 1080),
            fps: 60,
            url: None,
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or(format!("{flag} needs a value"));
            match flag.as_str() {
                "--seconds" => args.seconds = value()?.parse().map_err(|e| format!("{e}"))?,
                "--fps" => args.fps = value()?.parse().map_err(|e| format!("{e}"))?,
                "--url" => args.url = Some(value()?),
                "--size" => {
                    let v = value()?;
                    let (w, h) = v.split_once('x').ok_or("--size is WIDTHxHEIGHT")?;
                    args.size = (
                        w.parse().map_err(|e| format!("{e}"))?,
                        h.parse().map_err(|e| format!("{e}"))?,
                    );
                }
                other => return Err(format!("unknown option {other}")),
            }
        }
        Ok(args)
    }

    /// What the audio handler saw, per window.
    #[derive(Default)]
    struct Heard {
        packets: u64,
        frames: u64,
        first_position: Option<u64>,
        last_position: u64,
        unstamped: u64,
        errors: Vec<String>,
        peak: [f32; 2],
        /// Positive-going zero crossings per channel.
        crossings: [u64; 2],
        last: [f32; 2],
        started: Option<Instant>,
    }

    struct Listener(Arc<Mutex<Heard>>);

    impl AudioInputHandler for Listener {
        fn on_packet(&mut self, packet: &AudioPacket<'_>) {
            let mut h = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            h.started.get_or_insert_with(Instant::now);
            h.packets += 1;
            h.first_position.get_or_insert(packet.device_position);
            let frames = packet.samples.len() as u64 / 2;
            h.frames += frames;
            h.last_position = packet.device_position + frames;
            if packet.time.is_none() {
                h.unstamped += 1;
            }
            for pair in packet.samples.chunks_exact(2) {
                for (ch, &s) in pair.iter().enumerate() {
                    h.peak[ch] = h.peak[ch].max(s.abs());
                    if h.last[ch] <= 0.0 && s > 0.0 {
                        h.crossings[ch] += 1;
                    }
                    h.last[ch] = s;
                }
            }
        }

        fn on_error(&mut self, error: &PlatformError) {
            let mut h = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            h.errors.push(error.to_string());
        }
    }

    /// Distinct frames seen while polling the page.
    #[derive(Default)]
    struct Seen {
        frames: u64,
        last_pts: Option<MediaTime>,
        first_at: Option<f64>,
        size: Option<(u32, u32)>,
        format: Option<PixelFormat>,
    }

    fn line(text: impl std::fmt::Display) {
        let _ = writeln!(std::io::stdout(), "{text}");
    }

    fn dbfs(peak: f32) -> f64 {
        20.0 * f64::from(peak.max(1e-9)).log10()
    }

    fn report_audio(name: &str, heard: &Heard) {
        let secs = heard.frames as f64 / 48_000.0;
        let wall = heard
            .started
            .map_or(f64::NAN, |s| s.elapsed().as_secs_f64());
        let lost = heard.first_position.map_or(0, |first| {
            (heard.last_position - first).saturating_sub(heard.frames)
        });
        line(format_args!(
            "audio ({name}): {} packets, {} frames ({secs:.2} s of audio in {wall:.2} s), {lost} frames lost, {} unstamped",
            heard.packets, heard.frames, heard.unstamped
        ));
        for (ch, label) in ["left", "right"].iter().enumerate() {
            let hz = if secs > 0.0 {
                heard.crossings[ch] as f64 / secs
            } else {
                f64::NAN
            };
            line(format_args!(
                "  {label}: {hz:.1} Hz, peak {:.1} dBFS",
                dbfs(heard.peak[ch])
            ));
        }
        for e in &heard.errors {
            line(format_args!("  stream error: {e}"));
        }
    }

    pub(super) fn main() -> ExitCode {
        let args = match parse() {
            Ok(a) => a,
            Err(e) => {
                line(format_args!("error: {e}"));
                return ExitCode::from(2);
            }
        };
        match run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                line(format_args!("FAILED: {e}"));
                ExitCode::FAILURE
            }
        }
    }

    fn run(args: &Args) -> Result<(), String> {
        let clock = Arc::new(QpcClock::new().map_err(|e| e.to_string())?);
        let gpu = WindowsGpu::new()
            .open_device()
            .map_err(|e| format!("opening the engine's GPU device: {e}"))?;
        line(format_args!(
            "engine device: {} ({:?})",
            gpu.adapter().get_info().name,
            gpu.adapter_id()
        ));
        let state = std::env::temp_dir().join("jstream-page-check");
        let mut config = CefConfig::beside_exe(&state).map_err(|e| e.to_string())?;
        config.log_file = Some(state.join("cef.log"));
        line(format_args!(
            "helper: {}, CEF log: {}",
            config.helper.display(),
            state.join("cef.log").display()
        ));
        let backend = CefBackend::new(config, clock);
        backend.use_device(Some(&gpu));

        let location = match &args.url {
            Some(url) => PageLocation::web(url).map_err(|e| e.to_string())?,
            None => {
                let path = state.join("page-check.html");
                std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
                std::fs::write(&path, TEST_PAGE).map_err(|e| e.to_string())?;
                PageLocation::File(path)
            }
        };
        let spec = PageSpec {
            location,
            size: FrameSize::new(args.size.0, args.size.1).map_err(|e| e.to_string())?,
            fps: args.fps,
            css: DEFAULT_CSS.to_owned(),
        };
        line(format_args!("opening {}", spec.location.to_url()));
        let started = Instant::now();
        let mut page = backend.open(&spec).map_err(|e| e.to_string())?;
        line(format_args!(
            "CEF started and the page was asked for in {:.0} ms",
            started.elapsed().as_secs_f64() * 1e3
        ));

        let first = Arc::new(Mutex::new(Heard::default()));
        let stream = page.audio().open(Box::new(Listener(Arc::clone(&first))));
        let mut seen = Seen::default();
        let half = Duration::from_secs_f64(args.seconds / 2.0);
        let end = Duration::from_secs_f64(args.seconds);
        let mut second: Option<(Arc<Mutex<Heard>>, _)> = None;
        let mut css_at = None;
        let poll = Duration::from_millis(2);
        let mut window_frames = 0u64;
        let mut window_start = None;
        while started.elapsed() < end {
            if let Some(frame) = page.frame() {
                let pts = frame.info().pts;
                if seen.last_pts != Some(pts) {
                    seen.last_pts = Some(pts);
                    seen.frames += 1;
                    seen.first_at
                        .get_or_insert_with(|| started.elapsed().as_secs_f64() * 1e3);
                    seen.size = Some((frame.info().size.width(), frame.info().size.height()));
                    seen.format = Some(frame.info().format);
                    if started.elapsed() > Duration::from_secs(3) {
                        window_frames += 1;
                        window_start.get_or_insert_with(Instant::now);
                    }
                }
            }
            if css_at.is_none() && started.elapsed() >= half {
                page.set_css(&format!("{DEFAULT_CSS}\n:root {{ --jstream-tone: 880; }}"));
                css_at = Some(Instant::now());
            }
            // A second listener, a second after the CSS changed, hears the new tone.
            if second.is_none() && css_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                let heard = Arc::new(Mutex::new(Heard::default()));
                let s = page.audio().open(Box::new(Listener(Arc::clone(&heard))));
                second = Some((heard, s));
            }
            std::thread::sleep(poll);
        }

        line("");
        line(format_args!(
            "video: {} distinct frames, first at {:.0} ms, {:?} {:?}",
            seen.frames,
            seen.first_at.unwrap_or(f64::NAN),
            seen.size,
            seen.format
        ));
        if let Some(start) = window_start {
            line(format_args!(
                "  {:.2} frames a second after the first 3 s (target {})",
                window_frames as f64 / start.elapsed().as_secs_f64(),
                args.fps
            ));
        }
        report_audio(
            "first half",
            &first.lock().unwrap_or_else(PoisonError::into_inner),
        );
        drop(stream);
        if let Some((heard, s)) = second {
            report_audio(
                "after the CSS change",
                &heard.lock().unwrap_or_else(PoisonError::into_inner),
            );
            drop(s);
        }
        line(format_args!(
            "audio packets dropped while attaching: {}",
            page.audio().dropped()
        ));

        let closing = Instant::now();
        drop(page);
        drop(backend);
        line(format_args!(
            "page closed and CEF shut down in {:.0} ms",
            closing.elapsed().as_secs_f64() * 1e3
        ));
        if seen.frames == 0 {
            return Err("no frame arrived".into());
        }
        Ok(())
    }
}
