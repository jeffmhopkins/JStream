//! The Media Foundation Source Reader on a local file: it demuxes, and decodes video
//! through the OS decoders, converting to RGB32 with its video processor; this module
//! hands the frames on as packed RGBA or BGRA. Audio, when asked for, comes out as
//! interleaved `f32` at the stream's own rate, mixed down to stereo by the decoder when
//! its channel count has no layout here, and cut to its content without the encoder's
//! priming and padding ([`gapless`](super::gapless)).

use std::path::Path;

use jstream_platform::media::{
    AudioBuffer, AudioFormat, ChannelLayout, ColorInfo, ColorRange, ColorSpace, FrameSize,
    MediaTime, PixelFormat, Plane, Rounding, SampleFormat, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    HardwarePreference, MediaAudioInfo, MediaDecoder, MediaDecoderConfig, MediaDecoders, MediaInfo,
    MediaVideoInfo, PlatformError,
};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFMediaBuffer, IMFMediaType, IMFSample, IMFSourceReader,
    MF_E_INVALID_POSITION, MF_E_INVALIDMEDIATYPE, MF_E_INVALIDSTREAMNUMBER,
    MF_E_TOPO_CODEC_NOT_FOUND, MF_E_UNSUPPORTED_BYTESTREAM_TYPE, MF_E_UNSUPPORTED_FORMAT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_DEFAULT_STRIDE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE,
    MF_MT_SUBTYPE, MF_PD_DURATION, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SD_AUDIO_ENCODER_DELAY, MF_SD_AUDIO_ENCODER_PADDING, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
    MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READERF_ERROR, MFAudioFormat_AAC, MFAudioFormat_Float, MFCreateAttributes,
    MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio, MFMediaType_Video,
    MFVideoArea, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, HRESULT, HSTRING, Interface};

use super::convert::{self, Area};
use super::gapless::{AAC_PRIMING, Block, Gapless, frames_at};
use crate::com::{Com, OsResult, os_error};
use crate::video_device::source::MfStarted;

/// What `NotFound` names.
const KIND: &str = "media file";

/// The reader's stream indexes, as the `u32`s its methods take.
const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
const ALL: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
const SOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;

/// Local media files through the Media Foundation Source Reader: the [`MediaDecoders`]
/// backend on Windows. It plays what Media Foundation does: MP4, MOV, MKV and WebM among
/// containers, and H.264, HEVC, VP9 and AV1 where the OS has the decoder (the last
/// three come from Microsoft Store extensions).
///
/// Creating it starts nothing; each open file keeps Media Foundation started while it is
/// open.
#[derive(Clone, Copy, Debug, Default)]
pub struct MfMediaDecoders;

impl MfMediaDecoders {
    /// The backend.
    pub fn new() -> Self {
        Self
    }
}

impl MediaDecoders for MfMediaDecoders {
    fn open(
        &self,
        path: &Path,
        config: &MediaDecoderConfig,
    ) -> Result<Box<dyn MediaDecoder>, PlatformError> {
        if !matches!(config.video_format, PixelFormat::Rgba8 | PixelFormat::Bgra8) {
            return Err(PlatformError::Unsupported {
                what: format!("decoding media files to {:?}", config.video_format),
            });
        }
        if config.hardware == HardwarePreference::Require {
            // The reader picks its decoder itself, and doesn't say which.
            return Err(PlatformError::Unsupported {
                what: "requiring a hardware decoder for media files".into(),
            });
        }
        // This thread joins the MTA for the calls below. The objects they make are
        // free-threaded, and the MTA outlives them (`MfStarted` holds it).
        let _com = Com::init()?;
        let mf = MfStarted::start()?;
        let reader = create_reader(path, config.hardware)?;
        let shape = select_video(&reader)?;
        let video = shape.map(|shape| MediaVideoInfo {
            size: shape.size,
            frame_period: frame_period(&reader),
        });
        let audio = if config.audio {
            select_audio(&reader)?
        } else {
            None
        };
        let info = MediaInfo {
            video,
            audio: audio.map(|format| MediaAudioInfo { format }),
            duration: duration(&reader),
        };
        let gapless = audio.map(|format| gapless(&reader, format, info.duration));
        Ok(Box::new(MfMediaDecoder {
            reader,
            info,
            shape,
            format: config.video_format,
            at_end: false,
            gapless,
            audio_ended: false,
            _mf: mf,
        }))
    }
}

fn attributes(count: u32) -> Result<IMFAttributes, PlatformError> {
    let mut attrs = None;
    // SAFETY: `attrs` is a valid out-pointer.
    unsafe { MFCreateAttributes(&mut attrs, count) }.os("MFCreateAttributes")?;
    attrs.ok_or_else(|| PlatformError::Os {
        call: "MFCreateAttributes",
        code: None,
        message: "succeeded without attributes".into(),
    })
}

/// A reader on the file at `path` that may convert video (YUV to RGB32, deinterlacing),
/// using hardware decoders unless told to avoid them.
fn create_reader(
    path: &Path,
    hardware: HardwarePreference,
) -> Result<IMFSourceReader, PlatformError> {
    let attrs = attributes(2)?;
    // SAFETY: plain COM calls on live attributes.
    unsafe {
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
            .os("IMFAttributes::SetUINT32")?;
        if hardware == HardwarePreference::Prefer {
            attrs
                .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                .os("IMFAttributes::SetUINT32")?;
        }
    }
    let url = match path.to_str() {
        Some(path) => HSTRING::from(convert::url_path(path)),
        None => HSTRING::from(path),
    };
    // SAFETY: `url` is a live string for the duration of the call.
    unsafe { MFCreateSourceReaderFromURL(&url, &attrs) }.map_err(|e| open_error(path, &e))
}

/// Maps an error from opening the file: a missing file is `NotFound`, a format Media
/// Foundation can't read is `Unsupported`.
fn open_error(path: &Path, e: &windows::core::Error) -> PlatformError {
    let code = e.code();
    if code == ERROR_FILE_NOT_FOUND.to_hresult() || code == ERROR_PATH_NOT_FOUND.to_hresult() {
        PlatformError::NotFound {
            kind: KIND,
            id: path.display().to_string(),
        }
    } else if code == MF_E_UNSUPPORTED_BYTESTREAM_TYPE
        || code == MF_E_UNSUPPORTED_FORMAT
        || code == MF_E_INVALIDMEDIATYPE
    {
        PlatformError::Unsupported {
            what: format!("this file's format ({})", e.message()),
        }
    } else {
        os_error("MFCreateSourceReaderFromURL", e)
    }
}

/// The decoded picture: its whole size, its row pitch in the buffer's contiguous form,
/// and the area of it that is shown.
#[derive(Clone, Copy, Debug)]
struct Shape {
    width: u32,
    rows: u32,
    /// `MF_MT_DEFAULT_STRIDE`: negative for a bottom-up picture.
    stride: i64,
    area: Area,
    /// The shown area's size.
    size: FrameSize,
}

/// Selects the first video stream and nothing else, decoded to RGB32, and reads its
/// shape. `None` for a file with no video stream.
fn select_video(reader: &IMFSourceReader) -> Result<Option<Shape>, PlatformError> {
    // SAFETY: plain COM calls on a live reader.
    unsafe { reader.SetStreamSelection(ALL, false) }.os("IMFSourceReader::SetStreamSelection")?;
    // SAFETY: as above.
    match unsafe { reader.SetStreamSelection(VIDEO, true) } {
        Err(e) if e.code() == MF_E_INVALIDSTREAMNUMBER => return Ok(None),
        result => result.os("IMFSourceReader::SetStreamSelection")?,
    }
    // SAFETY: plain call.
    let wanted: IMFMediaType = unsafe { MFCreateMediaType() }.os("MFCreateMediaType")?;
    // SAFETY: plain COM calls on live objects.
    unsafe {
        wanted
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .os("IMFMediaType::SetGUID")?;
        wanted
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .os("IMFMediaType::SetGUID")?;
    }
    // SAFETY: plain COM call on live objects.
    unsafe { reader.SetCurrentMediaType(VIDEO, None, &wanted) }.map_err(|e| {
        let code = e.code();
        if code == MF_E_INVALIDMEDIATYPE || code == MF_E_TOPO_CODEC_NOT_FOUND {
            PlatformError::Unsupported {
                what: format!("decoding this file's video ({})", e.message()),
            }
        } else {
            os_error("IMFSourceReader::SetCurrentMediaType", &e)
        }
    })?;
    shape(reader).map(Some)
}

/// The shape of the video the reader delivers now.
fn shape(reader: &IMFSourceReader) -> Result<Shape, PlatformError> {
    // SAFETY: plain COM call on a live reader.
    let t =
        unsafe { reader.GetCurrentMediaType(VIDEO) }.os("IMFSourceReader::GetCurrentMediaType")?;
    // SAFETY: plain attribute read on a live media type.
    let packed = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }.os("IMFMediaType::GetUINT64")?;
    let (width, rows) = convert::unpack(packed);
    // SAFETY: as above.
    let stride = unsafe { t.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
        .ok()
        .map_or(i64::from(width) * 4, |s| i64::from(s.cast_signed()));
    let area = aperture(&t, width, rows).unwrap_or(Area::whole(width, rows));
    Ok(Shape {
        width,
        rows,
        stride,
        area,
        size: FrameSize::new(area.width, area.height)?,
    })
}

/// The type's minimum display aperture, when it has one that fits: a decoder that pads
/// 1080 rows to 1088 shows 1080 of them.
fn aperture(t: &IMFMediaType, width: u32, rows: u32) -> Option<Area> {
    let mut area = MFVideoArea::default();
    // SAFETY: the slice covers exactly `area`, a plain C struct any bytes may fill.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut((&raw mut area).cast::<u8>(), size_of::<MFVideoArea>())
    };
    // SAFETY: plain attribute read into a buffer of the blob's documented size.
    unsafe { t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None) }.ok()?;
    Area::aperture(
        (area.OffsetX.value, area.OffsetY.value),
        (area.Area.cx, area.Area.cy),
        width,
        rows,
    )
}

/// The video's nominal frame period, from its native type.
fn frame_period(reader: &IMFSourceReader) -> Option<jstream_platform::media::Timebase> {
    // SAFETY: plain COM calls on a live reader and the type it returns.
    let rate = unsafe {
        reader
            .GetNativeMediaType(VIDEO, 0)
            .and_then(|t| t.GetUINT64(&MF_MT_FRAME_RATE))
    };
    rate.ok().and_then(convert::frame_period)
}

/// Selects the first audio stream too, decoded to interleaved `f32`, and returns its
/// format. `None`, with the stream left unselected, for a file with no audio stream or
/// one the OS can't select or decode to a layout the engine mixes: the video still
/// plays. Only failing to unselect it again is an error, as the reader would then hold
/// samples nobody reads.
fn select_audio(reader: &IMFSourceReader) -> Result<Option<AudioFormat>, PlatformError> {
    // SAFETY: plain COM call on a live reader.
    if unsafe { reader.SetStreamSelection(AUDIO, true) }.is_err() {
        return Ok(None);
    }
    // As the decoder gives it, then mixed down to stereo when that has no layout here.
    for channels in [None, Some(2)] {
        if set_float(reader, channels).is_ok()
            && let Ok(Some(format)) = audio_format(reader)
        {
            return Ok(Some(format));
        }
    }
    // SAFETY: as above.
    unsafe { reader.SetStreamSelection(AUDIO, false) }.os("IMFSourceReader::SetStreamSelection")?;
    Ok(None)
}

/// Asks the reader for float PCM on the audio stream, with `channels` when set.
fn set_float(reader: &IMFSourceReader, channels: Option<u32>) -> windows::core::Result<()> {
    // SAFETY: plain calls on live objects.
    unsafe {
        let wanted: IMFMediaType = MFCreateMediaType()?;
        wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        wanted.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        if let Some(channels) = channels {
            wanted.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
        }
        reader.SetCurrentMediaType(AUDIO, None, &wanted)
    }
}

/// The audio format the reader delivers now, or `None` when its channel count has no
/// layout here.
fn audio_format(reader: &IMFSourceReader) -> Result<Option<AudioFormat>, PlatformError> {
    // SAFETY: plain COM calls on a live reader and the type it returns.
    let (rate, channels) = unsafe {
        let t = reader
            .GetCurrentMediaType(AUDIO)
            .os("IMFSourceReader::GetCurrentMediaType")?;
        (
            t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
                .os("IMFMediaType::GetUINT32")?,
            t.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
                .os("IMFMediaType::GetUINT32")?,
        )
    };
    let layout = match channels {
        1 => ChannelLayout::Mono,
        2 => ChannelLayout::Stereo,
        6 => ChannelLayout::Surround51,
        _ => return Ok(None),
    };
    Ok(Some(AudioFormat::new(
        rate,
        layout,
        SampleFormat::F32Interleaved,
    )?))
}

/// What to cut from the audio stream: the priming and padding the source reports
/// (`MF_SD_AUDIO_ENCODER_DELAY` and `_PADDING`, from an MP3's LAME header or an MP4's
/// `iTunSMPB`), else for AAC the usual 1024 frames of priming, and whatever runs past
/// the file's duration (`MF_PD_DURATION`, which an MP4's edit list sets to the content's
/// length). The reader applies none of these itself (#445).
fn gapless(reader: &IMFSourceReader, format: AudioFormat, duration: Option<MediaTime>) -> Gapless {
    let attribute = |key: &GUID| {
        // SAFETY: plain COM call on a live reader; the PROPVARIANT is cleared by its Drop.
        let value = unsafe { reader.GetPresentationAttribute(AUDIO, key) }.ok()?;
        u32::try_from(&value).ok()
    };
    // SAFETY: plain COM calls on a live reader and the type it returns.
    let aac = unsafe {
        reader
            .GetNativeMediaType(AUDIO, 0)
            .and_then(|t| t.GetGUID(&MF_MT_SUBTYPE))
    }
    .is_ok_and(|subtype| subtype == MFAudioFormat_AAC);
    let delay = attribute(&MF_SD_AUDIO_ENCODER_DELAY)
        .or(aac.then_some(AAC_PRIMING))
        .unwrap_or(0);
    let padding = attribute(&MF_SD_AUDIO_ENCODER_PADDING).unwrap_or(0);
    let end = duration
        .and_then(|d| d.rescale(format.timebase(), Rounding::Nearest).ok())
        .map(MediaTime::ticks);
    Gapless::new(format.channels(), delay, padding, end)
}

/// How long the file plays, when the container says.
fn duration(reader: &IMFSourceReader) -> Option<MediaTime> {
    // SAFETY: plain COM call on a live reader; the PROPVARIANT is cleared by its Drop.
    let value = unsafe { reader.GetPresentationAttribute(SOURCE, &MF_PD_DURATION) }.ok()?;
    let hns = u64::try_from(&value).ok()?;
    i64::try_from(hns)
        .ok()
        .filter(|&t| t > 0)
        .map(convert::from_hns)
}

/// One open file.
struct MfMediaDecoder {
    reader: IMFSourceReader,
    info: MediaInfo,
    /// The video's shape; `None` for a file with no video.
    shape: Option<Shape>,
    format: PixelFormat,
    /// Seeked past the end: both streams read as ended until the next seek.
    at_end: bool,
    /// Cuts the audio to its content; `None` with no audio.
    gapless: Option<Gapless>,
    /// The audio stream ended, and its last block was handed on: it reads as ended
    /// until the next seek.
    audio_ended: bool,
    /// Declared last, so the reader is released before Media Foundation shuts down.
    _mf: MfStarted,
}

// SAFETY: the Source Reader was created in the MTA and is free-threaded, and the MTA
// lives as long as `_mf`, so any thread may use it as an implicit MTA member. The decoder
// is `Send` but not `Sync`: one thread at a time drives it, as the trait requires.
unsafe impl Send for MfMediaDecoder {}

impl MediaDecoder for MfMediaDecoder {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn next_video(&mut self) -> Result<Option<VideoFrame>, PlatformError> {
        if self.shape.is_none() || self.at_end {
            return Ok(None);
        }
        loop {
            let mut flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample: Option<IMFSample> = None;
            // SAFETY: valid out-pointers for the duration of the call.
            unsafe {
                self.reader.ReadSample(
                    VIDEO,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
            }
            .os("IMFSourceReader::ReadSample")?;
            if flags & MF_SOURCE_READERF_ERROR.0.cast_unsigned() != 0 {
                return Err(PlatformError::Os {
                    call: "IMFSourceReader::ReadSample",
                    code: None,
                    message: "the video stream failed".into(),
                });
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0.cast_unsigned() != 0 {
                let shape = shape(&self.reader)?;
                self.shape = Some(shape);
                if let Some(video) = &mut self.info.video {
                    video.size = shape.size;
                }
            }
            if let Some(sample) = sample {
                return self.frame(&sample, timestamp).map(Some);
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                return Ok(None);
            }
            // No sample and no end: a gap in the stream. Read on.
        }
    }

    fn next_audio(&mut self) -> Result<Option<AudioBuffer>, PlatformError> {
        let Some(audio) = self.info.audio else {
            return Ok(None);
        };
        if self.at_end || self.audio_ended {
            return Ok(None);
        }
        loop {
            let mut flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample: Option<IMFSample> = None;
            // SAFETY: valid out-pointers for the duration of the call.
            unsafe {
                self.reader.ReadSample(
                    AUDIO,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
            }
            .os("IMFSourceReader::ReadSample")?;
            if flags & MF_SOURCE_READERF_ERROR.0.cast_unsigned() != 0 {
                return Err(PlatformError::Os {
                    call: "IMFSourceReader::ReadSample",
                    code: None,
                    message: "the audio stream failed".into(),
                });
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0.cast_unsigned() != 0
                && audio_format(&self.reader)? != Some(audio.format)
            {
                // The engine's mixer input was made for the first format.
                return Err(PlatformError::Unsupported {
                    what: "an audio format change partway through".into(),
                });
            }
            let gapless = self.gapless.as_mut().ok_or(PlatformError::InvalidState {
                what: "reading audio",
                state: "the file has no audio",
            })?;
            if let Some(sample) = sample {
                let block = samples(&sample, audio.format, timestamp)?;
                // Priming, or the block held back to see whether it is the last.
                match gapless.push(block) {
                    Some(block) => return buffer(audio.format, block).map(Some),
                    None => continue,
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                self.audio_ended = true;
                return gapless
                    .finish()
                    .map(|block| buffer(audio.format, block))
                    .transpose();
            }
            // No sample and no end: a gap in the stream. Read on.
        }
    }

    fn seek(&mut self, to: MediaTime) -> Result<(), PlatformError> {
        let hns = convert::to_hns(to)
            .ok_or_else(|| PlatformError::Unsupported {
                what: format!("seeking to {to}"),
            })?
            .max(0);
        let position = PROPVARIANT::from(hns);
        if let Some(gapless) = &mut self.gapless {
            gapless.reset();
        }
        self.audio_ended = false;
        // SAFETY: plain COM call; GUID_NULL is 100 ns units, and `position` is a VT_I8
        // that lives for the duration of the call.
        match unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &position) } {
            Err(e) if past_end(e.code()) => self.at_end = true,
            result => {
                result.os("IMFSourceReader::SetCurrentPosition")?;
                self.at_end = false;
            }
        }
        Ok(())
    }
}

/// Whether `SetCurrentPosition` refused the time as past the end of the file, which
/// the trait treats as the end of the stream rather than an error.
fn past_end(code: HRESULT) -> bool {
    code == MF_E_INVALID_POSITION
}

impl MfMediaDecoder {
    /// A decoded RGB32 sample as a frame in the configured format.
    fn frame(&self, sample: &IMFSample, timestamp: i64) -> Result<VideoFrame, PlatformError> {
        let shape = self.shape.ok_or(PlatformError::InvalidState {
            what: "reading video",
            state: "the file has no video",
        })?;
        // SAFETY: plain COM call on a live sample.
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .os("IMFSample::ConvertToContiguousBuffer")?;
        let pixels = with_picture(&buffer, shape, |bytes, pitch| {
            convert::rgb32(bytes, pitch, shape.rows, shape.area, self.format)
        })?
        .ok_or_else(|| PlatformError::Os {
            call: "IMFMediaBuffer::Lock",
            code: None,
            message: "a decoded frame was smaller than its media type says".into(),
        })?;
        let info = VideoFrameInfo {
            size: shape.size,
            format: self.format,
            color: ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            pts: convert::from_hns(timestamp),
        };
        let row = shape.size.width() as usize * 4;
        Ok(VideoFrame::new(info, vec![Plane::new(pixels, row)])?)
    }
}

/// A decoded float PCM sample as a block of `format`, starting at `timestamp` (100 ns
/// units), which is counted in frames at the stream's rate.
fn samples(
    sample: &IMFSample,
    format: AudioFormat,
    timestamp: i64,
) -> Result<Block, PlatformError> {
    // SAFETY: plain COM call on a live sample.
    let buffer =
        unsafe { sample.ConvertToContiguousBuffer() }.os("IMFSample::ConvertToContiguousBuffer")?;
    /// A locked buffer, unlocked on drop.
    struct Locked(IMFMediaBuffer);
    impl Drop for Locked {
        fn drop(&mut self) {
            // SAFETY: balances the successful Lock below, once.
            let _ = unsafe { self.0.Unlock() };
        }
    }
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: valid out-pointers.
    unsafe { buffer.Lock(&mut ptr, None, Some(&mut len)) }.os("IMFMediaBuffer::Lock")?;
    let locked = Locked(buffer);
    let bytes: &[u8] = if ptr.is_null() {
        &[]
    } else {
        // SAFETY: the locked pointer is valid for `len` bytes until Unlock, which runs
        // when `locked` drops, after the last use of `bytes`.
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }
    };
    // Whole frames only: a decoder never splits one, but a short buffer can't be trusted.
    let frame = format.channels() * 4;
    let whole = bytes.len() / frame * frame;
    let pcm: Vec<f32> = bytes[..whole]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    drop(locked);
    Ok(Block {
        start: frames_at(timestamp, format.sample_rate()),
        samples: pcm,
    })
}

/// A block cut to the content, as the stream's next buffer: its time is its first
/// frame's, at the stream's rate.
fn buffer(format: AudioFormat, block: Block) -> Result<AudioBuffer, PlatformError> {
    let pts = MediaTime::new(block.start, format.timebase());
    Ok(AudioBuffer::new(format, pts, block.samples)?)
}

/// Calls `f` with a buffer's picture, locked for the duration of the call: its bytes
/// from the lowest address and the pitch from one row to the next one down.
fn with_picture<R>(
    buffer: &IMFMediaBuffer,
    shape: Shape,
    f: impl FnOnce(&[u8], i64) -> R,
) -> Result<R, PlatformError> {
    if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
        /// A locked 2D buffer, unlocked on drop.
        struct Locked(IMF2DBuffer);
        impl Drop for Locked {
            fn drop(&mut self) {
                // SAFETY: balances the successful Lock2D below, once.
                let _ = unsafe { self.0.Unlock2D() };
            }
        }
        let mut scan0 = std::ptr::null_mut();
        let mut pitch = 0i32;
        // SAFETY: valid out-pointers.
        unsafe { two_d.Lock2D(&mut scan0, &mut pitch) }.os("IMF2DBuffer::Lock2D")?;
        let locked = Locked(two_d);
        let step = pitch.unsigned_abs() as usize;
        let last_row = shape.rows.saturating_sub(1) as usize * step;
        let len = last_row + shape.width as usize * 4;
        let bytes: &[u8] = if scan0.is_null() || shape.rows == 0 {
            &[]
        } else {
            // SAFETY: Lock2D returned the top row's address and the pitch between rows.
            // The picture's `rows` rows of `width * 4` bytes each lie from the lowest
            // row's start (the top one, or the bottom one for a negative pitch) for
            // `len` bytes, all inside the locked buffer until Unlock2D, which runs when
            // `locked` drops, after the last use of `bytes`.
            unsafe {
                let lowest = if pitch < 0 {
                    scan0.sub(last_row)
                } else {
                    scan0
                };
                std::slice::from_raw_parts(lowest, len)
            }
        };
        let out = f(bytes, i64::from(pitch));
        drop(locked);
        return Ok(out);
    }
    /// A locked buffer, unlocked on drop.
    struct Locked(IMFMediaBuffer);
    impl Drop for Locked {
        fn drop(&mut self) {
            // SAFETY: balances the successful Lock below, once.
            let _ = unsafe { self.0.Unlock() };
        }
    }
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: valid out-pointers.
    unsafe { buffer.Lock(&mut ptr, None, Some(&mut len)) }.os("IMFMediaBuffer::Lock")?;
    let locked = Locked(buffer.clone());
    let bytes: &[u8] = if ptr.is_null() {
        &[]
    } else {
        // SAFETY: the locked pointer is valid for `len` bytes until Unlock, which runs
        // when `locked` drops, after the last use of `bytes`.
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }
    };
    // A contiguous buffer is laid out by the type's default stride.
    let out = f(bytes, shape.stride);
    drop(locked);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seek_past_the_end_is_the_end_of_the_stream() {
        // What `SetCurrentPosition` returns for a time beyond the duration (#421).
        assert!(past_end(HRESULT(0xC00D_36E5_u32.cast_signed())));
        assert!(!past_end(MF_E_INVALIDMEDIATYPE));
        assert!(!past_end(HRESULT(0)));
    }
}
