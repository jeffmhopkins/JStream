//! The parts of the AVFoundation webcam backend that make no system call, so they are
//! tested on every platform: which formats a camera is listed with, the colour its
//! frames are labelled with, and which audio input belongs to it.

use jstream_platform::media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, PixelFormat, Timebase,
};
use jstream_platform::{CaptureEncoding, DeviceId, VideoDeviceFormat};

/// One of a camera's `AVCaptureDeviceFormat`s, as read: its dimensions and the shortest
/// frame duration of each of its frame-rate ranges (`minFrameDuration`, a `CMTime`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawFormat {
    /// `CMVideoFormatDescriptionGetDimensions`.
    pub(crate) width: i32,
    /// As above.
    pub(crate) height: i32,
    /// Each range's `minFrameDuration`, as (`value`, `timescale`).
    pub(crate) min_durations: Vec<(i64, i32)>,
}

/// A frame duration of `value / timescale` seconds as a frame period, if it is a valid
/// positive one.
pub(crate) fn frame_period(value: i64, timescale: i32) -> Option<Timebase> {
    let num = u32::try_from(value).ok()?;
    let den = u32::try_from(timescale).ok()?;
    Timebase::new(num, den).ok()
}

/// The formats a camera is listed with, in its own order, each once: one per format and
/// frame-rate range, at that range's highest rate.
///
/// AVFoundation converts every format to the pixel format the backend asks for, and
/// decodes MJPEG itself (ADR-0063 §10), so every listed format delivers NV12 as it is:
/// [`CaptureEncoding::Raw`]. Two formats differing only in their native encoding (a
/// camera's YUY2 and MJPEG at the same size and rate) are therefore one entry.
pub(crate) fn formats(raw: &[RawFormat]) -> Vec<VideoDeviceFormat> {
    let mut out: Vec<VideoDeviceFormat> = Vec::new();
    for f in raw {
        let (Ok(w), Ok(h)) = (u32::try_from(f.width), u32::try_from(f.height)) else {
            continue;
        };
        let Ok(size) = FrameSize::new(w, h) else {
            continue;
        };
        for &(value, timescale) in &f.min_durations {
            let Some(frame_period) = frame_period(value, timescale) else {
                continue;
            };
            let format = VideoDeviceFormat {
                size,
                frame_period,
                encoding: CaptureEncoding::Raw(PixelFormat::Nv12),
            };
            if !out.contains(&format) {
                out.push(format);
            }
        }
    }
    out
}

/// The index in `raw` of the first format that offers `format`, as [`formats`] lists it.
pub(crate) fn find(raw: &[RawFormat], format: &VideoDeviceFormat) -> Option<usize> {
    if format.encoding != CaptureEncoding::Raw(PixelFormat::Nv12) {
        return None;
    }
    raw.iter().position(|f| {
        u32::try_from(f.width).ok() == Some(format.size.width())
            && u32::try_from(f.height).ok() == Some(format.size.height())
            && f.min_durations
                .iter()
                .any(|&(v, t)| frame_period(v, t) == Some(format.frame_period))
    })
}

/// The colour of a camera's NV12 frames, which the backend asks for in video range:
/// BT.601 below 720 lines and BT.709 from there, the usual convention for cameras that
/// don't say, as on Windows.
pub(crate) fn color(height: u32) -> ColorInfo {
    ColorInfo {
        space: if height < 720 {
            ColorSpace::Bt601
        } else {
            ColorSpace::Bt709
        },
        range: ColorRange::Limited,
    }
}

/// The audio input that belongs to a camera with `model` (`AVCaptureDevice`'s `modelID`):
/// the one audio capture device with the same model, if exactly one has it. An audio
/// capture device's `uniqueID` is its Core Audio UID, so the ID is one `CoreAudioIo`
/// opens. A USB webcam's camera and microphone share a model ID; a Mac's built-in camera
/// and microphone don't, and stay unpaired.
pub(crate) fn paired(model: &str, audio: &[(String, String)]) -> Option<DeviceId> {
    if model.is_empty() {
        return None;
    }
    let mut same = audio.iter().filter(|(_, m)| m == model);
    match (same.next(), same.next()) {
        (Some((uid, _)), None) => Some(DeviceId::new(uid.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(width: i32, height: i32, min_durations: &[(i64, i32)]) -> RawFormat {
        RawFormat {
            width,
            height,
            min_durations: min_durations.to_vec(),
        }
    }

    #[test]
    fn frame_periods_are_in_lowest_terms() {
        assert_eq!(frame_period(1, 30), Timebase::new(1, 30).ok());
        assert_eq!(frame_period(1001, 30000), Timebase::new(1001, 30000).ok());
        assert_eq!(
            frame_period(1_000_000, 30_000_000),
            Timebase::new(1, 30).ok()
        );
        assert_eq!(frame_period(0, 30), None);
        assert_eq!(frame_period(-1, 30), None);
        assert_eq!(frame_period(1, 0), None);
    }

    #[test]
    fn formats_are_nv12_once_each_in_the_device_order() {
        let list = formats(&[
            raw(1920, 1080, &[(1, 30)]),
            raw(1280, 720, &[(1, 60), (1, 30)]),
            // The same size and rate in another native encoding: listed once.
            raw(1920, 1080, &[(1, 30)]),
            raw(0, 720, &[(1, 30)]),
            raw(640, 480, &[(0, 30)]),
        ]);
        let got: Vec<(u32, u32, u32)> = list
            .iter()
            .map(|f| (f.size.width(), f.size.height(), f.frame_period.den()))
            .collect();
        assert_eq!(got, [(1920, 1080, 30), (1280, 720, 60), (1280, 720, 30)]);
        assert!(
            list.iter()
                .all(|f| f.encoding == CaptureEncoding::Raw(PixelFormat::Nv12))
        );
    }

    #[test]
    fn a_listed_format_is_found_and_another_is_not() {
        let raws = [raw(1920, 1080, &[(1, 30)]), raw(1280, 720, &[(1, 60)])];
        let listed = formats(&raws);
        assert_eq!(find(&raws, &listed[1]), Some(1));
        let mjpeg = VideoDeviceFormat {
            encoding: CaptureEncoding::Mjpeg,
            ..listed[0]
        };
        assert_eq!(find(&raws, &mjpeg), None);
        let faster = VideoDeviceFormat {
            frame_period: Timebase::new(1, 120).unwrap(),
            ..listed[0]
        };
        assert_eq!(find(&raws, &faster), None);
    }

    #[test]
    fn colour_follows_the_height() {
        assert_eq!(color(480).space, ColorSpace::Bt601);
        assert_eq!(color(720).space, ColorSpace::Bt709);
        assert_eq!(color(1080).range, ColorRange::Limited);
    }

    #[test]
    fn audio_pairs_only_on_one_model_match() {
        let audio = vec![
            (
                "BuiltInMicrophoneDevice".to_owned(),
                "Built-in Microphone".to_owned(),
            ),
            (
                "AppleUSBAudioEngine:Logi:C920:1".to_owned(),
                "UVC Camera VendorID_1133".to_owned(),
            ),
            ("AppleUSBAudioEngine:Other:2".to_owned(), "Other".to_owned()),
            ("AppleUSBAudioEngine:Other:3".to_owned(), "Other".to_owned()),
        ];
        assert_eq!(
            paired("UVC Camera VendorID_1133", &audio),
            Some(DeviceId::new("AppleUSBAudioEngine:Logi:C920:1"))
        );
        assert_eq!(paired("FaceTime HD Camera", &audio), None);
        assert_eq!(paired("Other", &audio), None, "ambiguous");
        assert_eq!(paired("", &audio), None);
    }
}
