//! Local media files through AVFoundation (ADR-0061, ADR-0063 §6): an `AVURLAsset` read by
//! an `AVAssetReader`, which demuxes and decodes with the OS decoders (VideoToolbox for
//! video, Core Audio's converters for audio). This module asks it for the formats
//! [`av_reader_settings`] and [`audio_layout`] choose and hands the samples on as CPU
//! frames and interleaved `f32` blocks.
//!
//! Opening loads the asset's tracks (through `loadTracksWithMediaType:completionHandler:`,
//! waited on), starts a reader with one output per stream, and reads the first video
//! frame to learn the size of every frame. Seeking starts a new reader on the time range
//! from the seek target to the end: AVFoundation decodes from the key frame before it.
//! A reader is driven by one thread at a time, as the trait asks.
//!
//! AVFoundation trims an AAC track's priming and padding where the container's edit list
//! says, so a looping file joins up without a gap there.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use block2::RcBlock;
use jstream_platform::media::{
    AudioBuffer, AudioFormat, MediaTime, PixelFormat, SampleFormat, Timebase, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::{
    MediaAudioInfo, MediaDecoder, MediaDecoderConfig, MediaDecoders, MediaInfo, MediaVideoInfo,
    PlatformError,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAsset, AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack,
    AVFoundationErrorDomain, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo, AVURLAsset,
};
use objc2_core_foundation::CFString;
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMFormatDescription, CMSampleBuffer, CMTime,
    CMTimeFlags, CMTimeRange, kCMTimePositiveInfinity,
};
use objc2_core_video::kCVPixelBufferPixelFormatTypeKey;
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL};

use crate::annexb::{cm_time_parts, media_time_from_cm};
use crate::audio::rate_hz;
use crate::fourcc::fourcc;
use crate::media_file::{
    AvReaderSettings, audio_layout, av_error_is_unsupported, av_reader_settings, bgra_to_rgba,
};
use crate::vtdecoder::copy_image;

/// What `NotFound` names.
const KIND: &str = "media file";

/// How long loading an asset's tracks may take: a local file answers in milliseconds,
/// so this only stops a wedged load from hanging the source's thread.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// `kAudioFormatLinearPCM`.
const LINEAR_PCM: u32 = fourcc(b"lpcm");

/// The time base video frames are timed in when a track has no timescale of its own.
const FALLBACK_TIMESCALE: u32 = 600;

/// Local media files through AVFoundation's asset reader: the [`MediaDecoders`] backend
/// on macOS. It plays what AVFoundation does: MP4, MOV and M4V among containers (not MKV
/// or WebM), and H.264, HEVC and ProRes video with AAC, ALAC and MP3 audio, among
/// others.
#[derive(Clone, Copy, Debug, Default)]
pub struct AvMediaDecoders;

impl AvMediaDecoders {
    /// The backend.
    pub fn new() -> Self {
        Self
    }
}

impl MediaDecoders for AvMediaDecoders {
    fn open(
        &self,
        path: &Path,
        config: &MediaDecoderConfig,
    ) -> Result<Box<dyn MediaDecoder>, PlatformError> {
        let settings = av_reader_settings(config)?;
        if !path.is_file() {
            return Err(PlatformError::NotFound {
                kind: KIND,
                id: path.display().to_string(),
            });
        }
        let path_str = path.to_str().ok_or_else(|| PlatformError::Unsupported {
            what: format!("a media file path that isn't UTF-8 ({})", path.display()),
        })?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
        // SAFETY: a file URL, and no options.
        let asset = unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) };
        // SAFETY: AVFoundation constants, set when the framework loads and never written.
        let (video_type, audio_type) = unsafe { (AVMediaTypeVideo, AVMediaTypeAudio) };
        let video_track = match video_type {
            Some(t) => first_track(&asset, t)?,
            None => None,
        };
        let audio_track = match (config.audio, audio_type) {
            (true, Some(t)) => first_track(&asset, t)?,
            _ => None,
        };
        let audio = audio_track
            .as_ref()
            .and_then(|t| audio_format(t).map(|format| (t.clone(), format)));
        if video_track.is_none() && audio.is_none() {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a media file with no video or audio AVFoundation reads ({})",
                    path.display()
                ),
            });
        }
        let video_tb = video_track
            .as_ref()
            // SAFETY: a plain property read on a live track.
            .and_then(|t| u32::try_from(unsafe { t.naturalTimeScale() }).ok())
            .filter(|&ts| ts > 0)
            .unwrap_or(FALLBACK_TIMESCALE);
        let video_tb = Timebase::new(1, video_tb)?;
        let mut decoder = AvMediaDecoder {
            reader: None,
            asset: Retained::into_super(asset),
            video_track,
            audio,
            settings,
            video_tb,
            info: MediaInfo {
                video: None,
                audio: None,
                duration: None,
            },
            peeked: None,
        };
        if let Err(error) = decoder.start(None) {
            // A file whose sound AVFoundation won't decode still plays its picture, with
            // no audio (`MediaInfo::audio` is `None`), as on Windows. The caller sees
            // that in `info`; this crate has no logger.
            if !matches!(error, PlatformError::Unsupported { .. })
                || decoder.audio.is_none()
                || decoder.video_track.is_none()
            {
                return Err(error);
            }
            decoder.audio = None;
            decoder.start(None)?;
        }
        let first = decoder.read_video()?;
        decoder.info = MediaInfo {
            video: first.as_ref().map(|f| MediaVideoInfo {
                size: f.info().size,
                frame_period: decoder.video_track.as_deref().and_then(frame_period),
            }),
            audio: decoder
                .audio
                .as_ref()
                .map(|&(_, format)| MediaAudioInfo { format }),
            duration: duration(&decoder.asset),
        };
        decoder.peeked = first;
        Ok(Box::new(decoder))
    }
}

/// An `NSError` from AVFoundation: `Unsupported` for a file it doesn't play, `Os` for the
/// rest.
fn ns_error(call: &'static str, error: &NSError) -> PlatformError {
    // SAFETY: an AVFoundation constant, set when the framework loads and never written.
    let domain: Option<&NSString> = unsafe { AVFoundationErrorDomain };
    let av = domain.is_some_and(|d| error.domain().isEqualToString(d));
    let code = error.code();
    let description = error.localizedDescription().to_string();
    if av && av_error_is_unsupported(code) {
        PlatformError::Unsupported {
            what: format!("this file's format ({description})"),
        }
    } else {
        PlatformError::Os {
            call,
            code: i32::try_from(code).ok().map(i32::cast_unsigned),
            message: description,
        }
    }
}

fn os(call: &'static str, message: &str) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: message.into(),
    }
}

/// The asset's first track of `media_type`, loaded and waited for.
fn first_track(
    asset: &AVURLAsset,
    media_type: &AVMediaType,
) -> Result<Option<Retained<AVAssetTrack>>, PlatformError> {
    const CALL: &str = "AVAsset loadTracksWithMediaType";
    let (tx, rx) = mpsc::sync_channel(1);
    let handler = RcBlock::new(
        move |tracks: *mut NSArray<AVAssetTrack>, error: *mut NSError| {
            let result = catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: AVFoundation passes each as null or an object valid for the
                // call; retaining takes this side's own reference.
                let (tracks, error) =
                    unsafe { (Retained::retain(tracks), Retained::retain(error)) };
                match (tracks, error) {
                    (_, Some(e)) => Err(ns_error(CALL, &e)),
                    (Some(t), None) => Ok(t.firstObject()),
                    (None, None) => Err(os(CALL, "returned neither tracks nor an error")),
                }
            }))
            .unwrap_or_else(|_| Err(os(CALL, "the completion handler panicked")));
            let _ = tx.try_send(result);
        },
    );
    // SAFETY: the method copies the block and calls it once, on a queue of its own. The
    // track it hands over moves to this thread, which only reads it after the load, and
    // retain counts are atomic.
    unsafe { asset.loadTracksWithMediaType_completionHandler(media_type, &handler) };
    match rx.recv_timeout(LOAD_TIMEOUT) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(os(CALL, "AVFoundation didn't answer in time")),
        Err(RecvTimeoutError::Disconnected) => Err(os(
            CALL,
            "AVFoundation released the completion handler without calling it",
        )),
    }
}

/// The format `track`'s audio is read in: its own rate, mono or stereo, interleaved
/// `f32`. `None` when its format description doesn't say.
fn audio_format(track: &AVAssetTrack) -> Option<AudioFormat> {
    // SAFETY: a plain property read on a live track.
    let descriptions = unsafe { track.formatDescriptions() };
    let first = descriptions.firstObject()?;
    // SAFETY: an audio track's `formatDescriptions` holds `CMFormatDescription`s
    // (toll-free bridged to objects), which outlive this borrow of the array.
    let desc: &CMFormatDescription =
        unsafe { &*ptr::from_ref::<AnyObject>(&first).cast::<CMFormatDescription>() };
    // SAFETY: `desc` is a live format description; the result is null, or a description
    // Core Media owns for as long as `desc` lives.
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(desc).as_ref() }?;
    let rate = rate_hz(asbd.mSampleRate)?;
    let layout = audio_layout(asbd.mChannelsPerFrame)?;
    AudioFormat::new(rate, layout, SampleFormat::F32Interleaved).ok()
}

/// One frame's nominal duration, when the track declares one.
fn frame_period(track: &AVAssetTrack) -> Option<Timebase> {
    // SAFETY: a plain property read on a live track.
    let t = unsafe { track.minFrameDuration() };
    if !t.flags.contains(CMTimeFlags::Valid) {
        return None;
    }
    let num = u32::try_from(t.value).ok().filter(|&n| n > 0)?;
    let den = u32::try_from(t.timescale).ok().filter(|&d| d > 0)?;
    Timebase::new(num, den).ok()
}

/// How long the asset plays, when it says.
fn duration(asset: &AVAsset) -> Option<MediaTime> {
    // SAFETY: a plain property read on a live asset; a local file's duration is read
    // from its header.
    let t = unsafe { asset.duration() };
    let valid = t.flags.contains(CMTimeFlags::Valid)
        && !t.flags.contains(CMTimeFlags::Indefinite)
        && !t.flags.contains(CMTimeFlags::PositiveInfinity);
    if !valid {
        return None;
    }
    let den = u32::try_from(t.timescale).ok().filter(|&d| d > 0)?;
    let tb = Timebase::new(1, den).ok()?;
    media_time_from_cm(t.value, t.timescale, tb).ok()
}

fn number_entry<'a>(key: &'a NSString, value: &'a NSNumber) -> (&'a NSString, &'a AnyObject) {
    (key, value)
}

/// The video output's settings: the pixel format.
fn video_settings(pixel_format: u32) -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: a Core Video constant, set before any code runs and never written. A
    // `CFString` is toll-free bridged to `NSString`.
    let key: &NSString =
        unsafe { &*ptr::from_ref::<CFString>(kCVPixelBufferPixelFormatTypeKey).cast::<NSString>() };
    let format = NSNumber::new_u32(pixel_format);
    let (k, v) = number_entry(key, &format);
    NSDictionary::from_slices(&[k], &[v])
}

/// The audio output's settings: linear PCM, 32-bit float, interleaved, little-endian, at
/// `format`'s rate and channel count. The keys are AVFAudio's `AV…Key` constants, whose
/// values are their names.
fn audio_settings(format: AudioFormat) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys = [
        "AVFormatIDKey",
        "AVSampleRateKey",
        "AVNumberOfChannelsKey",
        "AVLinearPCMBitDepthKey",
        "AVLinearPCMIsFloatKey",
        "AVLinearPCMIsNonInterleaved",
        "AVLinearPCMIsBigEndianKey",
    ]
    .map(NSString::from_str);
    let channels = u32::try_from(format.channels()).unwrap_or(2);
    let values = [
        NSNumber::new_u32(LINEAR_PCM),
        NSNumber::new_f64(f64::from(format.sample_rate())),
        NSNumber::new_u32(channels),
        NSNumber::new_u32(32),
        NSNumber::new_bool(true),
        NSNumber::new_bool(false),
        NSNumber::new_bool(false),
    ];
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&AnyObject> = values.iter().map(|v| -> &AnyObject { v }).collect();
    NSDictionary::from_slices(&keys, &values)
}

/// A started reader and its outputs.
struct Reader {
    reader: Retained<AVAssetReader>,
    video: Option<Retained<AVAssetReaderTrackOutput>>,
    audio: Option<Retained<AVAssetReaderTrackOutput>>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: a live reader; cancelling one that has finished does nothing.
        unsafe { self.reader.cancelReading() };
    }
}

/// One open media file.
struct AvMediaDecoder {
    reader: Option<Reader>,
    asset: Retained<AVAsset>,
    video_track: Option<Retained<AVAssetTrack>>,
    /// The audio track and the format it is read in.
    audio: Option<(Retained<AVAssetTrack>, AudioFormat)>,
    settings: AvReaderSettings,
    video_tb: Timebase,
    info: MediaInfo,
    /// The first frame, read while opening to learn the size.
    peeked: Option<VideoFrame>,
}

// SAFETY: AVFoundation's assets, tracks and readers have no thread affinity; Apple asks
// only that one reader isn't used from two threads at once. `AvMediaDecoder` is not
// `Sync`, and the trait's `&mut self` methods keep it on one thread at a time.
unsafe impl Send for AvMediaDecoder {}

impl AvMediaDecoder {
    /// Starts a reader at `from` on the file's timeline (the start for `None`).
    fn start(&mut self, from: Option<MediaTime>) -> Result<(), PlatformError> {
        const CALL: &str = "AVAssetReader";
        self.reader = None;
        self.peeked = None;
        // SAFETY: a live asset.
        let reader = unsafe { AVAssetReader::assetReaderWithAsset_error(&self.asset) }
            .map_err(|e| ns_error(CALL, &e))?;
        let video = match &self.video_track {
            Some(track) => {
                let settings = video_settings(self.settings.pixel_format);
                // SAFETY: a live video track and a pixel-format settings dictionary.
                let output = unsafe {
                    AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                        track,
                        Some(&settings),
                    )
                };
                add_output(&reader, &output, "video")?;
                Some(output)
            }
            None => None,
        };
        let audio = match &self.audio {
            Some((track, format)) => {
                let settings = audio_settings(*format);
                // SAFETY: a live audio track and a linear-PCM settings dictionary.
                let output = unsafe {
                    AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                        track,
                        Some(&settings),
                    )
                };
                add_output(&reader, &output, "audio")?;
                Some(output)
            }
            None => None,
        };
        if let Some(from) = from {
            let (value, timescale) =
                cm_time_parts(from).ok_or_else(|| PlatformError::Unsupported {
                    what: format!("seeking to a time in {} on macOS", from.timebase()),
                })?;
            let range = CMTimeRange {
                start: CMTime {
                    value: value.max(0),
                    timescale,
                    flags: CMTimeFlags::Valid,
                    epoch: 0,
                },
                // SAFETY: a Core Media constant, set before any code runs and never
                // written.
                duration: unsafe { kCMTimePositiveInfinity },
            };
            // SAFETY: a reader that hasn't started reading.
            unsafe { reader.setTimeRange(range) };
        }
        // SAFETY: a configured reader.
        if !unsafe { reader.startReading() } {
            // SAFETY: a plain property read on a live reader.
            return Err(match unsafe { reader.error() } {
                Some(e) => ns_error("AVAssetReader startReading", &e),
                None => os("AVAssetReader startReading", "failed without an error"),
            });
        }
        self.reader = Some(Reader {
            reader,
            video,
            audio,
        });
        Ok(())
    }

    /// The reader's error, once an output has returned no sample: `Ok` at the end.
    fn ended(&self) -> Result<(), PlatformError> {
        let Some(reader) = &self.reader else {
            return Ok(());
        };
        // SAFETY: plain property reads on a live reader.
        unsafe {
            if reader.reader.status() == AVAssetReaderStatus::Failed {
                return Err(match reader.reader.error() {
                    Some(e) => ns_error("AVAssetReaderOutput copyNextSampleBuffer", &e),
                    None => os("AVAssetReaderOutput copyNextSampleBuffer", "reading failed"),
                });
            }
        }
        Ok(())
    }

    /// The next decoded frame from the reader.
    fn read_video(&mut self) -> Result<Option<VideoFrame>, PlatformError> {
        loop {
            let Some(output) = self.reader.as_ref().and_then(|r| r.video.as_ref()) else {
                return Ok(None);
            };
            // SAFETY: a started reader's output, read from one thread.
            let Some(sample) = (unsafe { output.copyNextSampleBuffer() }) else {
                self.ended()?;
                return Ok(None);
            };
            if let Some(frame) = self.frame(&sample)? {
                return Ok(Some(frame));
            }
            // A sample without a picture (a marker): read on.
        }
    }

    fn frame(&self, sample: &CMSampleBuffer) -> Result<Option<VideoFrame>, PlatformError> {
        // SAFETY: a live sample buffer the reader handed over.
        let Some(image) = (unsafe { sample.image_buffer() }) else {
            return Ok(None);
        };
        // SAFETY: as above.
        let pts = unsafe { sample.presentation_time_stamp() };
        if !pts.flags.contains(CMTimeFlags::Valid) {
            return Err(os(
                "CMSampleBufferGetPresentationTimeStamp",
                "a decoded frame without a presentation time",
            ));
        }
        let pts = media_time_from_cm(pts.value, pts.timescale, self.video_tb)?;
        let frame = copy_image(&image, pts)?;
        if !self.settings.bgra_to_rgba {
            return Ok(Some(frame));
        }
        let info = VideoFrameInfo {
            format: PixelFormat::Rgba8,
            ..*frame.info()
        };
        let mut planes = frame.into_planes();
        for plane in &mut planes {
            bgra_to_rgba(plane.data_mut());
        }
        Ok(Some(VideoFrame::new(info, planes)?))
    }
}

fn add_output(
    reader: &AVAssetReader,
    output: &AVAssetReaderTrackOutput,
    what: &str,
) -> Result<(), PlatformError> {
    // SAFETY: a live reader that hasn't started, and an output made for one of its
    // asset's tracks. Not copying lets the reader hand over its own buffers.
    unsafe {
        output.setAlwaysCopiesSampleData(false);
        if !reader.canAddOutput(output) {
            return Err(PlatformError::Unsupported {
                what: format!("reading this file's {what} in the asked-for format"),
            });
        }
        reader.addOutput(output);
    }
    Ok(())
}

impl MediaDecoder for AvMediaDecoder {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn next_video(&mut self) -> Result<Option<VideoFrame>, PlatformError> {
        if let Some(frame) = self.peeked.take() {
            return Ok(Some(frame));
        }
        self.read_video()
    }

    fn next_audio(&mut self) -> Result<Option<AudioBuffer>, PlatformError> {
        let Some(&(_, format)) = self.audio.as_ref() else {
            return Ok(None);
        };
        loop {
            let Some(output) = self.reader.as_ref().and_then(|r| r.audio.as_ref()) else {
                return Ok(None);
            };
            // SAFETY: a started reader's output, read from one thread.
            let Some(sample) = (unsafe { output.copyNextSampleBuffer() }) else {
                self.ended()?;
                return Ok(None);
            };
            if let Some(block) = audio_block(&sample, format)? {
                return Ok(Some(block));
            }
        }
    }

    fn seek(&mut self, to: MediaTime) -> Result<(), PlatformError> {
        self.start(Some(to))
    }
}

/// One sample buffer of interleaved `f32` audio as a block; `None` for an empty one.
fn audio_block(
    sample: &CMSampleBuffer,
    format: AudioFormat,
) -> Result<Option<AudioBuffer>, PlatformError> {
    // SAFETY: a live sample buffer the reader handed over.
    let Some(data) = (unsafe { sample.data_buffer() }) else {
        return Ok(None);
    };
    // SAFETY: a live block buffer.
    let bytes = unsafe { data.data_length() };
    let frame_bytes = 4 * format.channels();
    let frames = bytes / frame_bytes;
    if frames == 0 {
        return Ok(None);
    }
    let mut samples = vec![0f32; frames * format.channels()];
    let len = samples.len() * 4;
    // SAFETY: `samples` has room for `len` bytes, which the block buffer holds from
    // offset 0; Core Media copies them out whether or not the buffer is contiguous.
    let status =
        unsafe { data.copy_data_bytes(0, len, NonNull::from(samples.as_mut_slice()).cast()) };
    if status != 0 {
        return Err(crate::fourcc::os_status(
            "CMBlockBufferCopyDataBytes",
            status,
            "reading decoded audio",
        ));
    }
    // SAFETY: as above.
    let pts = unsafe { sample.presentation_time_stamp() };
    if !pts.flags.contains(CMTimeFlags::Valid) {
        return Err(os(
            "CMSampleBufferGetPresentationTimeStamp",
            "decoded audio without a presentation time",
        ));
    }
    let pts = media_time_from_cm(pts.value, pts.timescale, format.timebase())?;
    Ok(Some(AudioBuffer::new(format, pts, samples)?))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jstream_platform::media::{MediaTime, PixelFormat, Rounding, SampleFormat, Timebase};
    use jstream_platform::{HardwarePreference, MediaDecoderConfig, MediaDecoders, PlatformError};

    use super::AvMediaDecoders;

    const RGBA: MediaDecoderConfig = MediaDecoderConfig {
        video_format: PixelFormat::Rgba8,
        hardware: HardwarePreference::Prefer,
        audio: false,
    };

    /// A sound every Mac has: an audio-only file AVFoundation plays.
    const SYSTEM_SOUND: &str = "/System/Library/Sounds/Ping.aiff";

    fn ms(t: MediaTime) -> i64 {
        t.rescale(Timebase::MILLIS, Rounding::Down)
            .expect("a time")
            .ticks()
    }

    #[test]
    fn a_system_sound_decodes_as_audio_only() {
        let config = MediaDecoderConfig {
            audio: true,
            ..RGBA
        };
        let mut file = AvMediaDecoders::new()
            .open(Path::new(SYSTEM_SOUND), &config)
            .expect("open");
        let info = *file.info();
        assert_eq!(info.video, None, "no video track");
        assert_eq!(file.next_video().expect("no error"), None);
        let format = info.audio.expect("an audio stream").format;
        assert_eq!(format.sample_format(), SampleFormat::F32Interleaved);
        assert!(format.channels() <= 2);
        let mut end = None;
        let mut frames = 0;
        while let Some(block) = file.next_audio().expect("decode") {
            assert_eq!(block.format(), format);
            assert!(block.samples().iter().all(|s| s.is_finite()));
            let pts = ms(block.pts());
            assert!(
                end.is_none_or(|end: i64| (pts - end).abs() <= 1),
                "{pts} ms after {end:?}"
            );
            end = Some(
                pts + i64::try_from(block.frames()).expect("frames") * 1000
                    / i64::from(format.sample_rate()),
            );
            frames += block.frames();
        }
        assert!(frames > 0, "some sound");
        // Back to the start: the sound plays again.
        file.seek(MediaTime::new(0, Timebase::MILLIS))
            .expect("seek");
        let block = file.next_audio().expect("decode").expect("a block");
        assert!(ms(block.pts()) <= 1);
        // Without asking for audio, a file of sound alone has nothing to play.
        assert!(matches!(
            AvMediaDecoders::new().open(Path::new(SYSTEM_SOUND), &RGBA),
            Err(PlatformError::Unsupported { .. })
        ));
    }

    #[test]
    fn media_file_errors_name_what_is_wrong() {
        assert!(matches!(
            AvMediaDecoders::new().open(Path::new("/no/such/folder/clip.mp4"), &RGBA),
            Err(PlatformError::NotFound { .. })
        ));
        let dir = std::env::temp_dir().join(format!("jstream-media-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp folder");
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, b"not a media file at all").expect("a file");
        let opened = AvMediaDecoders::new().open(&junk, &RGBA);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(
            opened,
            Err(PlatformError::Unsupported { .. } | PlatformError::Os { .. })
        ));
        let i420 = MediaDecoderConfig {
            video_format: PixelFormat::I420,
            ..RGBA
        };
        assert!(matches!(
            AvMediaDecoders::new().open(Path::new(SYSTEM_SOUND), &i420),
            Err(PlatformError::Unsupported { .. })
        ));
    }

    /// The video file `JSTREAM_TEST_MEDIA` names: any file with sound that QuickTime
    /// Player plays, over two seconds long.
    #[cfg(feature = "hw-tests")]
    fn clip() -> std::path::PathBuf {
        let path = std::env::var_os("JSTREAM_TEST_MEDIA")
            .expect("set JSTREAM_TEST_MEDIA to a video file the OS can play");
        std::path::PathBuf::from(path)
            .canonicalize()
            .expect("the video file")
    }

    #[cfg(feature = "hw-tests")]
    #[test]
    #[ignore = "needs a video file: JSTREAM_TEST_MEDIA=clip.mp4 cargo test --features hw-tests -- --ignored media_file"]
    fn media_file_decodes_frames_in_order_and_seeks() {
        for format in [PixelFormat::Rgba8, PixelFormat::Bgra8, PixelFormat::Nv12] {
            let config = MediaDecoderConfig {
                video_format: format,
                ..RGBA
            };
            let mut file = AvMediaDecoders::new().open(&clip(), &config).expect("open");
            let info = *file.info();
            let video = info.video.expect("a video stream");
            let duration = info.duration.expect("a duration");
            assert!(ms(duration) > 2_000, "use a clip over two seconds long");
            let mut last = None;
            for i in 0..30 {
                let frame = file.next_video().expect("decode").expect("a frame");
                assert_eq!(frame.info().size, video.size, "frame {i}");
                assert_eq!(frame.info().format, format);
                if format != PixelFormat::Nv12 {
                    let row = frame.row(0, video.size.height() / 2).expect("a row");
                    assert!(row.chunks_exact(4).all(|px| px[3] == 0xff), "opaque");
                }
                let pts = ms(frame.info().pts);
                assert!(last.is_none_or(|last| pts > last), "frame {i} at {pts} ms");
                last = Some(pts);
            }
            let middle = ms(duration) / 2;
            file.seek(MediaTime::new(middle, Timebase::MILLIS))
                .expect("seek");
            let frame = file.next_video().expect("decode").expect("a frame");
            let pts = ms(frame.info().pts);
            assert!(
                pts <= middle + 100 && pts + 10_000 >= middle,
                "{pts} ms for {middle}"
            );
            file.seek(MediaTime::new(ms(duration) + 5_000, Timebase::MILLIS))
                .expect("seek past the end");
            let mut extra = 0;
            while file.next_video().expect("decode").is_some() {
                extra += 1;
                assert!(extra < 600, "the stream ends");
            }
        }
    }

    #[cfg(feature = "hw-tests")]
    #[test]
    #[ignore = "needs a video file: JSTREAM_TEST_MEDIA=clip.mp4 cargo test --features hw-tests -- --ignored media_file"]
    fn media_file_decodes_audio_beside_video() {
        let config = MediaDecoderConfig {
            audio: true,
            ..RGBA
        };
        let mut file = AvMediaDecoders::new().open(&clip(), &config).expect("open");
        let format = file.info().audio.expect("an audio stream").format;
        let mut frames = 0;
        while frames < usize::try_from(format.sample_rate()).expect("a rate") {
            let block = file.next_audio().expect("decode").expect("a block");
            assert_eq!(block.format(), format);
            frames += block.frames();
            // Video interleaves with it, as the source reads both.
            let _ = file.next_video().expect("decode");
        }
        let mut silent = AvMediaDecoders::new().open(&clip(), &RGBA).expect("open");
        assert_eq!(silent.info().audio, None);
        assert_eq!(silent.next_audio().expect("no error"), None);
    }
}
