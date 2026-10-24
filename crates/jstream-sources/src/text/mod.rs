//! The `text` source: a string drawn with a TrueType or OpenType font.

pub(crate) mod raster;

use crate::color::{parse_color, rgba_frame};
use crate::describe::{asset, kind, param};
use crate::source::{self, changed_non_live, setting};
use crate::{Context, Limits, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::{Value, json};
use skrifa::FontRef;
use std::borrow::Cow;
use std::sync::Arc;

/// The font used when the `font` setting is `null`: a Latin subset of Roboto Regular
/// (Apache-2.0; the crate README's "Bundled font" says how it was made).
pub(crate) static DEFAULT_FONT: &[u8] =
    include_bytes!("../../assets/fonts/Roboto-Regular-subset.ttf");

/// The longest `text`, in characters.
const MAX_TEXT_LEN: u32 = 4096;
/// The largest `size`, in pixels.
const MAX_SIZE: f64 = 1024.0;
/// The widest `outline_width`, in pixels.
const MAX_OUTLINE: u32 = 64;
/// The largest `padding`, in pixels.
const MAX_PADDING: u32 = 1024;
/// The largest `wrap_width`, in pixels: a source's widest frame.
const MAX_WRAP: u32 = 16384;

/// The `text` kind.
pub(crate) struct TextKind(KindDescriptor);

impl TextKind {
    pub(crate) fn new() -> Self {
        let live = |p: ParamDescriptor| ParamDescriptor { live: true, ..p };
        let pixels = |min: i64, max: u32| ParamType::Int {
            min: Some(min),
            max: Some(i64::from(max)),
            step: Some(1),
            unit: Some("px".into()),
        };
        let colour = || ParamType::Color { alpha: true };
        let option = |value: &str, label: &str| EnumOption {
            value: value.into(),
            label: label.into(),
        };
        Self(KindDescriptor {
            params: vec![
                live(param(
                    "text",
                    "Text",
                    ParamType::String {
                        multiline: true,
                        max_len: MAX_TEXT_LEN,
                    },
                    json!(""),
                )),
                ParamDescriptor {
                    description: Some(
                        "A TrueType or OpenType font, or the first font of a collection. \
                         None uses the bundled Roboto."
                            .into(),
                    ),
                    ..param("font", "Font", asset(&["ttf", "otf", "ttc"]), Value::Null)
                },
                live(param(
                    "size",
                    "Size",
                    ParamType::Float {
                        min: Some(1.0),
                        max: Some(MAX_SIZE),
                        step: Some(1.0),
                        unit: Some("px".into()),
                    },
                    json!(64.0),
                )),
                live(param("color", "Colour", colour(), json!("#ffffffff"))),
                live(param(
                    "align",
                    "Alignment",
                    ParamType::Enum {
                        options: vec![
                            option("left", "Left"),
                            option("center", "Centre"),
                            option("right", "Right"),
                        ],
                    },
                    json!("left"),
                )),
                live(ParamDescriptor {
                    description: Some("Lines wrap at this width. 0 doesn't wrap.".into()),
                    ..param("wrap_width", "Wrap width", pixels(0, MAX_WRAP), json!(0))
                }),
                live(param(
                    "outline_width",
                    "Outline width",
                    pixels(0, MAX_OUTLINE),
                    json!(0),
                )),
                live(param(
                    "outline_color",
                    "Outline colour",
                    colour(),
                    json!("#000000ff"),
                )),
                live(param(
                    "background",
                    "Background",
                    colour(),
                    json!("#00000000"),
                )),
                live(param(
                    "padding",
                    "Padding",
                    pixels(0, MAX_PADDING),
                    json!(0),
                )),
            ],
            ..kind("text", "Text", true, false)
        })
    }
}

impl SourceKind for TextKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    /// Reads and checks the font file, so this blocks on I/O, then draws the text.
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let (font, path) = match source::asset(settings, "font")? {
            Some((root, path)) => {
                let bytes = ctx.assets.read(root, &path, ctx.limits.max_file_bytes)?;
                (Cow::Owned(bytes), path)
            }
            None => (Cow::Borrowed(DEFAULT_FONT), "(bundled font)".to_owned()),
        };
        // Parsed once here to report a bad file; each draw parses it again, which only
        // reads the table directory.
        parse(&font).map_err(|reason| SourceError::Decode {
            path,
            format: "a TrueType or OpenType font",
            reason,
        })?;
        let (frame, extent) = draw(&font, settings, 1.0, &ctx.limits)?;
        Ok(Box::new(TextSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            limits: ctx.limits,
            font,
            frame,
            density: 1.0,
            extent,
        }))
    }
}

/// The first font in `bytes`, a font file or a collection.
pub(crate) fn parse(bytes: &[u8]) -> Result<FontRef<'_>, String> {
    FontRef::from_index(bytes, 0).map_err(|e| e.to_string())
}

/// A running `text` source. Its one frame is drawn on create, on each update and at
/// each new density.
struct TextSource {
    descriptor: KindDescriptor,
    settings: Settings,
    limits: Limits,
    /// The font file, already checked by [`parse`].
    font: Cow<'static, [u8]>,
    frame: Arc<VideoFrame>,
    /// How many pixels each way the frame draws per pixel of its extent (ADR-0071).
    density: f32,
    /// The frame's size at a density of 1, which it is placed at.
    extent: FrameSize,
}

/// The highest density a source draws at ([`Source::set_density`]): 8 times as many
/// pixels each way. The `text` kind is the one that draws at a density.
pub const MAX_DENSITY: f32 = 8.0;

impl Source for TextSource {
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
        (self.frame, self.extent) = draw(&self.font, settings, self.density, &self.limits)?;
        self.settings = settings.clone();
        Ok(())
    }

    /// Draws the text again at `density`, from 1 to [`MAX_DENSITY`]. A density whose
    /// frame would be over the limits is halved until it fits, down to 1, which the text
    /// already fit at when it was created or last updated.
    fn set_density(&mut self, density: f32) -> Option<FrameSize> {
        let mut density = if density.is_finite() {
            density.clamp(1.0, MAX_DENSITY)
        } else {
            1.0
        };
        while density != self.density {
            match draw(&self.font, &self.settings, density, &self.limits) {
                Ok((frame, extent)) => {
                    (self.frame, self.extent, self.density) = (frame, extent, density);
                    break;
                }
                Err(_) => density = (density / 2.0).max(1.0),
            }
        }
        Some(self.extent)
    }
}

/// Draws the text `settings` describe with `font` into a new frame, at `density`, and
/// returns it with its extent ([`raster::Drawn::extent`]).
fn draw(
    font: &[u8],
    settings: &Settings,
    density: f32,
    limits: &Limits,
) -> Result<(Arc<VideoFrame>, FrameSize), SourceError> {
    let text = string(settings, "text")?;
    if text.chars().count() > MAX_TEXT_LEN as usize {
        return Err(bad(
            "text",
            &format!("must be at most {MAX_TEXT_LEN} characters"),
        ));
    }
    let style = style(settings)?;
    let font = parse(font).map_err(|reason| SourceError::Decode {
        path: "(font)".into(),
        format: "a TrueType or OpenType font",
        reason,
    })?;
    let drawn = raster::render(&font, text, &style, density, limits)?;
    let frame = rgba_frame(drawn.size, drawn.rgba, MediaTime::zero(Timebase::MILLIS))?;
    Ok((Arc::new(frame), drawn.extent))
}

/// How the text is laid out and coloured: every setting but `text` and `font`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Style {
    /// The em size in pixels, as CSS's `font-size`.
    pub(crate) size: f32,
    pub(crate) color: [u8; 4],
    pub(crate) align: Align,
    /// `None` doesn't wrap.
    pub(crate) wrap_width: Option<u32>,
    pub(crate) outline_width: u32,
    pub(crate) outline_color: [u8; 4],
    pub(crate) background: [u8; 4],
    pub(crate) padding: u32,
}

/// How lines sit within the text's width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Center,
    Right,
}

fn style(settings: &Settings) -> Result<Style, SourceError> {
    let align = match string(settings, "align")? {
        "left" => Align::Left,
        "center" => Align::Center,
        "right" => Align::Right,
        _ => return Err(bad("align", "must be `left`, `center` or `right`")),
    };
    let size = setting(settings, "size")?
        .as_f64()
        .filter(|s| (1.0..=MAX_SIZE).contains(s))
        .ok_or_else(|| bad("size", &format!("must be a number from 1 to {MAX_SIZE}")))?;
    let wrap_width = pixels(settings, "wrap_width", MAX_WRAP)?;
    // From 1 to 1024, which an f32 holds closely enough.
    let size = size as f32;
    Ok(Style {
        size,
        color: color(settings, "color")?,
        align,
        wrap_width: (wrap_width > 0).then_some(wrap_width),
        outline_width: pixels(settings, "outline_width", MAX_OUTLINE)?,
        outline_color: color(settings, "outline_color")?,
        background: color(settings, "background")?,
        padding: pixels(settings, "padding", MAX_PADDING)?,
    })
}

pub(crate) fn bad(param: &str, reason: &str) -> SourceError {
    SourceError::Setting {
        param: param.to_owned(),
        reason: reason.to_owned(),
    }
}

fn string<'s>(settings: &'s Settings, id: &str) -> Result<&'s str, SourceError> {
    setting(settings, id)?
        .as_str()
        .ok_or_else(|| bad(id, "must be a string"))
}

/// A whole number of pixels from 0 to `max`.
pub(crate) fn pixels(settings: &Settings, id: &str, max: u32) -> Result<u32, SourceError> {
    setting(settings, id)?
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&v| v <= max)
        .ok_or_else(|| bad(id, &format!("must be a whole number from 0 to {max}")))
}

/// A colour setting as straight sRGB RGBA bytes.
pub(crate) fn color(settings: &Settings, id: &str) -> Result<[u8; 4], SourceError> {
    setting(settings, id)?
        .as_str()
        .and_then(parse_color)
        .ok_or_else(|| bad(id, "must be a colour as `#rrggbb` or `#rrggbbaa`"))
}
