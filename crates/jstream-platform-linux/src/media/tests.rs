use std::io::Write;
use std::path::{Path, PathBuf};

use jstream_platform::media::{ChannelLayout, MediaTime, PixelFormat, Timebase};
use jstream_platform::{
    HardwarePreference, MediaDecoder, MediaDecoderConfig, MediaDecoders, PlatformError,
};

use super::LinuxMediaDecoders;

fn config(audio: bool) -> MediaDecoderConfig {
    MediaDecoderConfig {
        video_format: PixelFormat::Rgba8,
        hardware: HardwarePreference::Prefer,
        audio,
    }
}

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/testdata")
        .join(name)
}

/// Reads every block, returning the frame count, the first block's `pts` in seconds and
/// the largest sample.
fn read_all(decoder: &mut dyn MediaDecoder) -> (usize, f64, f32) {
    let mut frames = 0;
    let mut first = None;
    let mut peak = 0.0_f32;
    let mut next: Option<f64> = None;
    while let Some(block) = decoder.next_audio().expect("decodes") {
        let pts = block.pts().as_secs_f64();
        first.get_or_insert(pts);
        // Each block starts where the one before it ended: no gap, no overlap.
        let rate = f64::from(block.format().sample_rate());
        if let Some(expected) = next {
            assert!(
                (pts - expected).abs() < 0.5 / rate,
                "block at {pts}, expected {expected}"
            );
        }
        next = Some(pts + block.frames() as f64 / rate);
        frames += block.frames();
        peak = block.samples().iter().fold(peak, |p, s| p.max(s.abs()));
    }
    (frames, first.unwrap_or(-1.0), peak)
}

fn plays(name: &str, layout: ChannelLayout) {
    let mut decoder = LinuxMediaDecoders
        .open(&testdata(name), &config(true))
        .unwrap_or_else(|err| panic!("{name}: {err}"));
    let info = *decoder.info();
    assert_eq!(info.video, None, "{name}");
    let audio = info.audio.expect("audio");
    assert_eq!(audio.format.sample_rate(), 44_100, "{name}");
    assert_eq!(audio.format.layout(), layout, "{name}");
    assert!(decoder.next_video().expect("no video").is_none());

    let (frames, first, peak) = read_all(decoder.as_mut());
    // Half a second, give or take a codec frame of padding the stream doesn't declare.
    assert!(
        (22_050..=22_050 + 2_048).contains(&frames),
        "{name}: {frames} frames"
    );
    assert!(first.abs() < 1e-9, "{name}: starts at {first}");
    assert!(peak > 0.05 && peak <= 1.0, "{name}: peak {peak}");
    assert!(decoder.next_audio().expect("end").is_none(), "{name}");

    // Looping: back to the start, and the same audio again.
    decoder
        .seek(MediaTime::zero(Timebase::MICROS))
        .expect("seeks");
    let (again, _, _) = read_all(decoder.as_mut());
    assert_eq!(again, frames, "{name}");

    // Halfway in, then past the end.
    decoder
        .seek(MediaTime::new(250_000, Timebase::MICROS))
        .expect("seeks");
    let block = decoder.next_audio().expect("decodes").expect("a block");
    let at = block.pts().as_secs_f64();
    assert!((0.249..=0.251).contains(&at), "{name}: {at}");
    decoder
        .seek(MediaTime::new(10_000_000, Timebase::MICROS))
        .expect("seeks");
    assert!(decoder.next_audio().expect("end").is_none(), "{name}");
}

#[test]
fn plays_flac() {
    plays("tone.flac", ChannelLayout::Stereo);
}

#[test]
fn plays_mp3() {
    plays("tone.mp3", ChannelLayout::Stereo);
}

#[test]
fn plays_ogg_vorbis() {
    plays("tone.ogg", ChannelLayout::Mono);
}

/// A 16-bit PCM WAV of `channels` at 48 kHz, `frames` long, every sample `value`.
fn wav(channels: u16, frames: u32, value: i16) -> Vec<u8> {
    let data_len = frames * u32::from(channels) * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&48_000_u32.to_le_bytes());
    out.extend_from_slice(&(48_000 * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for _ in 0..frames * u32::from(channels) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// A file in the system's temp directory, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let path =
            std::env::temp_dir().join(format!("jstream-media-{}-{name}", std::process::id()));
        let mut file = std::fs::File::create(&path).expect("creates");
        file.write_all(bytes).expect("writes");
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn plays_pcm_wav_and_mixes_six_channels_to_stereo() {
    let file = TempFile::new("six.wav", &wav(6, 4_800, 8_192));
    let mut decoder = LinuxMediaDecoders
        .open(&file.0, &config(true))
        .expect("opens");
    let audio = decoder.info().audio.expect("audio");
    assert_eq!(audio.format.sample_rate(), 48_000);
    assert_eq!(audio.format.layout(), ChannelLayout::Stereo);
    let duration = decoder.info().duration.expect("duration");
    assert!((duration.as_secs_f64() - 0.1).abs() < 1e-6, "{duration:?}");
    let (frames, first, peak) = read_all(decoder.as_mut());
    assert_eq!(frames, 4_800);
    assert!(first.abs() < 1e-9);
    assert!(peak > 0.2 && peak <= 0.25 + 1e-6, "{peak}");
}

#[test]
fn without_audio_a_sound_file_reads_nothing() {
    let mut decoder = LinuxMediaDecoders
        .open(&testdata("tone.flac"), &config(false))
        .expect("opens");
    assert_eq!(decoder.info().audio, None);
    assert!(decoder.next_audio().expect("nothing").is_none());
}

#[test]
fn a_missing_file_is_not_found() {
    let result = LinuxMediaDecoders.open(Path::new("/nonexistent/clip.mp4"), &config(true));
    assert!(
        matches!(result, Err(PlatformError::NotFound { .. })),
        "{:?}",
        result.err()
    );
}

#[test]
fn an_unknown_container_is_unsupported() {
    let file = TempFile::new("junk.avi", &[0x5A; 4_096]);
    let result = LinuxMediaDecoders.open(&file.0, &config(true));
    assert!(
        matches!(result, Err(PlatformError::Unsupported { .. })),
        "{:?}",
        result.err()
    );
}
