//! Media files (ADR-0061): the [`MediaDecoders`](jstream_platform::MediaDecoders)
//! backend on the Media Foundation Source Reader (`reader`), which demuxes and decodes a
//! local file with the OS's own containers and codecs. Video comes out on the CPU as
//! packed RGBA or BGRA, converted from the reader's RGB32 (`convert`); frames on the GPU
//! follow the zero-copy work (E3). Audio comes out cut to its content, without the
//! encoder's priming and padding (`gapless`).

// Platform-independent, so it is tested on every platform.
#[cfg(any(windows, test))]
mod convert;
#[cfg(any(windows, test))]
mod gapless;
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
