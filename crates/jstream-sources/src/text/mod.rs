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

/// The font used when the `font` setting is `null` and `face` is `regular`: a Latin
/// subset of Roboto Regular (Apache-2.0; the crate README's "Bundled fonts" says how it
/// was made).
pub(crate) static DEFAULT_FONT: &[u8] =
    include_bytes!("../../assets/fonts/Roboto-Regular-subset.ttf");

/// The bundled faces the `face` setting picks between while `font` is `null`, by
/// option value. The same Latin subset as [`DEFAULT_FONT`], of the same family
/// (Apache-2.0; the crate README's "Bundled fonts").
static FACES: [(&str, &str, &[u8]); 3] = [
    ("regular", "Regular", DEFAULT_FONT),
    (
        "bold",
        "Bold",
        include_bytes!("../../assets/fonts/Roboto-Bold-subset.ttf"),
    ),
    (
        "condensed_bold",
        "Condensed bold",
        include_bytes!("../../assets/fonts/RobotoCondensed-Bold-subset.ttf"),
    ),
];

/// The bundled face named by `face`'s value.
fn bundled(settings: &Settings) -> Result<&'static [u8], SourceError> {
    let face = string(settings, "face")?;
    FACES
        .iter()
        .find(|(value, ..)| *value == face)
        .map(|(.., bytes)| *bytes)
        .ok_or_else(|| bad("face", "must be `regular`, `bold` or `condensed_bold`"))
}

/// The longest `text`, in characters.
const MAX_TEXT_LEN: u32 = 4096;
/// The largest `size`, in pixels.
const MAX_SIZE: f64 = 1024.0;
/// The widest `outline_width`, in pixels.
const MAX_OUTLINE: u32 = 64;
/// The largest `padding`, in pixels.
const MAX_PADDING: u32 = 1024;
/// The widest `letter_spacing` either way, in pixels.
const MAX_SPACING: f64 = 200.0;
/// The farthest a shadow is offset either way, in pixels.
const MAX_SHADOW_OFFSET: f64 = 200.0;
/// The largest shadow `blur`, in pixels.
const MAX_SHADOW_BLUR: f64 = 100.0;
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
        let float = |min: f64, max: f64, step: f64| ParamType::Float {
            min: Some(min),
            max: Some(max),
            step: Some(step),
            unit: Some("px".into()),
        };
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
                live(ParamDescriptor {
                    description: Some(
                        "The bundled face drawn while Font is none. A font file ignores it.".into(),
                    ),
                    ..param(
                        "face",
                        "Face",
                        ParamType::Enum {
                            options: FACES
                                .iter()
                                .map(|(value, label, _)| option(value, label))
                                .collect(),
                        },
                        json!("regular"),
                    )
                }),
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
                live(ParamDescriptor {
                    description: Some(
                        "Added after every character. Negative draws them closer.".into(),
                    ),
                    ..param(
                        "letter_spacing",
                        "Letter spacing",
                        float(-MAX_SPACING, MAX_SPACING, 0.5),
                        json!(0.0),
                    )
                }),
                live(ParamDescriptor {
                    description: Some(
                        "The text's shadow, drawn under its outline and fill. Fully \
                         transparent draws none."
                            .into(),
                    ),
                    ..param("shadow_color", "Shadow color", colour(), json!("#00000000"))
                }),
                live(param(
                    "shadow_x",
                    "Shadow X",
                    float(-MAX_SHADOW_OFFSET, MAX_SHADOW_OFFSET, 1.0),
                    json!(0.0),
                )),
                live(param(
                    "shadow_y",
                    "Shadow Y",
                    float(-MAX_SHADOW_OFFSET, MAX_SHADOW_OFFSET, 1.0),
                    json!(0.0),
                )),
                live(ParamDescriptor {
                    description: Some("How far the shadow's edge spreads, softening.".into()),
                    ..param(
                        "shadow_blur",
                        "Shadow blur",
                        float(0.0, MAX_SHADOW_BLUR, 1.0),
                        json!(0.0),
                    )
                }),
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
            None => (
                Cow::Borrowed(bundled(settings)?),
                "(bundled font)".to_owned(),
            ),
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
    /// How many pixels each way the frame draws per pixel of its extent (ADR-0073).
    density: f32,
    /// The frame's size at a density of 1, which it is placed at.
    extent: FrameSize,
}

/// The highest density a source draws at ([`Source::set_density`]): 32 times as many
/// pixels each way, so a default 64 px line stretched across a 4K canvas is drawn at
/// the size it is shown. The `text` kind is the one that draws at a density, and
/// it draws at a lower one when the frame would be larger than a 4K canvas
/// (`raster::render`, ADR-0079).
pub const MAX_DENSITY: f32 = 32.0;

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
        // A bundled face is swapped live; a font file ignores `face`.
        let font = match source::asset(settings, "font")? {
            None => Cow::Borrowed(bundled(settings)?),
            Some(_) => self.font.clone(),
        };
        (self.frame, self.extent) = draw(&font, settings, self.density, &self.limits)?;
        self.font = font;
        self.settings = settings.clone();
        Ok(())
    }

    /// Draws the text again at `density`, from 1 to [`MAX_DENSITY`], or at the highest
    /// density under that whose frame [`raster::render`] allows. A density whose frame
    /// would be over the limits is halved until it fits, down to 1, which the text
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
    /// Pixels added after every character, negative to draw them closer.
    pub(crate) letter_spacing: f32,
    pub(crate) shadow: Shadow,
}

/// A text's shadow: its ink, offset and blurred, under the outline and fill.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Shadow {
    /// Fully transparent draws none.
    pub(crate) color: [u8; 4],
    /// The offset in pixels, right and down.
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// How far the edge spreads, in pixels: twice the Gaussian's standard deviation,
    /// as CSS's `text-shadow` blur radius.
    pub(crate) blur: f32,
}

impl Shadow {
    /// Whether it draws anything.
    pub(crate) fn shows(&self) -> bool {
        self.color[3] > 0
    }
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
        letter_spacing: number(settings, "letter_spacing", -MAX_SPACING, MAX_SPACING)?,
        shadow: Shadow {
            color: color(settings, "shadow_color")?,
            x: number(settings, "shadow_x", -MAX_SHADOW_OFFSET, MAX_SHADOW_OFFSET)?,
            y: number(settings, "shadow_y", -MAX_SHADOW_OFFSET, MAX_SHADOW_OFFSET)?,
            blur: number(settings, "shadow_blur", 0.0, MAX_SHADOW_BLUR)?,
        },
    })
}

/// A number from `min` to `max`.
fn number(settings: &Settings, id: &str, min: f64, max: f64) -> Result<f32, SourceError> {
    setting(settings, id)?
        .as_f64()
        .filter(|v| (min..=max).contains(v))
        // Within ±200, which an f32 holds closely enough.
        .map(|v| v as f32)
        .ok_or_else(|| bad(id, &format!("must be a number from {min} to {max}")))
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
