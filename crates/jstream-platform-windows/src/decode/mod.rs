//! Media files (ADR-0140): the [`MediaDecoders`](jstream_platform::MediaDecoders)
//! backend on the Media Foundation Source Reader (`reader`), which demuxes and decodes a
//! local file with the OS's own containers and codecs. Video comes out on the CPU as
//! packed RGBA or BGRA, converted from the reader's RGB32 (`convert`); frames on the GPU
//! follow the zero-copy work (E3). Audio comes out cut to its content, without the
//! encoder's priming and padding (`gapless`), an MP3's read from its LAME tag (`lame`).
//!
//! # Behaviour
//!
//! **`MfMediaDecoders`** (`MediaDecoders`), local media files through the Media
//! Foundation Source Reader (`decode/`,
//! ADR-0140):
//! - **Opening:** `MFCreateSourceReaderFromURL` on the file's host path, with the
//!   `\\?\` prefix `canonicalize` adds removed, since the source resolver would read it
//!   as a URL scheme. Advanced video processing is on, so the reader converts the
//!   decoder's YUV to RGB32 and deinterlaces; `HardwarePreference::Prefer` also turns on
//!   hardware decoders, `Avoid` leaves them off, and `Require` is `Unsupported`, because
//!   the reader doesn't say which decoder it picked. Only the first video stream is
//!   selected, and the first audio stream too when `MediaDecoderConfig::audio` asks for
//!   it. The file plays what Media Foundation does: MP4, MOV, MKV and WebM, and
//!   H.264, HEVC, VP9 and AV1 where the OS has the decoder (Store extensions for the
//!   last three). A missing file is `NotFound`; a container or codec the OS can't read
//!   is `Unsupported`.
//! - **Frames:** each RGB32 sample is copied into packed RGBA8 (or BGRA8) with alpha
//!   255, top row first, through `IMF2DBuffer::Lock2D` when the buffer has it (which
//!   gives the real pitch, negative for a bottom-up picture) and the type's default
//!   stride otherwise. The picture is cropped to the type's minimum display aperture,
//!   so a decoder that pads 1080 rows to 1088 shows 1080. A frame's `pts` is the
//!   sample's time, in 100 ns, on the file's timeline. A media type change mid-stream
//!   re-reads the shape. Colour is BT.709 full range, as RGB.
//! - **Audio:** the reader decodes to float PCM at the stream's own rate
//!   (`MFAudioFormat_Float`), in the stream's channel count when that is mono, stereo or
//!   6 (read as 5.1), else asking the decoder for stereo. A stream it can't select, or
//!   decode either way, is left unselected and `info().audio` is `None`, so the video still plays.
//!   Each sample is copied out as whole frames of little-endian `f32`, its `pts` the
//!   sample's time counted in frames at the stream's rate. The reader leaves in the
//!   encoder's priming and padding, which a looping file heard as a 25-40 ms hole at
//!   each wrap (#445), so `decode/gapless.rs` cuts them: the priming is
//!   `MF_SD_AUDIO_ENCODER_DELAY` when the source reports it, else 1024 frames for AAC
//!   (FFmpeg's, whose edit list Media Foundation doesn't apply), else none; the padding is
//!   `MF_SD_AUDIO_ENCODER_PADDING`, for which the last block is held back until the end
//!   of the stream shows it is the last. For an MP3 whose source reports neither (the
//!   reported values are logged), both come from its LAME tag instead (`decode/lame.rs`):
//!   the priming is the encoder's plus the decoder's 529 frames, plus the Info frame,
//!   which the decoder most likely decodes as silence, and the padding is the encoder's
//!   less 529. Either way, nothing past `MF_PD_DURATION` is kept. For an MP3 cut from its LAME tag, a whole
//!   pass from the start counts its frames: when the count says the Info frame wasn't
//!   decoded after all, the passes after it cut one frame less (both choices are logged,
//!   for the hardware rerun).
//!   Times then count from the content's first frame. A seek forgets a held block. A
//!   format change mid-stream is `Unsupported`, since the engine's mixer input was made
//!   for the first.
//! - **Seeking** sets the reader's position in 100 ns units, for both streams; the
//!   reader restarts video at the key frame before it. A time past the end, which the
//!   reader refuses with `MF_E_INVALID_POSITION`, leaves both streams ended until the
//!   next seek.
//! - **Threads:** `open` joins the MTA for its calls; each open file holds Media
//!   Foundation started and the MTA alive (`MfStarted`), so the thread that reads it,
//!   the source's decode thread, uses the free-threaded reader as an implicit MTA member.
//! - **Not yet:** frames on the GPU (they follow the zero-copy work).

// Platform-independent, so it is tested on every platform.
#[cfg(any(windows, test))]
mod convert;
#[cfg(any(windows, test))]
mod gapless;
#[cfg(any(windows, test))]
mod lame;
#[cfg(windows)]
mod reader;

#[cfg(windows)]
pub use reader::MfMediaDecoders;

/// Platform tests against Media Foundation (R-TEST-004): behind `hw-tests` and
/// `#[ignore]`d. They play the video file `JSTREAM_TEST_MEDIA` names, any file with
/// sound that Windows' Films & TV app plays:
/// `$env:JSTREAM_TEST_MEDIA = "C:\path\to\clip.mp4"; cargo test -p jstream-platform-windows --features hw-tests -- --ignored media_file`.
#[cfg(all(test, windows, feature = "hw-tests"))]
mod device_tests {
    use std::path::{Path, PathBuf};

    use jstream_platform::media::{MediaTime, PixelFormat, SampleFormat, Timebase};
    use jstream_platform::{HardwarePreference, MediaDecoderConfig, MediaDecoders, PlatformError};

    use super::MfMediaDecoders;

    const RGBA: MediaDecoderConfig = MediaDecoderConfig {
        video_format: PixelFormat::Rgba8,
        hardware: HardwarePreference::Prefer,
        audio: false,
    };

    fn clip() -> PathBuf {
        let path = std::env::var_os("JSTREAM_TEST_MEDIA")
            .expect("set JSTREAM_TEST_MEDIA to a video file the OS can play");
        PathBuf::from(path).canonicalize().expect("the video file")
    }

    fn ms(t: MediaTime) -> i64 {
        t.rescale(Timebase::MILLIS, jstream_platform::media::Rounding::Down)
            .expect("a time")
            .ticks()
    }

    #[test]
    #[ignore = "needs Media Foundation and a video file"]
    fn media_file_decodes_frames_in_order_and_seeks() {
        let mut file = MfMediaDecoders::new().open(&clip(), &RGBA).expect("open");
        let info = *file.info();
        let video = info.video.expect("a video stream");
        let duration = info.duration.expect("a duration");
        assert!(ms(duration) > 2_000, "use a clip over two seconds long");
        let mut last = None;
        for i in 0..30 {
            let frame = file.next_video().expect("decode").expect("a frame");
            assert_eq!(frame.info().size, video.size, "frame {i}");
            assert_eq!(frame.info().format, PixelFormat::Rgba8);
            let row = frame.row(0, video.size.height() / 2).expect("a row");
            assert!(row.chunks_exact(4).all(|px| px[3] == 0xff), "opaque");
            let pts = ms(frame.info().pts);
            assert!(last.is_none_or(|last| pts > last), "frame {i} at {pts} ms");
            last = Some(pts);
        }
        // To the middle: the first frame after is at or before it (decoding restarts
        // at a key frame), and within a few seconds of it.
        let middle = ms(duration) / 2;
        file.seek(MediaTime::new(middle, Timebase::MILLIS))
            .expect("seek");
        let frame = file.next_video().expect("decode").expect("a frame");
        let pts = ms(frame.info().pts);
        assert!(
            pts <= middle + 100 && pts + 10_000 >= middle,
            "{pts} ms for {middle}"
        );
        // Past the end: the end of the stream.
        file.seek(MediaTime::new(ms(duration) + 5_000, Timebase::MILLIS))
            .expect("seek past the end");
        let mut extra = 0;
        while file.next_video().expect("decode").is_some() {
            extra += 1;
            assert!(extra < 600, "the stream ends");
        }
    }

    #[test]
    #[ignore = "needs Media Foundation and a video file"]
    fn media_file_decodes_audio_in_order_and_seeks_it() {
        let config = MediaDecoderConfig {
            audio: true,
            ..RGBA
        };
        let mut file = MfMediaDecoders::new().open(&clip(), &config).expect("open");
        let format = file.info().audio.expect("an audio stream").format;
        assert_eq!(format.sample_format(), SampleFormat::F32Interleaved);
        let mut end = None;
        let mut frames = 0;
        while frames < usize::try_from(format.sample_rate()).expect("a rate") {
            let block = file.next_audio().expect("decode").expect("a block");
            assert_eq!(block.format(), format);
            assert!(
                block.samples().iter().all(|s| s.is_finite()),
                "finite samples"
            );
            // Each block starts where the last ended, give or take a millisecond.
            let pts = ms(block.pts());
            assert!(
                end.is_none_or(|end: i64| (pts - end).abs() <= 1),
                "{pts} ms after {end:?}"
            );
            end = Some(
                ms(block.pts())
                    + i64::try_from(block.frames()).expect("frames") * 1000
                        / i64::from(format.sample_rate()),
            );
            frames += block.frames();
            // Video interleaves with it, as the source reads both.
            let _ = file.next_video().expect("decode");
        }
        let middle = ms(file.info().duration.expect("a duration")) / 2;
        file.seek(MediaTime::new(middle, Timebase::MILLIS))
            .expect("seek");
        let block = file
            .next_audio()
            .expect("decode")
            .expect("a block after the seek");
        let pts = ms(block.pts());
        assert!(
            pts <= middle + 100 && pts + 10_000 >= middle,
            "{pts} ms for {middle}"
        );
        // Without asking, there is no audio.
        let mut silent = MfMediaDecoders::new().open(&clip(), &RGBA).expect("open");
        assert_eq!(silent.info().audio, None);
        assert_eq!(silent.next_audio().expect("no error"), None);
    }

    #[test]
    #[ignore = "needs Media Foundation"]
    fn media_file_errors_name_what_is_wrong() {
        let missing = Path::new(r"C:\no\such\folder\clip.mp4");
        assert!(matches!(
            MfMediaDecoders::new().open(missing, &RGBA),
            Err(PlatformError::NotFound { .. })
        ));
        let dir = std::env::temp_dir().join("jstream-media-file-test");
        std::fs::create_dir_all(&dir).expect("a temp folder");
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, b"not a media file at all").expect("a file");
        assert!(matches!(
            MfMediaDecoders::new().open(&junk.canonicalize().expect("the file"), &RGBA),
            Err(PlatformError::Unsupported { .. } | PlatformError::Os { .. })
        ));
        let nv12 = MediaDecoderConfig {
            video_format: PixelFormat::Nv12,
            ..RGBA
        };
        assert!(matches!(
            MfMediaDecoders::new().open(&clip(), &nv12),
            Err(PlatformError::Unsupported { .. })
        ));
    }
}
