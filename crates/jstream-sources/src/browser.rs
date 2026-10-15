//! The `browser` source (ADR-0021, docs/adr/0021-browser-source.md): a web page or a
//! local HTML file rendered off screen by the injected [`BrowserBackend`], with its
//! audio recorded like a device's.

use crate::color::frame_size;
use crate::describe::{asset, kind, param, size};
use crate::source::{changed_non_live, setting};
use crate::{Context, Source, SourceAudio, SourceError, SourceKind};
use jstream_browser::{
    BrowserPage, DEFAULT_CSS, MAX_CSS_BYTES, MAX_FPS, PageAudio, PageLocation, PageSpec,
};
use jstream_media::{AudioFormat, MediaTime, Timebase, VideoFrame};
use jstream_platform::{AudioInputHandler, AudioStream, GpuFrame};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::{Value, json};
use std::sync::Arc;

/// The kind's ID.
const BROWSER: &str = "browser";

/// Whether sources of `kind` open their page in the injected [`BrowserBackend`]
/// (`browser` and its presets), so a host that sets the backend late must build them
/// again.
///
/// [`BrowserBackend`]: crate::BrowserBackend
pub fn opens_browser(kind: &str) -> bool {
    kind == BROWSER || kind == crate::youtube_chat::KIND
}

/// The longest `url` setting, in characters.
const MAX_URL_CHARS: u32 = 8192;

/// The `browser` kind.
pub(crate) struct BrowserKind(KindDescriptor);

impl BrowserKind {
    pub(crate) fn new() -> Self {
        let text = |max_len, multiline| ParamType::String { multiline, max_len };
        Self(KindDescriptor {
            params: vec![
                ParamDescriptor {
                    description: Some(
                        "The page's address, http:// or https://. Ignored when a local file \
                         is set."
                            .into(),
                    ),
                    ..param("url", "URL", text(MAX_URL_CHARS, false), json!(""))
                },
                ParamDescriptor {
                    description: Some(
                        "A local HTML file to show instead of a URL. The page can load files \
                         beside it."
                            .into(),
                    ),
                    ..param("file", "Local file", asset(&["html", "htm"]), Value::Null)
                },
                param("width", "Width", size(), json!(1920)),
                param("height", "Height", size(), json!(1080)),
                ParamDescriptor {
                    description: Some(
                        "The most frames a second the page paints. It paints less when \
                         nothing changes."
                            .into(),
                    ),
                    ..param(
                        "fps",
                        "Frame rate",
                        ParamType::Int {
                            min: Some(1),
                            max: Some(i64::from(MAX_FPS)),
                            step: Some(1),
                            unit: Some("fps".into()),
                        },
                        json!(30),
                    )
                },
                ParamDescriptor {
                    description: Some(
                        "CSS added to the page after it loads. The default makes the page's \
                         background transparent."
                            .into(),
                    ),
                    live: true,
                    ..param(
                        "css",
                        "Custom CSS",
                        text(u32::try_from(MAX_CSS_BYTES).unwrap_or(u32::MAX), true),
                        json!(DEFAULT_CSS),
                    )
                },
            ],
            ..kind(BROWSER, "Browser", true, true)
        })
    }
}

impl SourceKind for BrowserKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        open(&self.0, settings, ctx, location(settings, ctx)?)
    }
}

/// Opens a page at `location` with the `width`, `height`, `fps` and `css` settings: a
/// `browser` source, or a kind built on one (`youtube_chat`). `descriptor` is the
/// kind's, for telling live changes from restarts.
pub(crate) fn open(
    descriptor: &KindDescriptor,
    settings: &Settings,
    ctx: &Context,
    location: PageLocation,
) -> Result<Box<dyn Source>, SourceError> {
    let size = frame_size(settings, &ctx.limits)?;
    let fps = setting(settings, "fps")?
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| (1..=MAX_FPS).contains(v))
        .ok_or_else(|| SourceError::Setting {
            param: "fps".into(),
            reason: format!("must be a whole number from 1 to {MAX_FPS}"),
        })?;
    let css = css(settings)?.to_owned();
    let backend = ctx
        .browser
        .as_ref()
        .ok_or(SourceError::NoBackend { backend: "browser" })?;
    // The transparent stand-in `frame` returns (see `BrowserSource::frame`), made
    // before the page opens so a size the limits refuse opens nothing.
    let blank = Arc::new(crate::color::rgba_frame(
        size,
        vec![
            0;
            usize::try_from(u64::from(size.width()) * u64::from(size.height()) * 4).map_err(
                |_| jstream_media::MediaError::Overflow {
                    what: "a browser frame",
                }
            )?
        ],
        MediaTime::zero(Timebase::MILLIS),
    )?);
    let page = backend.open(&PageSpec {
        location,
        size,
        fps,
        css,
    })?;
    let audio: Arc<dyn SourceAudio> = Arc::new(PageAudioOf(Arc::clone(page.audio())));
    Ok(Box::new(BrowserSource {
        descriptor: descriptor.clone(),
        settings: settings.clone(),
        page,
        blank,
        audio,
    }))
}

/// Where the page loads from: the `file` asset when it is set, else the `url`.
fn location(settings: &Settings, ctx: &Context) -> Result<PageLocation, SourceError> {
    if let Some((root, path)) = crate::source::asset(settings, "file")? {
        return Ok(PageLocation::File(ctx.assets.resolve(root, &path)?));
    }
    let url = setting(settings, "url")?
        .as_str()
        .ok_or_else(|| SourceError::Setting {
            param: "url".into(),
            reason: "must be a string".into(),
        })?;
    PageLocation::web(url).map_err(|e| SourceError::Setting {
        param: "url".into(),
        reason: e.to_string(),
    })
}

/// The `css` setting, at most [`MAX_CSS_BYTES`] bytes.
fn css(settings: &Settings) -> Result<&str, SourceError> {
    setting(settings, "css")?
        .as_str()
        .filter(|css| css.len() <= MAX_CSS_BYTES)
        .ok_or_else(|| SourceError::Setting {
            param: "css".into(),
            reason: format!("must be text of at most {MAX_CSS_BYTES} bytes"),
        })
}

/// A running `browser` source: an open page.
struct BrowserSource {
    descriptor: KindDescriptor,
    settings: Settings,
    page: Box<dyn BrowserPage>,
    /// A transparent frame of the page's size.
    blank: Arc<VideoFrame>,
    audio: Arc<dyn SourceAudio>,
}

impl Source for BrowserSource {
    /// A transparent frame of the page's size: the page itself is only on the GPU
    /// ([`gpu_frame`](Source::gpu_frame)), and is never copied to the CPU (ADR-0021).
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&self.blank))
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
        let css = css(settings)?;
        if Some(css) != self.settings.get("css").and_then(Value::as_str) {
            self.page.set_css(css);
        }
        self.settings = settings.clone();
        Ok(())
    }

    fn gpu_frame(&mut self) -> Option<GpuFrame> {
        self.page.frame()
    }

    fn audio(&self) -> Option<Arc<dyn SourceAudio>> {
        Some(Arc::clone(&self.audio))
    }
}

/// A page's audio as a [`SourceAudio`].
struct PageAudioOf(Arc<PageAudio>);

impl SourceAudio for PageAudioOf {
    fn format(&self) -> AudioFormat {
        PageAudio::format()
    }

    fn open(&self, handler: Box<dyn AudioInputHandler>) -> Box<dyn AudioStream> {
        Box::new(self.0.open(handler))
    }
}
