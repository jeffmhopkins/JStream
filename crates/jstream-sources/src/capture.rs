//! The `display`, `window` and `webcam` sources: what the platform captures.
//!
//! Their frames don't come from [`Source::frame`]. The platform's capture backend (for
//! `webcam`, its video-device backend) delivers them, on its own thread, to the engine's
//! render thread, as CPU frames or as textures shared with the compositor. A capture source only says what to capture
//! ([`Source::capture`]); the engine starts and reads the capture.
//!
//! A `webcam` source has audio too (ADR-0061: a video capture device comes with its
//! paired audio stream): the input the platform pairs with its device, a webcam's
//! microphone or a capture card's HDMI audio. The engine records it as it records an
//! audio input ([`webcam_device`] says which device's pair).

use crate::describe::{kind, param};
use crate::source::{changed_non_live, setting};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{MediaTime, VideoFrame};
use jstream_protocol::{
    CaptureFormatPart, CaptureTargetKind, KindDescriptor, ParamType, Settings, parse_resolution,
};
use serde_json::json;
use std::sync::Arc;

/// The longest `target` setting, in characters. A Windows target ID is under 300.
const MAX_TARGET: u32 = 1024;

/// The `webcam` kind's ID.
const WEBCAM: &str = "webcam";

/// The video device a source of `kind_id` with these settings captures, whose paired
/// audio input is the source's sound: `Some(Some(id))` for the device with that ID,
/// `Some(None)` for the first device the platform lists, and `None` when `kind_id` is
/// not `webcam`. Settings are complete, as the graph stores them; a missing or mistyped
/// `device` reads as its default, the first device.
pub fn webcam_device(kind_id: &str, settings: &Settings) -> Option<Option<String>> {
    if kind_id != WEBCAM {
        return None;
    }
    let device = settings
        .get("device")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    Some((!device.is_empty()).then(|| device.to_owned()))
}

/// What a capture source shows.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CaptureSpec {
    /// A whole display: the one whose capture-backend target ID is given, or the
    /// primary display when `None`.
    Display(Option<String>),
    /// One top-level window: the one whose capture-backend target ID is given, or
    /// nothing when `None`.
    Window(Option<String>),
    /// A video capture device, such as a webcam or a capture card: the one whose
    /// video-device ID is given, or the first one listed when `None`, opened in a format
    /// with what the [`DeviceFormatWant`] asks for.
    Device(Option<String>, DeviceFormatWant),
}

/// What a `webcam` source asks of the format its device opens in: each part `None`
/// leaves it to the engine.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceFormatWant {
    /// The frame size, width by height.
    pub size: Option<(u32, u32)>,
    /// Frames per second, in hundredths (`2997` for 29.97).
    pub fps_centi: Option<u32>,
    /// The pixel format, as `capture.targets` names it (`mjpeg`, `nv12`, …).
    pub pixel_format: Option<String>,
}

impl DeviceFormatWant {
    /// Whether it asks nothing.
    pub fn is_any(&self) -> bool {
        *self == Self::default()
    }
}

/// Frames per second in hundredths, rounded: how a frame rate is compared.
pub fn fps_centi(fps: f64) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to a non-negative value well inside u32"
    )]
    let centi = (fps.clamp(0.0, 1.0e7) * 100.0).round() as u32;
    centi
}

/// Which of the capture kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Display,
    Window,
    Device,
}

/// The `display`, `window` or `webcam` kind.
pub(crate) struct CaptureKind {
    descriptor: KindDescriptor,
    flavor: Flavor,
}

impl CaptureKind {
    /// The `display` kind. An empty `target` captures the primary display, which is
    /// what a `display` source showed before it had settings.
    pub(crate) fn display() -> Self {
        Self::new("display", "Display capture", Flavor::Display)
    }

    /// The `window` kind. An empty `target` captures nothing.
    pub(crate) fn window() -> Self {
        Self::new("window", "Window capture", Flavor::Window)
    }

    /// The `webcam` kind: a webcam or capture card. An empty `device` captures the
    /// first device the platform lists.
    pub(crate) fn webcam() -> Self {
        Self::new(WEBCAM, "Webcam", Flavor::Device)
    }

    fn new(id: &str, name: &str, flavor: Flavor) -> Self {
        let (param_id, label) = match flavor {
            Flavor::Display => ("target", "Display"),
            Flavor::Window => ("target", "Window"),
            Flavor::Device => ("device", "Device"),
        };
        // A display or window names a `capture.targets` entry, so a UI can list them.
        let ty = match flavor {
            Flavor::Display => ParamType::CaptureTarget {
                kind: CaptureTargetKind::Display,
                max_len: MAX_TARGET,
            },
            Flavor::Window => ParamType::CaptureTarget {
                kind: CaptureTargetKind::Window,
                max_len: MAX_TARGET,
            },
            // Since protocol 0.89 a device is one too; before, it was a plain string.
            Flavor::Device => ParamType::CaptureTarget {
                kind: CaptureTargetKind::Device,
                max_len: MAX_TARGET,
            },
        };
        let mut target = param(param_id, label, ty, json!(""));
        target.description = Some(match flavor {
            Flavor::Window => "The ID of the window to capture, as `capture.targets` lists \
                               it. Empty captures nothing."
                .into(),
            Flavor::Display => "The ID of the display to capture, as `capture.targets` lists \
                                it. Empty captures the primary display."
                .into(),
            Flavor::Device => "The ID of the webcam or capture card, as `capture.targets` \
                               lists it. Empty captures the first device listed."
                .into(),
        });
        let mut params = vec![target];
        if flavor == Flavor::Device {
            params.extend(format_params(param_id));
        }
        // A webcam has no pointer to show; a display or window capture may leave it out.
        if flavor != Flavor::Device {
            let mut show_cursor = param("show_cursor", "Show cursor", ParamType::Bool, json!(true));
            show_cursor.description = Some("Whether the pointer shows in the capture.".into());
            params.push(show_cursor);
        }
        Self {
            descriptor: KindDescriptor {
                params,
                // A webcam's paired audio is its sound; a display or window has none.
                ..kind(id, name, true, flavor == Flavor::Device)
            },
            flavor,
        }
    }

    /// What the settings say to capture.
    fn spec(&self, settings: &Settings) -> Result<CaptureSpec, SourceError> {
        let target = self.target(settings)?;
        Ok(match self.flavor {
            Flavor::Display => CaptureSpec::Display(target),
            Flavor::Window => CaptureSpec::Window(target),
            Flavor::Device => CaptureSpec::Device(target, format_want(settings)?),
        })
    }

    /// The target or device the settings name, `None` for the default.
    fn target(&self, settings: &Settings) -> Result<Option<String>, SourceError> {
        let param = &self.descriptor.params[0].id;
        let value = setting(settings, param)?;
        let target = value.as_str().ok_or_else(|| SourceError::Setting {
            param: param.clone(),
            reason: "must be a string".into(),
        })?;
        Ok((!target.is_empty()).then(|| target.to_owned()))
    }
}

/// A `webcam`'s format parameters: its resolution, frame rate and pixel format, each
/// picked from what `capture.targets` lists for the device that `target` names.
fn format_params(target: &str) -> Vec<jstream_protocol::ParamDescriptor> {
    let part = |id: &str, label: &str, part, default, description: &str| {
        let mut p = param(
            id,
            label,
            ParamType::CaptureFormat {
                part,
                target: target.into(),
            },
            default,
        );
        p.description = Some(description.into());
        p.group = Some("Format".into());
        p
    };
    vec![
        part(
            "resolution",
            "Resolution",
            CaptureFormatPart::Resolution,
            json!(""),
            "The frame size to capture, as `<width>x<height>` (`1920x1080`), one the \
             device lists in `capture.targets`. Empty lets the engine choose: the largest \
             up to 1920x1080.",
        ),
        part(
            "fps",
            "Frame rate",
            CaptureFormatPart::Fps,
            json!(0),
            "The frames per second to capture, one the device lists for the resolution. \
             0 lets the engine choose: the fastest.",
        ),
        part(
            "pixel_format",
            "Video format",
            CaptureFormatPart::PixelFormat,
            json!(""),
            "How the device sends its frames: `mjpeg` (compressed, decoded by the \
             engine), `yuy2`, `nv12`, `i420`, `bgra` or `rgba`. Empty lets the engine \
             choose: uncompressed when it can.",
        ),
    ]
}

/// What a `webcam`'s format settings ask. A device that offers no format matching them
/// opens in the engine's choice, so they never stop a capture.
fn format_want(settings: &Settings) -> Result<DeviceFormatWant, SourceError> {
    let bad = |param: &str, reason: &str| SourceError::Setting {
        param: param.into(),
        reason: reason.into(),
    };
    let size = setting(settings, "resolution")?
        .as_str()
        .and_then(parse_resolution)
        .ok_or_else(|| bad("resolution", "must be `<width>x<height>` or empty"))?;
    let fps = setting(settings, "fps")?
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| bad("fps", "must be a number of at least 0"))?;
    let pixel_format = setting(settings, "pixel_format")?
        .as_str()
        .ok_or_else(|| bad("pixel_format", "must be a string"))?
        .trim();
    Ok(DeviceFormatWant {
        size,
        fps_centi: (fps > 0.0).then(|| fps_centi(fps)),
        pixel_format: (!pixel_format.is_empty()).then(|| pixel_format.to_owned()),
    })
}

impl SourceKind for CaptureKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.descriptor
    }

    fn create(&self, settings: &Settings, _: &Context) -> Result<Box<dyn Source>, SourceError> {
        let show_cursor = match self.flavor {
            Flavor::Device => true,
            Flavor::Display | Flavor::Window => setting(settings, "show_cursor")?
                .as_bool()
                .ok_or_else(|| SourceError::Setting {
                    param: "show_cursor".into(),
                    reason: "must be a boolean".into(),
                })?,
        };
        Ok(Box::new(CaptureSource {
            kind_id: self.descriptor.kind_id.clone(),
            descriptor: self.descriptor.clone(),
            settings: settings.clone(),
            spec: self.spec(settings)?,
            show_cursor,
        }))
    }
}

/// A running `display`, `window` or `webcam` source.
struct CaptureSource {
    kind_id: String,
    descriptor: KindDescriptor,
    settings: Settings,
    spec: CaptureSpec,
    show_cursor: bool,
}

impl Source for CaptureSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Err(SourceError::Captured {
            kind_id: self.kind_id.clone(),
        })
    }

    fn next_change(&self, _: MediaTime) -> Option<MediaTime> {
        None
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        self.settings = settings.clone();
        Ok(())
    }

    fn capture(&self) -> Option<&CaptureSpec> {
        Some(&self.spec)
    }

    fn show_cursor(&self) -> bool {
        self.show_cursor
    }
}
