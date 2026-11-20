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
    plays_at(name, layout, 44_100);
}

/// As [`plays`], for a file whose audio decodes at `rate`.
fn plays_at(name: &str, layout: ChannelLayout, rate: u32) {
    let mut decoder = LinuxMediaDecoders
        .open(&testdata(name), &config(true))
        .unwrap_or_else(|err| panic!("{name}: {err}"));
    let info = *decoder.info();
    assert_eq!(info.video, None, "{name}");
    let audio = info.audio.expect("audio");
    assert_eq!(audio.format.sample_rate(), rate, "{name}");
    assert_eq!(audio.format.layout(), layout, "{name}");
    assert!(decoder.next_video().expect("no video").is_none());

    let (frames, first, peak) = read_all(decoder.as_mut());
    // Half a second, give or take a codec frame of padding the stream doesn't declare.
    let half = usize::try_from(rate / 2).expect("fits");
    assert!(
        (half..=half + 2_048).contains(&frames),
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

#[test]
fn plays_ogg_opus() {
    plays_at("tone.opus", ChannelLayout::Mono, 48_000);
}

// AAC-LC (ADR-0160): an ADTS stream through symphonia's reader. ADTS declares no
// encoder delay, so its first 1 024 frames are the encoder's priming, within `plays`'s
// allowance.
#[test]
fn plays_adts_aac() {
    plays("tone.aac", ChannelLayout::Stereo);
}

// Its pre-skip (312 frames) is cut, from the start and after a seek back to it: the
// file's final granule position is 24 312, so exactly 24 000 frames play, from 0.
#[test]
fn an_ogg_opus_file_plays_without_its_pre_skip() {
    let mut decoder = LinuxMediaDecoders
        .open(&testdata("tone.opus"), &config(true))
        .expect("opens");
    for _ in 0..2 {
        let first = decoder.next_audio().expect("decodes").expect("a block");
        assert_eq!(first.pts().ticks(), 0);
        let mut frames = first.frames();
        while let Some(block) = decoder.next_audio().expect("decodes") {
            frames += block.frames();
        }
        assert_eq!(frames, 24_000);
        decoder
            .seek(MediaTime::zero(Timebase::MICROS))
            .expect("seeks");
    }
}

// A WebM file goes through the crate's own demuxer and the movie decoder, built on
// Linux only. Its audio needs no GPU.
#[cfg(target_os = "linux")]
#[test]
fn plays_webm_opus_and_mixes_five_one_to_stereo() {
    plays_at("tone-5.1.webm", ChannelLayout::Stereo, 48_000);
}

// AAC-LC in MP4, MOV and Matroska, through the crate's demuxers: the MP4's edit list cuts
// the encoder's priming.
#[cfg(target_os = "linux")]
#[test]
fn plays_aac_in_mp4_and_matroska() {
    plays("tone-aac.m4a", ChannelLayout::Stereo);
    plays("tone-aac.mkv", ChannelLayout::Stereo);
    // QuickTime's version 1 sound description: its `esds` is inside a `wave` box.
    plays("tone-aac.mov", ChannelLayout::Stereo);
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

// An ADTS stream whose header says AAC Main: symphonia would decode it with the LC
// tools, so it is refused by name instead.
#[test]
fn adts_beyond_aac_lc_is_unsupported() {
    let mut bytes = std::fs::read(testdata("tone.aac")).expect("reads");
    // The first frame's profile bits: LC (1) to Main (0).
    assert_eq!(&bytes[..3], &[0xFF, 0xF1, 0x50]);
    bytes[2] = 0x10;
    let file = TempFile::new("main.aac", &bytes);
    match LinuxMediaDecoders.open(&file.0, &config(true)) {
        Err(PlatformError::Unsupported { what }) => {
            assert!(what.contains("object type 1"), "{what}");
        }
        Err(err) => panic!("{err}"),
        Ok(_) => panic!("AAC Main doesn't decode"),
    }
}

// The same MP4 with its AudioSpecificConfig's backward-compatible extension saying SBR
// is present: explicit HE-AAC, refused by name rather than played as its LC core.
#[cfg(target_os = "linux")]
#[test]
fn explicit_he_aac_is_unsupported() {
    let mut bytes = std::fs::read(testdata("tone-aac.m4a")).expect("reads");
    // DecoderSpecificInfo: AAC-LC, 44.1 kHz, stereo, then 0x2B7, SBR, absent.
    let asc = [0x12, 0x10, 0x56, 0xE5, 0x00];
    let at = bytes
        .windows(asc.len())
        .position(|w| w == asc)
        .expect("the config");
    bytes[at + 4] = 0x98;
    let file = TempFile::new("he-aac.m4a", &bytes);
    match LinuxMediaDecoders.open(&file.0, &config(true)) {
        Err(PlatformError::Unsupported { what }) => {
            assert!(what.contains("HE-AAC"), "{what}");
        }
        Err(err) => panic!("{err}"),
        Ok(_) => panic!("explicit HE-AAC doesn't decode"),
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

// Movies decode through VA-API, so only on Linux; elsewhere the crate is built for its
// portable parts' tests.
#[cfg(target_os = "linux")]
#[test]
fn a_movie_opens_or_names_the_missing_decoder() {
    // On a machine with a VA-API H.264 decoder it opens; elsewhere (CI) it names what
    // to install. MP3 audio in the MP4, Vorbis in the Matroska file.
    for name in ["h264-high.mp4", "h264-vorbis.mkv"] {
        match LinuxMediaDecoders.open(&testdata(name), &config(true)) {
            Ok(decoder) => {
                let info = decoder.info();
                let video = info.video.expect("video");
                assert_eq!((video.size.width(), video.size.height()), (120, 90));
                assert!(info.audio.is_some(), "{name}: audio");
            }
            Err(PlatformError::MissingComponent { component, remedy }) => {
                assert!(component.contains("VA-API"), "{component}");
                assert!(remedy.contains("mesa-va-drivers"), "{remedy}");
            }
            Err(err) => panic!("{name}: {err}"),
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_vp9_webm_opens_or_names_the_missing_decoder() {
    // Profile 0, with a hidden alternate reference: on a machine with a VA-API VP9
    // decoder it opens; elsewhere (CI) it names what to install.
    match LinuxMediaDecoders.open(&testdata("vp9-altref.webm"), &config(true)) {
        Ok(decoder) => {
            let video = decoder.info().video.expect("video");
            assert_eq!((video.size.width(), video.size.height()), (96, 64));
        }
        Err(PlatformError::MissingComponent { component, remedy }) => {
            assert!(component.contains("VP9"), "{component}");
            assert!(remedy.contains("mesa-va-drivers"), "{remedy}");
        }
        Err(err) => panic!("{err}"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_vp8_webm_opens_or_names_the_missing_decoder() {
    // An odd-sized stream with token partitions: on a machine with a VA-API VP8
    // decoder it opens; elsewhere (CI) it names what to install.
    match LinuxMediaDecoders.open(&testdata("vp8-parts.webm"), &config(true)) {
        Ok(decoder) => {
            let video = decoder.info().video.expect("video");
            assert_eq!((video.size.width(), video.size.height()), (126, 70));
        }
        Err(PlatformError::MissingComponent { component, remedy }) => {
            assert!(component.contains("VP8"), "{component}");
            assert!(remedy.contains("mesa-va-drivers"), "{remedy}");
        }
        Err(err) => panic!("{err}"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn an_av1_webm_opens_or_names_the_missing_decoder() {
    // Film grain on every frame: on a machine with a VA-API AV1 decoder it opens;
    // elsewhere (CI) it names what to install.
    match LinuxMediaDecoders.open(&testdata("av1-grain.webm"), &config(true)) {
        Ok(decoder) => {
            let video = decoder.info().video.expect("video");
            assert_eq!((video.size.width(), video.size.height()), (176, 96));
        }
        Err(PlatformError::MissingComponent { component, remedy }) => {
            assert!(component.contains("AV1"), "{component}");
            assert!(remedy.contains("mesa-va-drivers"), "{remedy}");
        }
        Err(err) => panic!("{err}"),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn vp9_beyond_profile_0_is_unsupported() {
    // An RGB (4:4:4, profile 1) VP9 stream, refused before any GPU is looked for.
    match LinuxMediaDecoders.open(&testdata("vp9-opus.webm"), &config(true)) {
        Err(PlatformError::Unsupported { what }) => assert!(what.contains("profile 1"), "{what}"),
        Err(err) => panic!("{err}"),
        Ok(_) => panic!("profile 1 doesn't decode"),
    }
}
