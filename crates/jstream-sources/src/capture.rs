//! The `display` and `window` sources: what the platform captures.
//!
//! Their frames don't come from [`Source::frame`]. The platform's capture backend
//! delivers them, on its own thread, to the engine's render thread, as CPU frames or as
//! textures shared with the compositor. A capture source only says what to capture
//! ([`Source::capture`]); the engine starts and reads the capture.

use crate::describe::{kind, param};
use crate::source::{changed_non_live, setting};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{MediaTime, VideoFrame};
use jstream_protocol::{KindDescriptor, ParamType, Settings};
use serde_json::json;
use std::sync::Arc;

/// The longest `target` setting, in characters. A Windows target ID is under 300.
const MAX_TARGET: u32 = 1024;

/// What a capture source shows.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CaptureSpec {
    /// A whole display: the one whose capture-backend target ID is given, or the
    /// primary display when `None`.
    Display(Option<String>),
    /// One top-level window: the one whose capture-backend target ID is given, or
    /// nothing when `None`.
    Window(Option<String>),
}

/// The `display` or `window` kind.
pub(crate) struct CaptureKind {
    descriptor: KindDescriptor,
    window: bool,
}

impl CaptureKind {
    /// The `display` kind. An empty `target` captures the primary display, which is
    /// what a `display` source showed before it had settings.
    pub(crate) fn display() -> Self {
        Self::new("display", "Display capture", "Display", false)
    }

    /// The `window` kind. An empty `target` captures nothing.
    pub(crate) fn window() -> Self {
        Self::new("window", "Window capture", "Window", true)
    }

    fn new(id: &str, name: &str, label: &str, window: bool) -> Self {
        let mut target = param(
            "target",
            label,
            ParamType::String {
                multiline: false,
                max_len: MAX_TARGET,
            },
            json!(""),
        );
        target.description = Some(if window {
            "The capture target ID of the window to capture. Empty captures nothing.".into()
        } else {
            "The capture target ID of the display to capture. Empty captures the primary \
             display."
                .into()
        });
        Self {
            descriptor: KindDescriptor {
                params: vec![target],
                ..kind(id, name, true, false)
            },
            window,
        }
    }

    /// What the settings say to capture.
    fn spec(&self, settings: &Settings) -> Result<CaptureSpec, SourceError> {
        let value = setting(settings, "target")?;
        let target = value.as_str().ok_or_else(|| SourceError::Setting {
            param: "target".into(),
            reason: "must be a string".into(),
        })?;
        let target = (!target.is_empty()).then(|| target.to_owned());
        Ok(if self.window {
            CaptureSpec::Window(target)
        } else {
            CaptureSpec::Display(target)
        })
    }
}

impl SourceKind for CaptureKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.descriptor
    }

    fn create(&self, settings: &Settings, _: &Context) -> Result<Box<dyn Source>, SourceError> {
        Ok(Box::new(CaptureSource {
            kind_id: self.descriptor.kind_id.clone(),
            descriptor: self.descriptor.clone(),
            settings: settings.clone(),
            spec: self.spec(settings)?,
        }))
    }
}

/// A running `display` or `window` source.
struct CaptureSource {
    kind_id: String,
    descriptor: KindDescriptor,
    settings: Settings,
    spec: CaptureSpec,
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
}
