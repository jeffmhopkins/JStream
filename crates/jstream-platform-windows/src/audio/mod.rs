//! WASAPI audio I/O: the [`AudioIo`](jstream_platform::AudioIo) backend.
//!
//! Streams are event-driven and shared-mode, each on its own thread (`stream`). Samples
//! are always interleaved `f32` at the device's own rate: WASAPI converts sample types
//! or downmixes where needed, and never resamples (`format`). Timestamps are the QPC
//! times WASAPI reports, in the crate's 100 ns [`timebase`](crate::timebase).
//!
//! # Behaviour
//!
//! **`WasapiAudio`** (`AudioIo`):
//! - **Listing** the active capture and render endpoints, with the console-role default
//!   of each, and the running processes (a ToolHelp snapshot: each one's ID, its
//!   parent's and its executable's file name), which process loopback can record.
//! - **Format query:** `input_format` returns the format a stream on a target would
//!   have, without opening it: the format below for the target's endpoint (the current
//!   default for a default target), found by activating a client and reading its mix
//!   format, and 48 kHz stereo for process loopback of a running process (`NotFound`
//!   for one that isn't).
//! - **Streams** for every target the traits define: an input, the default input, loopback
//!   of an output or the default output, process loopback (a process and its children,
//!   through the virtual process-loopback device), an output and the default output.
//!   Each stream is event-driven and shared-mode, on its own thread registered with MMCSS
//!   "Pro Audio" (ARCH › Threads, clock and sync).
//!   That thread owns the stream's COM objects. In its loop it calls the handler and does
//!   not allocate, lock or log (R-RT-001): the sample
//!   buffer is allocated once, before the stream starts.
//! - **Format:** samples are interleaved `f32` at the device's own rate. A device whose
//!   mix format is already `f32` in a layout `jstream-media` has (mono, stereo, 5.1) is
//!   opened as is. For any other format the backend asks WASAPI to convert
//!   (`AUTOCONVERTPCM`) to `f32` at the same rate: it converts integer samples and
//!   downmixes other channel counts to stereo, and never resamples. For an output with
//!   another channel count the stream is stereo, and WASAPI maps it onto the device's
//!   speakers. `jstream-media`'s 5.1 has its surround pair at the back, so a 5.1 device
//!   whose mix puts it at the side is opened through the same conversion, and WASAPI
//!   maps its channels into that order. Process loopback is asked for 48 kHz stereo,
//!   since it has no mix format.
//! - **Times and positions:** packet times are WASAPI's QPC positions, `None` when the
//!   device flags a timestamp error. Device positions count frames since the stream
//!   started.
//! - **Default devices are followed:** a stream on a default device moves to the new
//!   default when the user changes it or the device goes away. Its positions continue, and
//!   the first packet after the move is flagged as a discontinuity. If the new device has
//!   a different format, the stream ends with `DeviceLost` so the engine can reopen it. A
//!   stream on one device ends with `DeviceLost` when that device goes away.
//! - **Failures and panics:** a failure in a stream's loop is reported once through the
//!   handler's `on_error`, and the stream ends. A panic on the stream thread, the
//!   handler's included, is caught there. The stream ends, and `on_error` gets
//!   `InvalidState`. COM callbacks (activation completion, device notifications) catch
//!   panics too (R-UNSAFE-005).
//! - **Buffers:** capture and loopback ask for a 200 ms buffer, as S3 did, so a late
//!   thread loses nothing. Render asks for 40 ms, which bounds how far ahead the output
//!   handler is asked for samples.

#[cfg(windows)]
mod device;
// Platform-independent, so it is tested on every platform.
#[cfg(any(windows, test))]
mod format;
#[cfg(windows)]
mod process;
#[cfg(windows)]
mod stream;

#[cfg(windows)]
pub use backend::WasapiAudio;

#[cfg(windows)]
mod backend {
    use jstream_platform::media::AudioFormat;
    use jstream_platform::{
        AudioDeviceInfo, AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler,
        AudioOutputTarget, AudioProcess, AudioStream, PlatformError,
    };

    use super::{device, process, stream};
    use crate::com::Com;

    /// Audio devices through WASAPI: capture endpoints, loopback of render endpoints and
    /// of process trees, and render endpoints.
    ///
    /// It holds no COM objects, so it is `Send + Sync`: each call initialises COM on its
    /// calling thread (joining the multithreaded apartment unless the thread already has
    /// one), and each stream runs on a thread of its own.
    #[derive(Debug, Default)]
    pub struct WasapiAudio {
        _private: (),
    }

    impl WasapiAudio {
        /// The backend. Opening nothing yet, it can't fail.
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl AudioIo for WasapiAudio {
        fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
            let _com = Com::init()?;
            device::list(&device::enumerator()?)
        }

        fn processes(&self) -> Result<Vec<AudioProcess>, PlatformError> {
            process::list()
        }

        fn input_format(&self, target: &AudioInputTarget) -> Result<AudioFormat, PlatformError> {
            stream::input_format(target)
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            stream::open_input(target, handler)
        }

        fn open_output(
            &self,
            target: &AudioOutputTarget,
            handler: Box<dyn AudioOutputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            stream::open_output(target, handler)
        }
    }
}

/// Platform tests against real devices (R-TEST-004): behind the `hw-tests` feature and
/// `#[ignore]`d, because CI machines may have no audio devices. Run them on a Windows
/// machine with a microphone and speakers:
/// `cargo test -p jstream-platform-windows --features hw-tests -- --ignored`.
#[cfg(all(test, windows, feature = "hw-tests"))]
mod device_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use jstream_platform::{
        AudioDirection, AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler,
        AudioOutputTarget, AudioPacket, AudioRenderBuffer, MediaClock, PlatformError,
    };

    use super::WasapiAudio;
    use crate::QpcClock;

    /// Counts packets and samples (frames of `channels`), and remembers the last packet
    /// time.
    struct Count {
        packets: Arc<AtomicU64>,
        frames: Arc<AtomicU64>,
        last_time: Arc<AtomicU64>,
        channels: usize,
    }

    impl Count {
        /// A counter nothing reads.
        fn new() -> Self {
            Self {
                packets: Arc::new(AtomicU64::new(0)),
                frames: Arc::new(AtomicU64::new(0)),
                last_time: Arc::new(AtomicU64::new(0)),
                channels: 1,
            }
        }
    }

    impl AudioInputHandler for Count {
        fn on_packet(&mut self, packet: &AudioPacket<'_>) {
            self.packets.fetch_add(1, Ordering::Relaxed);
            let frames = (packet.samples.len() / self.channels) as u64;
            self.frames.fetch_add(frames, Ordering::Relaxed);
            if let Some(t) = packet.time {
                self.last_time
                    .store(t.ticks().cast_unsigned(), Ordering::Relaxed);
            }
        }

        fn on_error(&mut self, _error: &PlatformError) {}
    }

    /// Plays a quiet tone, writing the same value to every channel in groups of
    /// `channels`.
    struct Tone {
        phase: f32,
        channels: usize,
        frames: Arc<AtomicU64>,
    }

    impl AudioOutputHandler for Tone {
        fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>) {
            for frame in buffer.samples.chunks_exact_mut(self.channels) {
                // A quiet 440 Hz tone at 48 kHz, close enough at other rates.
                let v = 0.05 * (self.phase * std::f32::consts::TAU).sin();
                frame.fill(v);
                self.phase = (self.phase + 440.0 / 48_000.0).fract();
            }
            let frames = (buffer.samples.len() / self.channels) as u64;
            self.frames.fetch_add(frames, Ordering::Relaxed);
        }

        fn on_error(&mut self, _error: &PlatformError) {}
    }

    #[test]
    #[ignore = "needs real audio devices"]
    fn lists_devices_with_one_default_each() {
        let devices = WasapiAudio::new().devices().expect("devices");
        for dir in [AudioDirection::Input, AudioDirection::Output] {
            let of_dir: Vec<_> = devices.iter().filter(|d| d.direction == dir).collect();
            assert!(!of_dir.is_empty(), "no {dir:?} devices");
            assert_eq!(of_dir.iter().filter(|d| d.is_default).count(), 1, "{dir:?}");
        }
    }

    #[test]
    #[ignore = "needs a microphone and speakers"]
    fn input_format_matches_the_opened_stream() {
        let audio = WasapiAudio::new();
        for target in [
            AudioInputTarget::DefaultInput,
            AudioInputTarget::DefaultOutputLoopback,
        ] {
            let format = audio.input_format(&target).expect("input format");
            let stream = audio
                .open_input(&target, Box::new(Count::new()))
                .expect("open");
            assert_eq!(stream.format(), format, "{target:?}");
        }
    }

    #[test]
    #[ignore = "needs real audio devices"]
    fn lists_this_process_and_records_its_loopback() {
        let audio = WasapiAudio::new();
        let me = std::process::id();
        let processes = audio.processes().expect("processes");
        let this = processes
            .iter()
            .find(|p| p.pid == me)
            .expect("this process is listed");
        assert!(this.exe.ends_with(".exe"), "{:?}", this.exe);
        let target = AudioInputTarget::ProcessLoopback { pid: me };
        let format = audio.input_format(&target).expect("input format");
        let stream = audio
            .open_input(&target, Box::new(Count::new()))
            .expect("open process loopback");
        assert_eq!(stream.format(), format);
        // A process that isn't running is not found.
        let gone = processes.iter().map(|p| p.pid).max().unwrap_or(0) + 4;
        let err = audio
            .input_format(&AudioInputTarget::ProcessLoopback { pid: gone })
            .err();
        assert!(
            matches!(err, Some(PlatformError::NotFound { .. })),
            "{err:?}"
        );
    }

    #[test]
    #[ignore = "needs a microphone"]
    fn records_the_default_input_on_the_media_clock() {
        let audio = WasapiAudio::new();
        let clock = QpcClock::new().expect("clock");
        let (packets, frames, last_time) = (
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        // The channel count is only known once the stream is open, so the handler counts
        // samples (`channels: 1`) and they are turned into seconds with the format.
        let stream = audio
            .open_input(
                &AudioInputTarget::DefaultInput,
                Box::new(Count {
                    packets: Arc::clone(&packets),
                    frames: Arc::clone(&frames),
                    last_time: Arc::clone(&last_time),
                    channels: 1,
                }),
            )
            .expect("open default input");
        let format = stream.format();
        std::thread::sleep(Duration::from_secs(1));
        let now = clock.now().ticks();
        drop(stream);
        let seconds = frames.load(Ordering::Relaxed) as f64
            / format.channels() as f64
            / f64::from(format.sample_rate());
        assert!(packets.load(Ordering::Relaxed) > 10);
        assert!((0.7..1.3).contains(&seconds), "{seconds} s recorded in 1 s");
        // The last packet was recorded within the last 200 ms of the media clock.
        let last = last_time.load(Ordering::Relaxed).cast_signed();
        assert!(now - last < 2_000_000 && last <= now, "{last} vs {now}");
    }

    #[test]
    #[ignore = "needs speakers, and plays a quiet tone"]
    fn plays_to_the_default_output_and_records_its_loopback() {
        let audio = WasapiAudio::new();
        let played = Arc::new(AtomicU64::new(0));
        let out = audio
            .open_output(
                &AudioOutputTarget::DefaultOutput,
                Box::new(Tone {
                    phase: 0.0,
                    channels: 2,
                    frames: Arc::clone(&played),
                }),
            )
            .expect("open default output");
        assert!(out.format().channels() >= 1);
        let looped = Arc::new(AtomicU64::new(0));
        let lb = audio
            .open_input(
                &AudioInputTarget::DefaultOutputLoopback,
                Box::new(Count {
                    packets: Arc::new(AtomicU64::new(0)),
                    frames: Arc::clone(&looped),
                    last_time: Arc::new(AtomicU64::new(0)),
                    channels: 1,
                }),
            )
            .expect("open loopback");
        std::thread::sleep(Duration::from_secs(1));
        drop(lb);
        drop(out);
        assert!(played.load(Ordering::Relaxed) > 0);
        assert!(looped.load(Ordering::Relaxed) > 0);
    }
}
