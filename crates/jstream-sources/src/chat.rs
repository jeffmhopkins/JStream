//! The `chat_message` source: the featured live chat message (ADR-0041 §2), drawn as
//! plain text. A [`ChatFeature`] in the [`Context`] holds the message; whoever reads the
//! chat sets it, and every `chat_message` source made from that context draws it.

use crate::color::rgba_frame;
use crate::describe::{asset, kind, param};
use crate::source::{self, changed_non_live, setting};
use crate::text::raster::{render, stack};
use crate::text::{Align, DEFAULT_FONT, Style, bad, color, parse, pixels};
use crate::{Context, Limits, Painter, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The most characters of a featured message's text drawn.
pub const MAX_CHAT_TEXT: usize = 1000;
/// The most characters of an author's name, badge or amount drawn.
pub const MAX_CHAT_LABEL: usize = 100;

/// The largest `size`, in pixels.
const MAX_SIZE: f64 = 256.0;
/// The widest `max_width`, in pixels: a source's widest frame.
const MAX_WIDTH: u32 = 16384;
/// The widest `outline_width`, in pixels.
const MAX_OUTLINE: u32 = 64;
/// The largest `padding`, in pixels.
const MAX_PADDING: u32 = 1024;
/// How often a source looks for a newly featured message, in milliseconds.
const CHECK_EVERY_MS: i64 = 100;

/// A chat message to show on stream. Every field is untrusted text from a stranger: it
/// is drawn as plain text, with control characters replaced by spaces, and cut to
/// [`MAX_CHAT_TEXT`] or [`MAX_CHAT_LABEL`] characters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeaturedMessage {
    /// The author's display name.
    pub author: String,
    /// A badge shown before the name, such as `Moderator` or `Member`.
    pub badge: Option<String>,
    /// The message.
    pub text: String,
    /// A Super Chat's amount as displayed, such as `$5.00`.
    pub amount: Option<String>,
}

/// The featured message slot: runtime state, not part of the collection (ADR-0041 §2).
/// Clones share the slot.
#[derive(Clone, Debug, Default)]
pub struct ChatFeature(Arc<Slot>);

#[derive(Debug, Default)]
struct Slot {
    /// Bumped on every [`ChatFeature::set`], so a source can tell a change without the
    /// lock.
    revision: AtomicU64,
    message: Mutex<Option<Arc<FeaturedMessage>>>,
}

impl ChatFeature {
    /// An empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Features `message`, or clears the slot with `None`.
    pub fn set(&self, message: Option<FeaturedMessage>) {
        let mut slot = self
            .0
            .message
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *slot = message.map(Arc::new);
        self.0.revision.fetch_add(1, Ordering::Release);
    }

    /// The featured message, if any.
    pub fn get(&self) -> Option<Arc<FeaturedMessage>> {
        self.read().1
    }

    fn revision(&self) -> u64 {
        self.0.revision.load(Ordering::Acquire)
    }

    /// The message with the revision it was read at.
    fn read(&self) -> (u64, Option<Arc<FeaturedMessage>>) {
        let slot = self
            .0
            .message
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (self.revision(), slot.clone())
    }
}

/// The `chat_message` kind.
pub(crate) struct ChatKind(KindDescriptor);

impl ChatKind {
    pub(crate) fn new() -> Self {
        let live = |p: ParamDescriptor| ParamDescriptor { live: true, ..p };
        let pixels = |max: u32| ParamType::Int {
            min: Some(0),
            max: Some(i64::from(max)),
            step: Some(1),
            unit: Some("px".into()),
        };
        let colour = || ParamType::Color { alpha: true };
        Self(KindDescriptor {
            params: vec![
                ParamDescriptor {
                    description: Some(
                        "A TrueType or OpenType font. None uses the bundled Roboto, which \
                         covers Latin text only."
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
                    json!(32.0),
                )),
                live(param(
                    "author_color",
                    "Author colour",
                    colour(),
                    json!("#ffd54fff"),
                )),
                live(param(
                    "text_color",
                    "Text colour",
                    colour(),
                    json!("#ffffffff"),
                )),
                live(param(
                    "background",
                    "Background",
                    colour(),
                    json!("#000000b3"),
                )),
                live(param("padding", "Padding", pixels(MAX_PADDING), json!(16))),
                live(ParamDescriptor {
                    description: Some("Lines wrap at this width. 0 doesn't wrap.".into()),
                    ..param("max_width", "Maximum width", pixels(MAX_WIDTH), json!(600))
                }),
                live(param(
                    "outline_width",
                    "Outline width",
                    pixels(MAX_OUTLINE),
                    json!(0),
                )),
                live(param(
                    "outline_color",
                    "Outline colour",
                    colour(),
                    json!("#000000ff"),
                )),
            ],
            ..kind("chat_message", "Chat message", true, false)
        })
    }
}

impl SourceKind for ChatKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    /// Reads and checks the font file, so this blocks on I/O, then draws the featured
    /// message.
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let (font, path) = match source::asset(settings, "font")? {
            Some((root, path)) => {
                let bytes = ctx.assets.read(root, &path, ctx.limits.max_file_bytes)?;
                (Cow::Owned(bytes), path)
            }
            None => (Cow::Borrowed(DEFAULT_FONT), "(bundled font)".to_owned()),
        };
        parse(&font).map_err(|reason| SourceError::Decode {
            path,
            format: "a TrueType or OpenType font",
            reason,
        })?;
        let (revision, message) = ctx.chat.read();
        let frame = show(&font, message.as_deref(), settings, &ctx.limits)?;
        Ok(Box::new(ChatSource {
            descriptor: self.0.clone(),
            painter: Arc::new(ChatPainter {
                limits: ctx.limits,
                feature: ctx.chat.clone(),
                font,
                drawing: Mutex::new(()),
                ready: Mutex::new(Ready {
                    settings: settings.clone(),
                    style: 0,
                    revision,
                    frame,
                }),
            }),
        }))
    }
}

/// A running `chat_message` source. Its [`Painter`] redraws when another message is
/// featured; `frame` only hands out the frame drawn last, so a caller holding a lock
/// while it asks never waits on drawing.
struct ChatSource {
    descriptor: KindDescriptor,
    painter: Arc<ChatPainter>,
}

impl Source for ChatSource {
    /// Hands out the frame drawn last, by [`create`](SourceKind::create),
    /// [`update`](Source::update) or the painter. It neither draws nor allocates, and
    /// never fails.
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&lock(&self.painter.ready).frame))
    }

    /// The featured message can change at any time, so the source is looked at again
    /// every 100 ms.
    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        let ms = i64::try_from(source::millis(at)).unwrap_or(i64::MAX);
        Some(MediaTime::new(
            ms.saturating_add(CHECK_EVERY_MS),
            Timebase::MILLIS,
        ))
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        let painter = &self.painter;
        let _drawing = lock(&painter.drawing);
        let old = lock(&painter.ready).settings.clone();
        if let Some(param) = changed_non_live(&self.descriptor, &old, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let (revision, message) = painter.feature.read();
        let frame = show(&painter.font, message.as_deref(), settings, &painter.limits)?;
        let mut ready = lock(&painter.ready);
        ready.settings = settings.clone();
        ready.style = ready.style.wrapping_add(1);
        ready.revision = revision;
        ready.frame = frame;
        Ok(())
    }

    fn painter(&self) -> Option<Arc<dyn Painter>> {
        let painter: Arc<dyn Painter> = self.painter.clone();
        Some(painter)
    }
}

/// A `chat_message` source's drawing, shared between the source and whoever paints it.
struct ChatPainter {
    limits: Limits,
    feature: ChatFeature,
    /// The font file, already checked by [`parse`].
    font: Cow<'static, [u8]>,
    /// Held while a frame is drawn, so paints and updates take turns.
    drawing: Mutex<()>,
    /// What the frame drawn last shows. Held only to copy or swap it, so
    /// [`Source::frame`] never waits on drawing.
    ready: Mutex<Ready>,
}

struct Ready {
    settings: Settings,
    /// Bumped by every update, so a paint that started under older settings is dropped.
    style: u64,
    /// The slot's revision the frame shows.
    revision: u64,
    frame: Arc<VideoFrame>,
}

impl Painter for ChatPainter {
    /// Redraws if another message was featured since the frame drawn last. Only that
    /// redraw allocates. A message these settings and limits can't draw (too wide
    /// unwrapped, or over the memory cap) shows nothing, once, until another message is
    /// featured.
    fn paint(&self, _: MediaTime) {
        if self.feature.revision() == lock(&self.ready).revision {
            return;
        }
        let _drawing = lock(&self.drawing);
        let (style, settings) = {
            let ready = lock(&self.ready);
            (ready.style, ready.settings.clone())
        };
        let (revision, message) = self.feature.read();
        let Ok(frame) =
            draw(&self.font, message.as_deref(), &settings, &self.limits).or_else(|_| blank())
        else {
            return;
        };
        let mut ready = lock(&self.ready);
        if ready.style == style {
            ready.revision = revision;
            ready.frame = frame;
        }
    }
}

/// Locks `mutex`; a panic while it was held leaves nothing half-done that matters for
/// drawing, so a poisoned lock is used as it is.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Checks `settings`, then draws `message` with them. Only the settings can fail it: a
/// message they or the limits can't draw shows [`blank`], as the painter
/// does, since it is a viewer's text and not the settings being applied.
fn show(
    font: &[u8],
    message: Option<&FeaturedMessage>,
    settings: &Settings,
    limits: &Limits,
) -> Result<Arc<VideoFrame>, SourceError> {
    let empty = draw(font, None, settings, limits)?;
    if message.is_none() {
        return Ok(empty);
    }
    draw(font, message, settings, limits).or(Ok(empty))
}

/// One transparent pixel: what a source shows with nothing (drawable) featured.
fn blank() -> Result<Arc<VideoFrame>, SourceError> {
    let zero = MediaTime::zero(Timebase::MILLIS);
    Ok(Arc::new(rgba_frame(
        FrameSize::new(1, 1)?,
        vec![0; 4],
        zero,
    )?))
}

/// Draws `message` as `settings` style it: a header line (badge, author, amount) in the
/// author colour over the text in the text colour. No message draws one transparent
/// pixel.
fn draw(
    font: &[u8],
    message: Option<&FeaturedMessage>,
    settings: &Settings,
    limits: &Limits,
) -> Result<Arc<VideoFrame>, SourceError> {
    let size = setting(settings, "size")?
        .as_f64()
        .filter(|s| (1.0..=MAX_SIZE).contains(s))
        .ok_or_else(|| bad("size", &format!("must be a number from 1 to {MAX_SIZE}")))?;
    let max_width = pixels(settings, "max_width", MAX_WIDTH)?;
    let padding = pixels(settings, "padding", MAX_PADDING)?;
    let base = Style {
        // From 1 to 256, which an f32 holds exactly.
        size: size as f32,
        color: color(settings, "text_color")?,
        align: Align::Left,
        wrap_width: (max_width > 0).then(|| max_width.saturating_sub(padding * 2).max(1)),
        outline_width: pixels(settings, "outline_width", MAX_OUTLINE)?,
        outline_color: color(settings, "outline_color")?,
        background: [0; 4],
        padding: 0,
    };
    let author_color = color(settings, "author_color")?;
    let background = color(settings, "background")?;
    let zero = MediaTime::zero(Timebase::MILLIS);
    let Some(message) = message else {
        return blank();
    };
    let font = parse(font).map_err(|reason| SourceError::Decode {
        path: "(font)".into(),
        format: "a TrueType or OpenType font",
        reason,
    })?;

    let mut header = String::new();
    if let Some(badge) = message.badge.as_deref().filter(|b| !b.is_empty()) {
        header.push_str(&plain(badge, MAX_CHAT_LABEL));
        header.push_str(" · ");
    }
    header.push_str(&plain(&message.author, MAX_CHAT_LABEL));
    if let Some(amount) = message.amount.as_deref().filter(|a| !a.is_empty()) {
        header.push_str("  ");
        header.push_str(&plain(amount, MAX_CHAT_LABEL));
    }
    let mut blocks = vec![fit(
        &font,
        &header,
        &Style {
            color: author_color,
            ..base
        },
        limits,
    )?];
    let text = plain(&message.text, MAX_CHAT_TEXT);
    if !text.trim().is_empty() {
        blocks.push(fit(&font, &text, &base, limits)?);
    }
    let gap = (size / 4.0).round() as u32;
    let (frame_size, rgba) = stack(&blocks, gap, background, padding, limits)?;
    Ok(Arc::new(rgba_frame(frame_size, rgba, zero)?))
}

/// Draws `text` as narrow as it fits: on one line when that is no wider than the
/// style's wrap width, wrapped at that width otherwise.
fn fit(
    font: &skrifa::FontRef<'_>,
    text: &str,
    style: &Style,
    limits: &Limits,
) -> Result<(FrameSize, Vec<u8>), SourceError> {
    let drawn = |style: &Style| {
        render(font, text, style, 1.0, limits).map(|drawn| (drawn.size, drawn.rgba))
    };
    let Some(wrap) = style.wrap_width else {
        return drawn(style);
    };
    let unwrapped = Style {
        wrap_width: None,
        ..*style
    };
    let first = drawn(&unwrapped);
    if matches!(&first, Ok(drawn) if drawn.0.width() <= wrap) {
        return first;
    }
    // Freed before the wrapped draw, so the two never count against memory together.
    drop(first);
    drawn(style)
}

/// `text` as one paragraph of plain text: line breaks and other control characters
/// become spaces, cut to `max` characters.
fn plain(text: &str, max: usize) -> String {
    text.chars()
        .take(max)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
