//! Streams to a real RTMP server and checks what it received (`cargo xtask e2e-stream`).
//!
//! `tests/e2e.rs` drives the daemon's wiring against a hand-written RTMP sink and fake
//! codecs, so nothing there is decodable. This harness swaps the fakes on the media
//! path for real parts: the GPU compositor on a software Vulkan adapter (lavapipe),
//! JStream's own AAC-LC encoder, a real H.264 encoder, and `ffmpeg` as the RTMP server
//! (`-listen 1`, writing what it receives to FLV). `ffprobe` and `ffmpeg` then read the
//! file back: codec, size, frame rate, keyframe spacing, audio, duration, and decoded
//! pixels and loudness. Only capture and the microphone stay fakes, on a fake clock the
//! pump keeps level with real time, so the encoders and the server see a live pace.
//!
//! The H.264 encoder is Cisco's OpenH264, the product's Linux software encoder, when
//! `JSTREAM_E2E_OPENH264` names its library (`--openh264` of the xtask). Otherwise
//! it is `x264.rs`'s stand-in, `ffmpeg`'s libx264 in a child process.
//!
//! The tests are `#[ignore]`d: they need `ffmpeg`, `ffprobe` and a Vulkan adapter, which
//! R-TEST-004 keeps out of `cargo test`. Run them with `cargo xtask e2e-stream`, or
//! `cargo test -p jstream-daemon --test receiver -- --ignored --test-threads=1`.

#![cfg(target_os = "linux")]
#![allow(
    clippy::print_stderr,
    reason = "the log shows what each receiver got, which a passing run's assertions don't"
)]

mod binary;
mod probe;
mod x264;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use jstream_control::Control;
use jstream_control::protocol::{
    Action, Batch, Call, CallResult, Event, Hello, OutputHealth, OutputId, OutputState,
    OutputStatsInfo, PROTOCOL_VERSION, Query, QueryResult, SecretString,
};
use jstream_core::{Engine, InProcessControl};
use jstream_daemon::live::{live, rtmp};
use jstream_media::{ChannelLayout, FrameSize, Timebase};
use jstream_platform::fake::{
    FakeAudioIo, FakeCapture, FakeClock, FakeMediaDecoders, FakeVideoDevices, MemoryKeyStore,
};
use jstream_platform::{
    AdapterId, AudioEncoder, AudioEncoderConfig, Backends, CodecInfo, Codecs, GpuContext, GpuFrame,
    GpuInterop, PlatformError, VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig,
};
use jstream_platform_linux::{OPENH264_PIN, OpenH264Pin, OpenH264Store, VaapiCodecs};
use jstream_render::{Compositor, OpenOptions};
use probe::{Receiver, free_port, mean_volume, pixel, probe};
use serde_json::{Value, json};

/// The stream key every receiver expects: a placeholder, not a secret.
const KEY: &str = "e2e-receiver-key";
/// The canvas: small, so lavapipe and the encoders keep up with real time.
const CANVAS: (u32, u32) = (320, 180);

/// A fresh, empty directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("receiver-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory is created");
    dir
}

// ---------------------------------------------------------------------------------------
// The backends: fake capture and microphone, real compositor and codecs

/// GPU interop on a software adapter, standing in for the platform's. It makes no
/// encode targets, so the program is read back for the CPU encoders.
struct SoftwareGpu;

impl GpuInterop for SoftwareGpu {
    fn open_device(&self) -> Result<GpuContext, PlatformError> {
        let compositor = Compositor::open(OpenOptions::headless()).map_err(|err| {
            PlatformError::Unsupported {
                what: format!("a software adapter: {err}"),
            }
        })?;
        Ok(GpuContext::new(
            compositor.adapter().clone(),
            compositor.device().clone(),
            compositor.queue().clone(),
            AdapterId {
                luid: None,
                vendor: 0,
            },
            None,
        ))
    }

    fn encode_target(
        &self,
        _context: &GpuContext,
        _size: FrameSize,
    ) -> Result<GpuFrame, PlatformError> {
        Err(PlatformError::Unsupported {
            what: "encode targets on the software adapter".into(),
        })
    }
}

/// Linux's codecs as the product has them, with no VA-API device: JStream's AAC-LC
/// encoder, and OpenH264 when [`openh264`] found Cisco's library, else the libx264
/// stand-in.
struct RealCodecs {
    linux: VaapiCodecs,
    openh264: bool,
}

/// Cisco's library from `JSTREAM_E2E_OPENH264`, stored in `dir` under a pin made from
/// its own SHA-256, so `VaapiCodecs` loads it the way it loads a download.
fn openh264(dir: &Path) -> Option<OpenH264Store> {
    let library = std::env::var_os("JSTREAM_E2E_OPENH264")?;
    let bytes = std::fs::read(&library).expect("JSTREAM_E2E_OPENH264 names a readable file");
    let output = std::process::Command::new("sha256sum")
        .arg(&library)
        .output()
        .expect("sha256sum runs");
    let hex = String::from_utf8_lossy(&output.stdout);
    let hex = hex.split_whitespace().next().expect("a SHA-256");
    let mut sha = [0u8; 32];
    for (i, byte) in sha.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex");
    }
    eprintln!("OpenH264 library SHA-256: {hex}");
    let pin = OpenH264Pin {
        library_sha256: Some(sha),
        ..OPENH264_PIN
    };
    let store = OpenH264Store::with_pin(dir.join("openh264"), pin);
    std::fs::create_dir_all(dir.join("openh264")).expect("the store's folder");
    std::fs::write(store.library_path(), bytes).expect("the library is stored");
    Some(store)
}

impl RealCodecs {
    fn new(dir: &Path) -> Self {
        match openh264(dir) {
            Some(store) => Self {
                linux: VaapiCodecs::new().with_openh264(store),
                openh264: true,
            },
            None => Self {
                linux: VaapiCodecs::new(),
                openh264: false,
            },
        }
    }

    /// The video encoder's name, as `output.status` and the logs give it.
    fn video_name(&self) -> &'static str {
        if self.openh264 {
            jstream_platform_linux::OPENH264_ENCODER_NAME
        } else {
            x264::NAME
        }
    }
}

impl Codecs for RealCodecs {
    fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
        let mut list = self.linux.list()?;
        if !self.openh264 {
            list.push(x264::info());
        }
        Ok(list)
    }

    fn video_encoder(
        &self,
        config: &VideoEncoderConfig,
    ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
        if self.openh264 {
            self.linux.video_encoder(config)
        } else {
            Ok(Box::new(x264::X264::open(config)?))
        }
    }

    fn audio_encoder(
        &self,
        config: &AudioEncoderConfig,
    ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
        self.linux.audio_encoder(config)
    }

    fn video_decoder(
        &self,
        config: &VideoDecoderConfig,
    ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
        self.linux.video_decoder(config)
    }
}

/// The fake devices and the pump that moves them in real time.
struct Fakes {
    clock: FakeClock,
    audio: FakeAudioIo,
}

/// The microphone's level: a 50 Hz square wave at this amplitude, about -14 dBFS.
const MIC_LEVEL: f32 = 0.2;

impl Fakes {
    fn new() -> Self {
        let clock = FakeClock::new(Timebase::MICROS);
        let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
        Self { clock, audio }
    }

    fn backends(&self, dir: &Path) -> (Backends, &'static str) {
        let size = FrameSize::new(CANVAS.0, CANVAS.1).expect("size");
        let codecs = RealCodecs::new(dir);
        let name = codecs.video_name();
        let backends = Backends {
            clock: Arc::new(self.clock.clone()),
            video_devices: Arc::new(FakeVideoDevices::new(self.clock.clone())),
            capture: Arc::new(FakeCapture::new(self.clock.clone(), size)),
            audio: Arc::new(self.audio.clone()),
            codecs: Arc::new(codecs),
            gpu: Arc::new(SoftwareGpu),
            vcam: None,
            media: Arc::new(FakeMediaDecoders::new()),
        };
        (backends, name)
    }

    /// Keeps the media clock level with real time, feeding the microphone 10 ms at a
    /// time (alternate packets flip sign: a 50 Hz square wave), until the flag is set.
    fn pump(&self) -> Pump {
        let stop = Arc::new(AtomicBool::new(false));
        let (clock, audio) = (self.clock.clone(), self.audio.clone());
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let start = Instant::now();
            let mut fed = 0u64;
            while !flag.load(Ordering::Acquire) {
                let now = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
                while fed + 10_000 <= now {
                    let sign = if (fed / 10_000).is_multiple_of(2) {
                        1.0
                    } else {
                        -1.0
                    };
                    audio.deliver(480, sign * MIC_LEVEL);
                    clock.advance(10_000);
                    fed += 10_000;
                }
                thread::sleep(Duration::from_millis(2));
            }
        });
        Pump {
            stop,
            thread: Some(thread),
        }
    }
}

struct Pump {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---------------------------------------------------------------------------------------
// The engine, and what it reports

/// The daemon's live engine on the backends above, a client session, and every event
/// it has sent, collected in the background.
struct Rig {
    _engine: Arc<Engine>,
    control: InProcessControl,
    events: Arc<Mutex<Vec<Event>>>,
    video_encoder: &'static str,
    _fakes: Fakes,
    _pump: Pump,
}

impl Rig {
    async fn new(dir: &Path) -> Self {
        let fakes = Fakes::new();
        let (backends, video_encoder) = fakes.backends(dir);
        let keys = Arc::new(MemoryKeyStore::new());
        // The daemon's own wiring (`jstream_daemon::live`), with the compositor it opens
        // by default: the GPU one, on the software adapter.
        let engine = live(Engine::new(), backends, keys, rtmp());
        let control = engine.connect();
        control
            .hello(Hello {
                protocol: PROTOCOL_VERSION,
                client_name: "receiver-e2e".into(),
            })
            .await
            .expect("welcome");
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut stream = control.events();
        let sink = Arc::clone(&events);
        tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(event);
            }
        });
        let pump = fakes.pump();
        let rig = Self {
            _engine: engine,
            control,
            events,
            video_encoder,
            _fakes: fakes,
            _pump: pump,
        };
        rig.edit(json!([
            {"type": "canvas.set_size", "width": CANVAS.0, "height": CANVAS.1},
            {"type": "scene.create", "scene_id": "main", "name": "Main"},
        ]))
        .await;
        rig
    }

    /// Applies `edits`, a JSON list of edits, as one batch.
    async fn edit(&self, edits: Value) {
        let batch: Batch =
            serde_json::from_value(json!({ "edits": edits })).expect("a valid batch");
        self.control
            .call(Call::Batch(batch))
            .await
            .unwrap_or_else(|e| panic!("the batch was refused: {e}"));
    }

    async fn action(&self, action: Action) {
        let name = action.name();
        self.control
            .call(Call::Action { action })
            .await
            .unwrap_or_else(|e| panic!("{name} failed: {e}"));
    }

    /// Creates an RTMP output to the receiver on `port`, with its key.
    async fn stream_to(&self, id: &str, port: u16) {
        self.edit(json!([{
            "type": "output.create",
            "output_id": id,
            "kind": {"type": "rtmp", "target": {"type": "generic", "url": format!("rtmp://127.0.0.1:{port}/live")}},
        }]))
        .await;
        self.action(Action::OutputSetKey {
            output_id: OutputId::new(id),
            key: SecretString::new(KEY),
        })
        .await;
    }

    async fn start(&self, id: &str) {
        self.action(Action::OutputStart {
            output_id: OutputId::new(id),
        })
        .await;
    }

    async fn stop(&self, id: &str) {
        self.action(Action::OutputStop {
            output_id: OutputId::new(id),
        })
        .await;
        self.until(&format!("{id} stopped"), |rig| {
            rig.state(id) == Some(OutputState::Stopped)
        })
        .await;
    }

    fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The output's latest state, from its state events.
    fn state(&self, id: &str) -> Option<OutputState> {
        self.states(id).last().copied()
    }

    fn states(&self, id: &str) -> Vec<OutputState> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::OutputStateChanged { output_id, state } if output_id.as_str() == id => {
                    Some(state)
                }
                _ => None,
            })
            .collect()
    }

    /// The render ticks skipped so far, engine-wide, by the latest `stats.tick` (0 before
    /// the first). The totals run for the engine's life, so a run's skips are the
    /// difference across it.
    fn render_ticks(&self) -> u64 {
        self.events()
            .into_iter()
            .rev()
            .find_map(|event| match event {
                Event::StatsTick { drops, .. } => Some(drops.render_ticks),
                _ => None,
            })
            .unwrap_or(0)
    }

    /// Every `stats.tick` entry for the output, oldest first.
    fn stats(&self, id: &str) -> Vec<OutputStatsInfo> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::StatsTick { outputs, .. } => {
                    outputs.into_iter().find(|o| o.output_id.as_str() == id)
                }
                _ => None,
            })
            .collect()
    }

    /// Waits up to 30 s for `done`.
    async fn until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; events: {:#?}",
                self.events()
                    .into_iter()
                    .filter(|e| !matches!(e, Event::StatsTick { .. }))
                    .collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits for the output to have sent `seconds` of video by its own stats.
    async fn stream_for(&self, id: &str, seconds: u64) {
        self.until(&format!("{seconds} s of {id}"), |rig| {
            rig.stats(id)
                .last()
                .is_some_and(|s| s.frames >= seconds * 30)
        })
        .await;
    }

    /// `output.status`'s encoder for the output, and its state.
    async fn status(&self, id: &str) -> QueryResult {
        let response = self
            .control
            .call(Call::Query {
                query: Query::OutputStatus {
                    output_id: OutputId::new(id),
                },
            })
            .await
            .expect("output.status");
        match response.result {
            CallResult::Query { result } => result,
            other => panic!("not a query result: {other:?}"),
        }
    }
}

/// A full-canvas colour, and the microphone.
fn red_scene_with_mic() -> Value {
    json!([
        {"type": "source.create", "source_id": "red", "name": "Red",
         "kind": {"type": "builtin", "kind": "color"},
         "settings": {"color": "#ff0000ff", "width": CANVAS.0, "height": CANVAS.1}},
        {"type": "item.add", "scene_id": "main", "item_id": "red_1", "source_id": "red"},
        {"type": "source.create", "source_id": "mic", "name": "Mic",
         "kind": {"type": "builtin", "kind": "audio_input"}},
    ])
}

fn close(value: f64, want: f64, slack: f64) -> bool {
    (value - want).abs() <= slack
}

/// Whether a decoded pixel is near `want`, allowing for limited-range YUV and lossy
/// coding.
fn near(rgb: [u8; 3], want: [u8; 3]) -> bool {
    rgb.iter()
        .zip(want)
        .all(|(&got, want)| (i16::from(got) - i16::from(want)).abs() <= 40)
}

// ---------------------------------------------------------------------------------------
// The tests

/// One stream on an encoder profile: the receiver gets H.264 at the canvas size, 30 fps,
/// keyframes at the profile's interval, AAC stereo at 48 kHz with the microphone in it,
/// red pixels, and as much media as the output's stats say it sent. The stats tick
/// each second with the uptime rising, bytes and frames growing, a bitrate near the
/// profile's, nothing dropped and health `good`; the uptime starts again at the next
/// `output.start`.
#[ignore = "needs ffmpeg, ffprobe and a Vulkan adapter: cargo xtask e2e-stream"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_reaches_a_real_receiver_with_its_encoder_settings_and_stats() {
    let dir = scratch("stream");
    let rig = Rig::new(&dir).await;
    rig.edit(red_scene_with_mic()).await;
    rig.edit(json!([
        {"type": "encoder.create", "profile_id": "low", "name": "Low",
         "profile": {
            "video": {"codec": "h264", "rate_control": {"type": "cbr", "kbps": 800},
                      "keyframe_interval_s": 1, "hardware": "avoid"},
            "audio": {"codec": "aac", "kbps": 128}}},
    ]))
    .await;
    let port = free_port();
    rig.stream_to("live", port).await;
    rig.edit(json!([{"type": "output.set_encoder", "output_id": "live", "profile_id": "low"}]))
        .await;

    let receiver = Receiver::start(port, KEY, &dir.join("live.flv"));
    let skipped_before = rig.render_ticks();
    let started = Instant::now();
    rig.start("live").await;
    rig.until("live active", |rig| {
        rig.state("live") == Some(OutputState::Active)
    })
    .await;
    match rig.status("live").await {
        QueryResult::OutputStatus { encoder, .. } => {
            let encoder = encoder.expect("a running output has a session");
            assert!(!encoder.hardware, "{encoder:?}");
        }
        other => panic!("not an output status: {other:?}"),
    }
    rig.stream_for("live", 5).await;
    let streamed = started.elapsed().as_secs_f64();
    let stats = rig.stats("live");
    rig.stop("live").await;
    // The stop's final tick, before `stopped`, holds the run's last skips.
    let skipped = rig.render_ticks() - skipped_before;
    let file = receiver.finish();

    // The stats, tick by tick.
    assert!(stats.len() >= 4, "{stats:#?}");
    assert!(
        stats.windows(2).all(|w| w[1].uptime_ms > w[0].uptime_ms
            && w[1].bytes >= w[0].bytes
            && w[1].frames >= w[0].frames),
        "uptime rises and the totals grow: {stats:#?}"
    );
    let last = stats.last().expect("a tick");
    assert!(
        last.uptime_ms as f64 / 1000.0 <= streamed + 0.5,
        "uptime counts from output.start: {last:?} after {streamed} s"
    );
    // A flat colour codes far below the profile's rate, so the bitrate is checked
    // against the bytes counted between ticks, and against the profile as a ceiling.
    for pair in stats[1..].windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let ms = (b.uptime_ms - a.uptime_ms) as f64;
        let measured = (b.bytes - a.bytes) as f64 * 8.0 / ms;
        assert!(
            close(f64::from(b.kbps), measured, measured * 0.25 + 8.0),
            "kbps is the bytes sent over the interval: {measured:.0} vs {b:?}"
        );
        assert!(
            b.kbps > 0 && b.kbps <= 1_000,
            "at most the profile's 800 + 128 kbit/s, with a little slack: {b:?}"
        );
        assert_eq!(b.health, OutputHealth::Good, "{b:?}");
    }
    assert_eq!(last.dropped_frames, 0, "{last:?}");
    assert_eq!(last.dropped_packets, 0, "{last:?}");
    assert!(
        rig.states("live")
            .iter()
            .all(|s| !matches!(s, OutputState::Failed | OutputState::Reconnecting)),
        "{:?}",
        rig.states("live")
    );

    // What the receiver got.
    let p = probe(&file);
    eprintln!("{} received: {p:?}", rig.video_encoder);
    assert_eq!(p.video_codec, "h264", "{p:?}");
    assert_eq!(
        (p.width, p.height),
        (u64::from(CANVAS.0), u64::from(CANVAS.1))
    );
    assert!(close(p.measured_fps(), 30.0, 1.0), "{p:?}");
    assert_eq!(p.audio_codec.as_deref(), Some("aac"), "{p:?}");
    assert_eq!(
        (p.sample_rate, p.channels),
        (Some(48_000), Some(2)),
        "{p:?}"
    );
    // The file is checked against the stats by its frame count, and its duration by
    // the frames' own timestamps. The pts are wall clock and a stalled tick is counted
    // in `render_ticks`, not `frames`, so a runner stall lengthens the file without
    // adding frames: `frames / 30` is not its duration then (#1007), but the frames
    // plus the ticks skipped over the run are, within a second (#1010).
    assert!(
        close(p.video_packets.len() as f64, last.frames as f64, 30.0),
        "the file holds the frames the stats counted, within a tick: {} vs {last:?}",
        p.video_packets.len()
    );
    let span = match (p.video_packets.first(), p.video_packets.last()) {
        (Some((start, _)), Some((end, _))) => end - start + 1.0 / 30.0,
        _ => 0.0,
    };
    assert!(
        close(p.duration, span, 1.0),
        "the file lasts as long as its frames: {span:.3} s vs {p:?}"
    );
    let ticks = p.video_packets.len() as f64 + skipped as f64;
    eprintln!(
        "{span:.3} s of video: {} frames and {skipped} skipped ticks",
        p.video_packets.len()
    );
    assert!(
        close(span * 30.0, ticks, 30.0),
        "the frames and the skipped ticks fill the file's span: {span:.3} s, {} frames, \
         {skipped} skipped ticks",
        p.video_packets.len()
    );
    assert_eq!(
        p.video_packets.first().map(|(_, k)| *k),
        Some(true),
        "{p:?}"
    );
    // The profile's interval is in frames (one second's worth), so it is checked in
    // frames, then in seconds from the second keyframe on: the render loop skips ticks
    // while lavapipe draws its first frames on a slow machine, which stretches the
    // first gap. Those are engine-wide `render_ticks` drops, not the output's. A runner
    // stall later on stretches the gaps it falls in the same way, by the ticks it
    // skipped, which `stats.tick` doesn't place in time (#1012). So each gap is a second
    // within 0.1 s plus at most the run's skipped ticks, and together the gaps stretch
    // by no more than those ticks: with none skipped, each is a second within 0.1 s.
    let spacing = p.keyframe_spacing();
    assert!(spacing.len() >= 3, "{spacing:?}");
    assert!(
        spacing.iter().all(|n| *n == 30),
        "a keyframe every 30 frames: {spacing:?}"
    );
    let gaps = p.keyframe_gaps();
    let stall = skipped as f64 / 30.0;
    assert!(
        gaps[1..].iter().all(|g| *g >= 0.9 && *g <= 1.1 + stall),
        "a keyframe every second once running, or later by the {skipped} skipped ticks: \
         {gaps:?}"
    );
    let stretch: f64 = gaps[1..].iter().map(|g| g - 1.0).sum();
    assert!(
        stretch <= stall + 0.1,
        "the gaps stretch by at most the {skipped} skipped ticks: {stretch:.3} s, {gaps:?}"
    );
    let rgb = pixel(&file, 2.0, 160, 90);
    assert!(near(rgb, [255, 0, 0]), "the canvas is red: {rgb:?}");
    let volume = mean_volume(&file);
    assert!(volume > -25.0, "the microphone is in it: {volume} dB");

    // A second run's uptime starts again from 0.
    let receiver = Receiver::start(port, KEY, &dir.join("again.flv"));
    let before = rig.stats("live").len();
    rig.start("live").await;
    rig.until("a tick of the second run", |rig| {
        rig.stats("live").len() > before
    })
    .await;
    let first = rig.stats("live")[before].clone();
    assert!(
        first.uptime_ms < 2_000,
        "the uptime starts again: {first:?}"
    );
    rig.stop("live").await;
    drop(receiver.finish());
}

/// Two outputs at once on one program: a wide stream, and a vertical one on a 180x320
/// video mix at half rate with a hue-shift filter on its own video chain (FL-07). The
/// wide receiver gets red at 320x180 and 30 fps; the vertical one gets 180x320 at
/// 15 fps, the canvas fitted in the middle with black above and below, turned green by
/// its filter. Its own chain gives it a dedicated encoder session.
#[ignore = "needs ffmpeg, ffprobe and a Vulkan adapter: cargo xtask e2e-stream"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vertical_mix_with_an_output_filter_beside_a_wide_stream() {
    let dir = scratch("vertical");
    let rig = Rig::new(&dir).await;
    rig.edit(red_scene_with_mic()).await;
    let (wide_port, tall_port) = (free_port(), free_port());
    rig.stream_to("wide", wide_port).await;
    rig.stream_to("tall", tall_port).await;
    rig.edit(json!([
        {"type": "video_mix.create", "mix_id": "vertical", "name": "Vertical",
         "mix": {"input": {"type": "program"}, "resolution": {"width": 180, "height": 320},
                 "fps_divisor": 2}},
        {"type": "output.set_video_mix", "output_id": "tall", "mix_id": "vertical"},
        {"type": "filter.add", "owner": {"type": "output", "output_id": "tall"},
         "chain": "video", "filter_id": "hue", "kind": "color_correction",
         "settings": {"hue_shift": 120.0}},
    ]))
    .await;

    let wide = Receiver::start(wide_port, KEY, &dir.join("wide.flv"));
    let tall = Receiver::start(tall_port, KEY, &dir.join("tall.flv"));
    rig.start("wide").await;
    rig.start("tall").await;
    rig.until("both active", |rig| {
        rig.state("wide") == Some(OutputState::Active)
            && rig.state("tall") == Some(OutputState::Active)
    })
    .await;
    match rig.status("tall").await {
        QueryResult::OutputStatus { encoder, .. } => {
            let encoder = encoder.expect("a session");
            assert!(
                encoder.dedicated,
                "its own chain gives it its own session: {encoder:?}"
            );
        }
        other => panic!("not an output status: {other:?}"),
    }
    rig.stream_for("wide", 4).await;
    rig.until("the tall stream's frames", |rig| {
        rig.stats("tall").last().is_some_and(|s| s.frames >= 45)
    })
    .await;
    let tall_stats = rig.stats("tall");
    rig.stop("tall").await;
    rig.stop("wide").await;
    let (wide, tall) = (wide.finish(), tall.finish());

    let p = probe(&wide);
    assert_eq!((p.width, p.height), (320, 180), "{p:?}");
    assert!(close(p.measured_fps(), 30.0, 1.0), "{p:?}");
    let rgb = pixel(&wide, 1.5, 160, 90);
    assert!(near(rgb, [255, 0, 0]), "the wide stream is red: {rgb:?}");

    let p = probe(&tall);
    eprintln!("vertical received: {p:?}");
    assert_eq!(p.video_codec, "h264", "{p:?}");
    assert_eq!((p.width, p.height), (180, 320), "{p:?}");
    assert!(
        close(p.measured_fps(), 15.0, 1.0),
        "half the base rate: {p:?}"
    );
    assert_eq!(p.audio_codec.as_deref(), Some("aac"), "{p:?}");
    let middle = pixel(&tall, 1.5, 90, 160);
    assert!(
        near(middle, [0, 255, 0]),
        "the filter turned red green: {middle:?}"
    );
    let top = pixel(&tall, 1.5, 90, 10);
    assert!(
        near(top, [0, 0, 0]),
        "black above the fitted canvas: {top:?}"
    );
    let last = tall_stats.last().expect("a tick");
    assert_eq!(last.dropped_frames, 0, "{last:?}");
}

/// Outputs carry a bus: one stream on the master hears the microphone, one on a bus
/// with no sends is silent; after a send into that bus, a new run of it hears the
/// microphone too.
#[ignore = "needs ffmpeg, ffprobe and a Vulkan adapter: cargo xtask e2e-stream"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_output_on_another_bus_carries_that_bus() {
    let dir = scratch("bus");
    let rig = Rig::new(&dir).await;
    rig.edit(red_scene_with_mic()).await;
    let (main_port, clean_port) = (free_port(), free_port());
    rig.stream_to("main", main_port).await;
    rig.stream_to("clean", clean_port).await;
    rig.edit(json!([
        {"type": "bus.create", "bus_id": "vod", "name": "VOD"},
        {"type": "output.set_audio_buses", "output_id": "clean", "audio_buses": ["vod"]},
    ]))
    .await;

    let main = Receiver::start(main_port, KEY, &dir.join("main.flv"));
    let clean = Receiver::start(clean_port, KEY, &dir.join("clean.flv"));
    rig.start("main").await;
    rig.start("clean").await;
    rig.stream_for("main", 3).await;
    rig.stream_for("clean", 3).await;
    rig.stop("clean").await;
    rig.stop("main").await;
    let (main, clean) = (main.finish(), clean.finish());
    let (heard, silent) = (mean_volume(&main), mean_volume(&clean));
    assert!(heard > -25.0, "the master has the microphone: {heard} dB");
    assert!(silent < -60.0, "the empty bus is silent: {silent} dB");
    assert_eq!(probe(&clean).audio_codec.as_deref(), Some("aac"));

    rig.edit(json!([{"type": "send.set", "source_id": "mic", "bus_id": "vod", "gain_db": 0.0}]))
        .await;
    let clean = Receiver::start(clean_port, KEY, &dir.join("clean-sent.flv"));
    rig.start("clean").await;
    rig.stream_for("clean", 3).await;
    rig.stop("clean").await;
    let sent = mean_volume(&clean.finish());
    assert!(sent > -25.0, "the bus has the microphone now: {sent} dB");
}

/// The receiver goes away mid-stream and comes back: the output reports
/// `reconnecting` with health `poor`, its uptime keeps counting, and once the server
/// listens again it reconnects on its own and goes `active`, staying `poor` while the
/// outage's lost video is inside the health's window, and the new connection carries a
/// decodable stream that opens on a keyframe with audio.
#[ignore = "needs ffmpeg, ffprobe and a Vulkan adapter: cargo xtask e2e-stream"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_reconnects_after_the_receiver_restarts() {
    let dir = scratch("reconnect");
    let rig = Rig::new(&dir).await;
    rig.edit(red_scene_with_mic()).await;
    let port = free_port();
    rig.stream_to("live", port).await;

    let receiver = Receiver::start(port, KEY, &dir.join("before.flv"));
    rig.start("live").await;
    rig.stream_for("live", 3).await;
    let before = receiver.kill();
    rig.until("live reconnecting", |rig| {
        rig.state("live") == Some(OutputState::Reconnecting)
    })
    .await;
    let ticks = rig.stats("live").len();
    rig.until("a tick while down", |rig| {
        rig.stats("live").len() > ticks + 1
    })
    .await;
    let down = rig.stats("live").last().cloned().expect("a tick");
    assert_eq!(down.health, OutputHealth::Poor, "{down:?}");

    let receiver = Receiver::start(port, KEY, &dir.join("after.flv"));
    rig.until("live active again", |rig| {
        rig.state("live") == Some(OutputState::Active)
            && rig.states("live").contains(&OutputState::Reconnecting)
    })
    .await;
    let frames = rig.stats("live").last().map_or(0, |s| s.frames);
    rig.until("3 s more", |rig| {
        rig.stats("live")
            .last()
            .is_some_and(|s| s.frames >= frames + 90)
    })
    .await;
    let stats = rig.stats("live");
    rig.stop("live").await;
    let after = receiver.finish();

    assert!(
        stats.windows(2).all(|w| w[1].uptime_ms > w[0].uptime_ms),
        "the uptime kept counting through the outage: {stats:#?}"
    );
    let last = stats.last().expect("a tick");
    // The video lost in the outage is still inside the health's ten-second window, so
    // the output stays `poor` after it reconnects (#994). Those frames are counted in
    // `dropped_packets`, not `dropped_frames`.
    assert_eq!(
        last.health,
        OutputHealth::Poor,
        "the outage's lost video still counts: {last:?}"
    );
    assert_eq!(last.dropped_frames, 0, "{last:?}");
    assert!(
        last.dropped_packets > 0,
        "the outage's packets were counted: {last:?}"
    );
    let states = rig.states("live");
    assert!(!states.contains(&OutputState::Failed), "{states:?}");

    assert!(probe(&before).duration > 1.0, "{:?}", probe(&before));
    let p = probe(&after);
    eprintln!("after the reconnect: {p:?}");
    assert_eq!(p.video_codec, "h264", "{p:?}");
    assert_eq!(p.audio_codec.as_deref(), Some("aac"), "{p:?}");
    assert_eq!(
        p.video_packets.first().map(|(_, k)| *k),
        Some(true),
        "the new connection opens on a keyframe: {p:?}"
    );
    assert!(p.duration > 2.0, "{p:?}");
    let rgb = pixel(&after, 1.0, 160, 90);
    assert!(near(rgb, [255, 0, 0]), "{rgb:?}");
}
