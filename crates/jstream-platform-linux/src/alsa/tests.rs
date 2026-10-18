use jstream_platform::PlatformError;
use jstream_platform::media::ChannelLayout;

use super::{
    SampleKind, alsa_error, device_name, from_f32, hns_for_frames, layout_for, listed, to_f32,
};

#[test]
fn sample_kinds_are_alsas_formats() {
    // SND_PCM_FORMAT_FLOAT_LE, S32_LE and S16_LE in <alsa/pcm.h>.
    assert_eq!(SampleKind::F32.alsa_format(), 14);
    assert_eq!(SampleKind::S32.alsa_format(), 10);
    assert_eq!(SampleKind::S16.alsa_format(), 2);
    assert_eq!(SampleKind::PREFERRED[0], SampleKind::F32);
    assert_eq!(
        SampleKind::PREFERRED.map(SampleKind::bytes),
        [4, 4, 2],
        "bytes per sample"
    );
}

#[test]
fn integer_samples_convert_to_f32() {
    let raw: Vec<u8> = [i16::MIN, -16_384, 0, 16_384, i16::MAX]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut out = [9.0_f32; 5];
    to_f32(SampleKind::S16, &raw, &mut out);
    assert_eq!(out[..4], [-1.0, -0.5, 0.0, 0.5]);
    assert!((out[4] - 1.0).abs() < 1e-4, "{}", out[4]);

    let raw: Vec<u8> = [i32::MIN, 0, 1 << 30]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut out = [9.0_f32; 3];
    to_f32(SampleKind::S32, &raw, &mut out);
    assert_eq!(out, [-1.0, 0.0, 0.5]);

    let raw: Vec<u8> = [0.25_f32, -0.75]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut out = [0.0_f32; 2];
    to_f32(SampleKind::F32, &raw, &mut out);
    assert_eq!(out, [0.25, -0.75]);
}

#[test]
fn f32_converts_to_integers_clamped() {
    let input = [-2.0_f32, -1.0, 0.0, 0.5, 1.0, 3.0, f32::NAN];
    let mut raw = [0_u8; 14];
    from_f32(SampleKind::S16, &input, &mut raw);
    let back: Vec<i16> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(back, [-32_767, -32_767, 0, 16_384, 32_767, 32_767, 0]);

    let mut raw = [0_u8; 28];
    from_f32(SampleKind::S32, &input, &mut raw);
    let back: Vec<i32> = raw
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    assert_eq!(
        back,
        [
            -i32::MAX,
            -i32::MAX,
            0,
            1_073_741_824,
            i32::MAX,
            i32::MAX,
            0
        ]
    );

    let mut raw = [0_u8; 8];
    from_f32(SampleKind::F32, &[0.5, -0.25], &mut raw);
    assert_eq!(raw[..4], 0.5_f32.to_le_bytes());
    assert_eq!(raw[4..], (-0.25_f32).to_le_bytes());
}

/// A round trip through each integer kind is within its quantisation step.
#[test]
fn round_trips_stay_within_a_step() {
    let input: Vec<f32> = (0..=40).map(|i| f32::from(i as u8) / 20.0 - 1.0).collect();
    for (kind, step) in [(SampleKind::S16, 1.0 / 32_767.0), (SampleKind::S32, 1e-7)] {
        let mut raw = vec![0_u8; input.len() * kind.bytes()];
        let mut out = vec![0.0_f32; input.len()];
        from_f32(kind, &input, &mut raw);
        to_f32(kind, &raw, &mut out);
        for (a, b) in input.iter().zip(&out) {
            assert!(
                (a - b).abs() <= step * 1.5,
                "{kind:?}: {a} came back as {b}"
            );
        }
    }
}

#[test]
fn only_the_layouts_jstream_has_are_taken() {
    assert_eq!(layout_for(1), Some(ChannelLayout::Mono));
    assert_eq!(layout_for(2), Some(ChannelLayout::Stereo));
    assert_eq!(layout_for(6), Some(ChannelLayout::Surround51));
    for n in [0, 3, 4, 5, 8] {
        assert_eq!(layout_for(n), None, "{n}");
    }
}

#[test]
fn null_and_speaker_aliases_are_not_listed() {
    for name in [
        "default",
        "pipewire",
        "pulse",
        "sysdefault:CARD=PCH",
        "hw:CARD=PCH,DEV=0",
        "plughw:CARD=PCH,DEV=0",
        "hdmi:CARD=HDMI,DEV=0",
        "dmix:CARD=PCH,DEV=0",
    ] {
        assert!(listed(name), "{name}");
    }
    for name in [
        "null",
        "",
        "front:CARD=PCH,DEV=0",
        "rear:CARD=PCH,DEV=0",
        "center_lfe:CARD=PCH,DEV=0",
        "side:CARD=PCH,DEV=0",
        "surround51:CARD=PCH,DEV=0",
        "surround71:CARD=PCH,DEV=0",
        "iec958:CARD=PCH,DEV=0",
    ] {
        assert!(!listed(name), "{name}");
    }
}

#[test]
fn descriptions_become_one_line_names() {
    assert_eq!(
        device_name(
            Some("HDA Intel PCH, ALC892 Analog\nDefault Audio Device"),
            "x"
        ),
        "HDA Intel PCH, ALC892 Analog, Default Audio Device"
    );
    assert_eq!(device_name(Some("  \n"), "hw:0"), "hw:0");
    assert_eq!(device_name(None, "default"), "default");
}

#[test]
fn frames_become_100ns_ticks() {
    assert_eq!(hns_for_frames(48_000, 48_000), 10_000_000);
    assert_eq!(hns_for_frames(480, 48_000), 100_000);
    assert_eq!(hns_for_frames(441, 44_100), 100_000);
    assert_eq!(hns_for_frames(-480, 48_000), -100_000);
    assert_eq!(hns_for_frames(1, 0), 0);
    assert_eq!(hns_for_frames(i64::MAX, 1), i64::MAX);
}

#[test]
fn missing_devices_are_not_found_and_the_rest_os_errors() {
    for code in [-2, -6, -19] {
        let err = alsa_error("snd_pcm_open", code, "gone".into(), "hw:9");
        assert!(
            matches!(&err, PlatformError::NotFound { kind: "audio device", id } if id == "hw:9"),
            "{code}: {err:?}"
        );
    }
    let err = alsa_error(
        "snd_pcm_open",
        -16,
        "Device or resource busy".into(),
        "hw:0",
    );
    let PlatformError::Os {
        call,
        code,
        message,
    } = err
    else {
        panic!("expected Os, got {err:?}");
    };
    assert_eq!((call, code), ("snd_pcm_open", Some(16)));
    assert_eq!(message, "Device or resource busy");
}

/// Against ALSA's own `null` PCM, which needs no sound card: so it runs wherever
/// libasound and its configuration are installed, and checks only the error otherwise.
#[cfg(target_os = "linux")]
mod null_device {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use jstream_platform::{
        AudioInputHandler, AudioInputTarget, AudioIo, AudioOutputHandler, AudioOutputTarget,
        AudioPacket, AudioRenderBuffer, DeviceId, MediaClock, PlatformError,
    };

    use super::super::AlsaAudio;
    use crate::clock::MonotonicClock;

    /// The backend, or `None` (after checking the error names a remedy) when libasound
    /// isn't installed.
    fn audio() -> Option<AlsaAudio> {
        match AlsaAudio::new() {
            Ok(audio) => Some(audio),
            Err(PlatformError::MissingComponent { component, remedy }) => {
                assert_eq!(component, "ALSA");
                assert!(remedy.contains("libasound"), "{remedy}");
                None
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    fn null() -> DeviceId {
        DeviceId::new("null")
    }

    #[derive(Default)]
    struct Seen {
        packets: AtomicU64,
        samples: AtomicU64,
        errors: AtomicU64,
        last_position: AtomicU64,
        last_time: AtomicU64,
        positions_in_order: AtomicU64,
    }

    struct Record(Arc<Seen>, u64);

    impl AudioInputHandler for Record {
        fn on_packet(&mut self, packet: &AudioPacket<'_>) {
            let seen = &self.0;
            if packet.device_position == self.1 {
                seen.positions_in_order.fetch_add(1, Ordering::Relaxed);
            }
            self.1 = packet.device_position + (packet.samples.len() / 2) as u64;
            seen.packets.fetch_add(1, Ordering::Relaxed);
            seen.samples
                .fetch_add(packet.samples.len() as u64, Ordering::Relaxed);
            seen.last_position
                .store(packet.device_position, Ordering::Relaxed);
            if let Some(time) = packet.time {
                seen.last_time
                    .store(time.ticks().cast_unsigned(), Ordering::Relaxed);
            }
        }

        fn on_error(&mut self, _error: &PlatformError) {
            self.0.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct Play(Arc<Seen>);

    impl AudioOutputHandler for Play {
        fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>) {
            buffer.samples.fill(0.25);
            self.0.packets.fetch_add(1, Ordering::Relaxed);
            self.0
                .samples
                .fetch_add(buffer.samples.len() as u64, Ordering::Relaxed);
        }

        fn on_error(&mut self, _error: &PlatformError) {
            self.0.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn the_null_device_records_on_the_media_clock_and_stops_on_drop() {
        let Some(audio) = audio() else { return };
        let target = AudioInputTarget::Input(null());
        let format = audio.input_format(&target).expect("null's format");
        assert_eq!(format.channels(), 2, "stereo asked, and null takes it");
        let seen = Arc::new(Seen::default());
        let clock = MonotonicClock::new().expect("clock");
        let before = clock.now().ticks();
        let stream = audio
            .open_input(&target, Box::new(Record(Arc::clone(&seen), 0)))
            .expect("open null for capture");
        assert_eq!(stream.format(), format);
        std::thread::sleep(Duration::from_millis(300));
        drop(stream);
        let after = clock.now().ticks();
        let packets = seen.packets.load(Ordering::Relaxed);
        assert!(packets > 0, "no packets");
        assert_eq!(seen.errors.load(Ordering::Relaxed), 0);
        assert_eq!(
            seen.positions_in_order.load(Ordering::Relaxed),
            packets,
            "each packet starts where the last ended"
        );
        assert_eq!(seen.samples.load(Ordering::Relaxed) % 2, 0, "whole frames");
        // A packet's time is when its first frame was recorded: before it was read, so
        // before the stream was dropped. ALSA's null device fills at once, so its queue
        // can put that time before the stream opened, but not by more than a buffer.
        let last = seen.last_time.load(Ordering::Relaxed).cast_signed();
        assert!(last <= after, "{last} after {after}");
        assert!(last > before - 10_000_000, "{last} long before {before}");
        // Stopped: nothing arrives after the drop.
        let count = seen.packets.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(seen.packets.load(Ordering::Relaxed), count);
    }

    #[test]
    fn the_null_device_plays_whole_periods_and_stops_on_drop() {
        let Some(audio) = audio() else { return };
        let seen = Arc::new(Seen::default());
        let stream = audio
            .open_output(
                &AudioOutputTarget::Output(null()),
                Box::new(Play(Arc::clone(&seen))),
            )
            .expect("open null for playback");
        let channels = stream.format().channels() as u64;
        std::thread::sleep(Duration::from_millis(200));
        drop(stream);
        assert!(seen.packets.load(Ordering::Relaxed) > 0, "never rendered");
        assert_eq!(seen.errors.load(Ordering::Relaxed), 0);
        assert_eq!(seen.samples.load(Ordering::Relaxed) % channels, 0);
        let count = seen.packets.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(seen.packets.load(Ordering::Relaxed), count, "stopped");
    }

    #[test]
    fn loopback_and_processes_need_pipewire() {
        let Some(audio) = audio() else { return };
        for target in [
            AudioInputTarget::DefaultOutputLoopback,
            AudioInputTarget::OutputLoopback(null()),
            AudioInputTarget::ProcessLoopback { pid: 1 },
        ] {
            let err = audio
                .input_format(&target)
                .expect_err("no loopback in ALSA");
            assert!(
                matches!(&err, PlatformError::Unsupported { what } if what.contains("PipeWire")),
                "{target:?}: {err:?}"
            );
        }
        assert!(matches!(
            audio.processes(),
            Err(PlatformError::Unsupported { .. })
        ));
    }

    #[test]
    fn an_unknown_device_is_not_found() {
        let Some(audio) = audio() else { return };
        let id = DeviceId::new("jstream_no_such_pcm");
        let err = audio
            .input_format(&AudioInputTarget::Input(id.clone()))
            .expect_err("no such PCM");
        assert!(
            matches!(&err, PlatformError::NotFound { id: got, .. } if got == id.as_str()),
            "{err:?}"
        );
        let err = audio
            .input_format(&AudioInputTarget::Input(DeviceId::new("nul\0l")))
            .expect_err("a NUL in the name");
        assert!(matches!(err, PlatformError::NotFound { .. }), "{err:?}");
    }

    #[test]
    fn listing_skips_null_and_names_the_default_once_per_direction() {
        let Some(audio) = audio() else { return };
        let devices = audio.devices().expect("ALSA lists its PCMs without a card");
        assert!(
            devices.iter().all(|d| d.id.as_str() != "null"),
            "{devices:?}"
        );
        for direction in [
            jstream_platform::AudioDirection::Input,
            jstream_platform::AudioDirection::Output,
        ] {
            let defaults = devices
                .iter()
                .filter(|d| d.direction == direction && d.is_default)
                .count();
            assert!(defaults <= 1, "{direction:?}: {devices:?}");
        }
    }
}

/// Against a real sound card, behind `hw-tests` and `#[ignore]`d: ALSA's `null` device
/// isn't paced (it hands over frames as fast as they are read), so only a real one shows
/// that a second of recording is a second of audio. Run on a Linux machine with a
/// microphone: `cargo test -p jstream-platform-linux --features hw-tests -- --ignored
/// default_input`.
#[cfg(all(target_os = "linux", feature = "hw-tests"))]
mod devices {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use jstream_platform::{
        AudioInputHandler, AudioInputTarget, AudioIo, AudioPacket, PlatformError,
    };

    use super::super::AlsaAudio;

    struct Count(Arc<AtomicU64>);

    impl AudioInputHandler for Count {
        fn on_packet(&mut self, packet: &AudioPacket<'_>) {
            self.0
                .fetch_add(packet.samples.len() as u64, Ordering::Relaxed);
        }

        fn on_error(&mut self, _error: &PlatformError) {}
    }

    #[test]
    #[ignore = "needs a sound card with a microphone"]
    fn the_default_input_records_in_real_time() {
        let audio = AlsaAudio::new().expect("libasound");
        let samples = Arc::new(AtomicU64::new(0));
        let stream = audio
            .open_input(
                &AudioInputTarget::DefaultInput,
                Box::new(Count(Arc::clone(&samples))),
            )
            .expect("open the default input");
        let format = stream.format();
        std::thread::sleep(Duration::from_secs(1));
        drop(stream);
        let seconds = samples.load(Ordering::Relaxed) as f64
            / format.channels() as f64
            / f64::from(format.sample_rate());
        assert!((0.7..1.3).contains(&seconds), "{seconds} s recorded in 1 s");
    }
}
