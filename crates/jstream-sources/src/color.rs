//! The `color` source: a solid colour of a given size.

use crate::describe::{kind, param, size};
use crate::source::{changed_non_live, setting};
use crate::{Context, Limits, Source, SourceError, SourceKind};
use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane, Timebase,
    VideoFrame, VideoFrameInfo,
};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::json;
use std::sync::Arc;

/// The `color` kind.
pub(crate) struct ColorKind(KindDescriptor);

impl ColorKind {
    pub(crate) fn new() -> Self {
        Self(KindDescriptor {
            params: vec![
                ParamDescriptor {
                    live: true,
                    ..param(
                        "color",
                        "Colour",
                        ParamType::Color { alpha: true },
                        json!("#000000ff"),
                    )
                },
                param("width", "Width", size(), json!(1920)),
                param("height", "Height", size(), json!(1080)),
            ],
            ..kind("color", "Colour", true, false)
        })
    }
}

impl SourceKind for ColorKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let size = frame_size(settings, &ctx.limits)?;
        let rgba = color(settings)?;
        Ok(Box::new(ColorSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            frame: Arc::new(fill(size, rgba)?),
        }))
    }
}

/// A running `color` source. Its one frame is made on create and on each update.
struct ColorSource {
    descriptor: KindDescriptor,
    settings: Settings,
    frame: Arc<VideoFrame>,
}

impl Source for ColorSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&self.frame))
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
        let rgba = color(settings)?;
        self.frame = Arc::new(fill(self.frame.info().size, rgba)?);
        self.settings = settings.clone();
        Ok(())
    }
}

/// The `width` and `height` settings, within the descriptor's range and `limits`.
fn frame_size(settings: &Settings, limits: &Limits) -> Result<FrameSize, SourceError> {
    let side = |id: &str| {
        let value = setting(settings, id)?;
        value
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|&v| (1..=limits.max_side).contains(&v))
            .ok_or_else(|| SourceError::Setting {
                param: id.to_owned(),
                reason: format!("must be a whole number from 1 to {}", limits.max_side),
            })
    };
    let (width, height) = (side("width")?, side("height")?);
    let bytes = u64::from(width) * u64::from(height) * 4;
    if bytes > limits.max_decoded_bytes {
        // Named after the larger side, the one a client would most likely shrink.
        return Err(SourceError::Setting {
            param: if width >= height { "width" } else { "height" }.into(),
            reason: format!(
                "{width}x{height} is over {} bytes",
                limits.max_decoded_bytes
            ),
        });
    }
    Ok(FrameSize::new(width, height)?)
}

/// The `color` setting as straight (not premultiplied) sRGB RGBA bytes.
fn color(settings: &Settings) -> Result<[u8; 4], SourceError> {
    let value = setting(settings, "color")?;
    value
        .as_str()
        .and_then(parse_color)
        .ok_or_else(|| SourceError::Setting {
            param: "color".into(),
            reason: "must be a colour as `#rrggbb` or `#rrggbbaa`".into(),
        })
}

/// `#rrggbb` (opaque) or `#rrggbbaa`, either case.
pub(crate) fn parse_color(s: &str) -> Option<[u8; 4]> {
    let hex = s.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    let alpha = if hex.len() == 8 { byte(6)? } else { 0xff };
    Some([byte(0)?, byte(2)?, byte(4)?, alpha])
}

/// A frame of `size` filled with `rgba`.
fn fill(size: FrameSize, rgba: [u8; 4]) -> Result<VideoFrame, SourceError> {
    let pixels =
        usize::try_from(u64::from(size.width()) * u64::from(size.height())).map_err(|_| {
            jstream_media::MediaError::Overflow {
                what: "a colour frame",
            }
        })?;
    Ok(rgba_frame(
        size,
        rgba.repeat(pixels),
        MediaTime::zero(Timebase::MILLIS),
    )?)
}

/// A packed RGBA8 frame in sRGB (full range; only BT.709's primaries apply to RGB).
pub(crate) fn rgba_frame(
    size: FrameSize,
    rgba: Vec<u8>,
    pts: MediaTime,
) -> Result<VideoFrame, jstream_media::MediaError> {
    let stride = size.width() as usize * 4;
    VideoFrame::new(
        VideoFrameInfo {
            size,
            format: PixelFormat::Rgba8,
            color: ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            pts,
        },
        vec![Plane::new(rgba, stride)],
    )
}

#[cfg(test)]
mod tests {
    use super::parse_color;

    #[test]
    fn colours_parse_with_and_without_alpha() {
        assert_eq!(parse_color("#ff8000"), Some([0xff, 0x80, 0x00, 0xff]));
        assert_eq!(parse_color("#A0b1C240"), Some([0xa0, 0xb1, 0xc2, 0x40]));
        for bad in [
            "ff8000",
            "#ff80",
            "#ff80001",
            "#gg8000",
            "#ff8000ff00",
            "#+f8000",
        ] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }
}
