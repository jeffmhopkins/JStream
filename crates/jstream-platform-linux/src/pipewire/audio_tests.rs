//! `PipeWireAudio` against the user's PipeWire daemon: they record from the default
//! input, play a tone on the default output and loop it back. They need no sound card:
//! a null sink and source are enough, and the graph's own driver paces them:
//!
//! ```text
//! pw-cli create-node adapter '{ factory.name=support.null-audio-sink
//!     node.name=jstream-test-sink media.class=Audio/Sink object.linger=1 }'
//! pw-cli create-node adapter '{ factory.name=support.null-audio-sink
//!     node.name=jstream-test-source media.class=Audio/Source/Virtual object.linger=1 }'
//! pw-cli create-node adapter '{ factory.name=support.null-audio-sink
//!     node.name=jstream-test-sink-51 media.class=Audio/Sink object.linger=1
//!     audio.position=[FL FR FC LFE RL RR] }'
//! pw-cli create-node adapter '{ factory.name=support.null-audio-sink
//!     node.name=jstream-test-card-source media.class=Audio/Source/Virtual
//!     object.linger=1 api.alsa.pcm.card=7 }'
//! cargo test -p jstream-platform-linux --features hw-tests -- --ignored audio_tests
//! ```

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use jstream_platform::media::MediaTime;
use jstream_platform::{
    AudioDirection, AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler,
    AudioOutputTarget, AudioPacket, AudioRenderBuffer, DeviceId, MediaClock, PlatformError,
};

use super::PipeWireAudio;
use super::audio::Connection;
use crate::MonotonicClock;

/// The backend, connected to the user's daemon, and a guard that runs these tests one
/// at a time: they share the default devices, and a process loopback records this
/// process's first playback stream, which must be the test's own.
fn audio() -> (MutexGuard<'static, ()>, PipeWireAudio) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let one = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let audio = PipeWireAudio::new().expect("libpipewire");
    audio.devices().expect("a PipeWire daemon");
    (one, audio)
}

/// Panics, naming what to create, without a device that way.
fn need(audio: &PipeWireAudio, direction: AudioDirection) {
    let devices = audio.devices().expect("the devices");
    assert!(
        devices.iter().any(|d| d.direction == direction),
        "no {direction:?} device: create a null one (see the module doc)"
    );
}

/// What an input handler saw.
#[derive(Debug, Default)]
struct Seen {
    packets: u64,
    /// The next position a continuous packet starts at.
    next: u64,
    gaps: u64,
    ragged: u64,
    /// Packets flagged silent (an idle source's empty chunks).
    silent: u64,
    peak: f32,
    times: Vec<MediaTime>,
    errors: Vec<String>,
    /// The name of the thread the first packet came on.
    thread: Option<String>,
}

struct Recorder {
    channels: usize,
    seen: Arc<Mutex<Seen>>,
}

impl AudioInputHandler for Recorder {
    fn on_packet(&mut self, packet: &AudioPacket<'_>) {
        let mut s = self.seen.lock().expect("the seen lock");
        if s.thread.is_none() {
            let comm = std::fs::read_to_string("/proc/thread-self/comm");
            s.thread = Some(comm.expect("the thread's name").trim().to_owned());
        }
        if !packet.samples.len().is_multiple_of(self.channels) {
            s.ragged += 1;
        }
        // A jump is a gap unless the packet says frames were lost (a skipped buffer).
        if packet.device_position != s.next && !packet.flags.discontinuity {
            s.gaps += 1;
        }
        s.next = packet.device_position + (packet.samples.len() / self.channels) as u64;
        s.packets += 1;
        s.silent += u64::from(packet.flags.silent);
        let peak = packet.samples.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        s.peak = s.peak.max(peak);
        if let Some(t) = packet.time {
            s.times.push(t);
        }
    }

    fn on_error(&mut self, error: &PlatformError) {
        self.seen
            .lock()
            .expect("the seen lock")
            .errors
            .push(error.to_string());
    }
}

/// Plays a constant `level`, counting the frames asked for.
struct Tone {
    level: f32,
    frames: Arc<AtomicU64>,
    channels: usize,
    failed: Arc<AtomicBool>,
}

impl AudioOutputHandler for Tone {
    fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>) {
        buffer.samples.fill(self.level);
        self.frames.fetch_add(
            (buffer.samples.len() / self.channels) as u64,
            Ordering::Relaxed,
        );
    }

    fn on_error(&mut self, _error: &PlatformError) {
        self.failed.store(true, Ordering::Relaxed);
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn record(
    audio: &PipeWireAudio,
    target: &AudioInputTarget,
) -> (Box<dyn jstream_platform::AudioStream>, Arc<Mutex<Seen>>) {
    let format = audio.input_format(target).expect("the input format");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let stream = audio
        .open_input(
            target,
            Box::new(Recorder {
                channels: format.channels(),
                seen: Arc::clone(&seen),
            }),
        )
        .expect("an input stream");
    assert_eq!(stream.format(), format, "the format input_format promised");
    (stream, seen)
}

struct Playing {
    _stream: Box<dyn jstream_platform::AudioStream>,
    frames: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
}

fn play(audio: &PipeWireAudio, level: f32) -> Playing {
    let frames = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicBool::new(false));
    let target = AudioOutputTarget::DefaultOutput;
    // The default sink's layout, as the plan picks it.
    let channels = {
        let plan = Connection::open(audio.pw.library())
            .expect("a connection")
            .graph()
            .output(&target)
            .expect("an output plan");
        plan.layout.channel_count()
    };
    let stream = audio
        .open_output(
            &target,
            Box::new(Tone {
                level,
                frames: Arc::clone(&frames),
                channels,
                failed: Arc::clone(&failed),
            }),
        )
        .expect("an output stream");
    assert_eq!(stream.format().channels(), channels);
    Playing {
        _stream: stream,
        frames,
        failed,
    }
}

#[test]
#[ignore = "needs a PipeWire daemon (see the module doc)"]
fn devices_have_ids_and_at_most_one_default_each_way() {
    let (_one, audio) = audio();
    let devices = audio.devices().unwrap();
    for d in &devices {
        assert!(!d.id.as_str().is_empty() && !d.name.is_empty(), "{d:?}");
    }
    for direction in [AudioDirection::Input, AudioDirection::Output] {
        let defaults = devices
            .iter()
            .filter(|d| d.direction == direction && d.is_default)
            .count();
        assert!(defaults <= 1, "{devices:?}");
    }
}

#[test]
#[ignore = "needs a PipeWire daemon (see the module doc)"]
fn recording_the_default_input_delivers_continuous_timed_packets() {
    let (_one, audio) = audio();
    need(&audio, AudioDirection::Input);
    let clock = MonotonicClock::new().unwrap();
    let start = clock.now();
    let (stream, seen) = record(&audio, &AudioInputTarget::DefaultInput);
    wait_for("ten packets", || seen.lock().unwrap().packets >= 10);
    drop(stream);
    let s = seen.lock().unwrap();
    assert_eq!((s.gaps, s.ragged), (0, 0), "{s:?}");
    assert!(s.errors.is_empty(), "{s:?}");
    // On PipeWire's data loop (`RT_PROCESS`: `pw-data-loop`, `data-loop.0` in newer
    // releases), not the connection's thread loop (`jstream-audio`).
    let thread = s.thread.as_deref().unwrap();
    assert!(thread.contains("data-loop"), "{thread}");
    let end = clock.now();
    // Times are on the media clock, in order, and about now (within a second of the
    // recording, allowing for the device's delay).
    assert!(s.times.windows(2).all(|w| w[0] <= w[1]), "{:?}", s.times);
    let second = 10_000_000;
    for t in &s.times {
        assert!(
            t.ticks() > start.ticks() - second && t.ticks() < end.ticks(),
            "{t:?}"
        );
    }
    // Nothing after the drop.
    let packets = s.packets;
    drop(s);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(seen.lock().unwrap().packets, packets);
}

#[test]
#[ignore = "needs a PipeWire daemon (see the module doc)"]
fn playing_renders_until_dropped() {
    let (_one, audio) = audio();
    need(&audio, AudioDirection::Output);
    let playing = play(&audio, 0.25);
    wait_for("a tenth of a second rendered", || {
        playing.frames.load(Ordering::Relaxed) >= 4_800
    });
    assert!(!playing.failed.load(Ordering::Relaxed));
    let frames = Arc::clone(&playing.frames);
    drop(playing);
    let after = frames.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(frames.load(Ordering::Relaxed), after);
}

/// Our own playback stream is an application playing audio: it is listed under this
/// process, and both process loopback and desktop audio record what it plays.
#[test]
#[ignore = "needs a PipeWire daemon (see the module doc)"]
fn our_own_playback_is_recorded_by_process_and_desktop_loopback() {
    let (_one, audio) = audio();
    need(&audio, AudioDirection::Output);
    let _playing = play(&audio, 0.5);
    let pid = std::process::id();
    wait_for("this process in the list", || {
        audio.processes().unwrap().iter().any(|p| p.pid == pid)
    });
    // Named by its executable, from `/proc`.
    let exe = std::env::current_exe().unwrap();
    let exe = exe.file_name().unwrap().to_str().unwrap();
    assert!(
        audio
            .processes()
            .unwrap()
            .iter()
            .any(|p| p.pid == pid && p.exe == exe)
    );
    for target in [
        AudioInputTarget::ProcessLoopback { pid },
        AudioInputTarget::DefaultOutputLoopback,
    ] {
        let (stream, seen) = record(&audio, &target);
        wait_for("the tone", || seen.lock().unwrap().peak > 0.25);
        drop(stream);
        assert!(seen.lock().unwrap().errors.is_empty(), "{target:?}");
    }
}

#[test]
#[ignore = "needs a PipeWire daemon (see the module doc)"]
fn unknown_targets_are_not_found() {
    let (_one, audio) = audio();
    let not_found = |r: Result<Box<dyn jstream_platform::AudioStream>, PlatformError>| {
        matches!(r, Err(PlatformError::NotFound { .. }))
    };
    let handler = || {
        Box::new(Recorder {
            channels: 2,
            seen: Arc::default(),
        })
    };
    let gone = DeviceId::new("jstream-test-no-such-node");
    assert!(not_found(
        audio.open_input(&AudioInputTarget::Input(gone.clone()), handler())
    ));
    assert!(not_found(audio.open_input(
        &AudioInputTarget::OutputLoopback(gone.clone()),
        handler()
    )));
    assert!(not_found(audio.open_input(
        &AudioInputTarget::ProcessLoopback { pid: u32::MAX },
        handler()
    )));
    assert!(matches!(
        audio.input_format(&AudioInputTarget::Input(gone)),
        Err(PlatformError::NotFound { .. })
    ));
}

/// The 5.1 null sink's layout comes from its node's info (its global doesn't carry one).
#[test]
#[ignore = "needs a PipeWire daemon (see the module doc)"]
fn a_surround_sink_is_planned_in_its_own_layout() {
    let (_one, audio) = audio();
    let sink = DeviceId::new("jstream-test-sink-51");
    let format = audio
        .input_format(&AudioInputTarget::OutputLoopback(sink))
        .expect("the 5.1 test sink (see the module doc)");
    assert_eq!(format.channels(), 6);
}

/// An input's ALSA card comes from its node's info, for pairing webcams: the test
/// source that says it is on card 7 is listed there, and no input is on card 99.
#[test]
#[ignore = "needs a PipeWire daemon (see the module doc)"]
fn inputs_are_listed_by_their_sound_card() {
    use crate::{PairedAudio, SoundCard};

    let (_one, audio) = audio();
    let inputs = audio.card_inputs().expect("card inputs");
    let card = |index| SoundCard {
        index,
        id: "unused".into(),
    };
    assert_eq!(
        inputs.input_for(&[card(7)]),
        Some(DeviceId::new("jstream-test-card-source")),
        "the card-7 test source (see the module doc)"
    );
    assert_eq!(inputs.input_for(&[card(99)]), None);
}

/// Panics on its first packet.
struct Panicky {
    errors: Arc<Mutex<Vec<PlatformError>>>,
}

impl AudioInputHandler for Panicky {
    fn on_packet(&mut self, _packet: &AudioPacket<'_>) {
        panic!("a handler bug");
    }

    fn on_error(&mut self, error: &PlatformError) {
        self.errors
            .lock()
            .expect("the errors lock")
            .push(error.clone());
    }
}

/// A panicking handler is told the stream failed, once, and isn't called again.
#[test]
#[ignore = "needs a PipeWire daemon (see the module doc)"]
fn a_panicking_handler_is_told_the_stream_failed() {
    let (_one, audio) = audio();
    need(&audio, AudioDirection::Input);
    let errors = Arc::new(Mutex::new(Vec::new()));
    let stream = audio
        .open_input(
            &AudioInputTarget::DefaultInput,
            Box::new(Panicky {
                errors: Arc::clone(&errors),
            }),
        )
        .unwrap();
    wait_for("the error", || !errors.lock().unwrap().is_empty());
    std::thread::sleep(Duration::from_millis(100));
    drop(stream);
    let errors = errors.lock().unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        matches!(errors[0], PlatformError::InvalidState { .. }),
        "{errors:?}"
    );
}
