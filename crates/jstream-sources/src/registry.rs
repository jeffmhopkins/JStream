//! The kind registry: every source kind by ID, for `kind.list`, `kind.describe` and
//! creating sources.

use crate::audio;
use crate::browser::BrowserKind;
use crate::color::ColorKind;
use crate::describe::described;
use crate::image::ImageKind;
use crate::media::MediaKind;
use crate::scope::{SpectrumKind, WaveformKind};
use crate::text::TextKind;
use crate::youtube_chat::YoutubeChatKind;
use crate::{Context, Source, SourceError, SourceKind};
use jstream_protocol::{KindDescriptor, Settings};
use std::collections::BTreeMap;

/// Every source kind the engine knows, keyed and ordered by kind ID.
pub struct Registry {
    kinds: BTreeMap<String, Box<dyn SourceKind>>,
}

impl Registry {
    /// The built-in kinds (ADR-0031, docs/adr/0031-source-types-and-decoding-scope.md).
    /// `browser`, `color`, `image`, `media`, `text`, `waveform`, `spectrum` and
    /// `youtube_chat` produce frames (`browser` and its `youtube_chat` preset through the
    /// injected backend, on the GPU). `audio_input`, `desktop_audio` and `app_audio` are
    /// audio-only: the engine records them, and `create` fails with
    /// [`SourceError::AudioOnly`]. `display`, `webcam` and `window` are described only, so
    /// their sources can be created and configured but fail to start with
    /// [`SourceError::NotBuilt`].
    pub fn builtin() -> Self {
        let mut kinds: BTreeMap<String, Box<dyn SourceKind>> = BTreeMap::new();
        let mut add = |kind: Box<dyn SourceKind>| {
            kinds.insert(kind.descriptor().kind_id.clone(), kind);
        };
        add(Box::new(BrowserKind::new()));
        add(Box::new(YoutubeChatKind::new()));
        add(Box::new(ColorKind::new()));
        add(Box::new(ImageKind::new()));
        add(Box::new(MediaKind::new()));
        add(Box::new(TextKind::new()));
        add(Box::new(WaveformKind::new()));
        add(Box::new(SpectrumKind::new()));
        for kind in audio::kinds() {
            add(Box::new(kind));
        }
        for kind in described() {
            add(Box::new(kind));
        }
        Self { kinds }
    }

    /// Every kind's descriptor, ordered by kind ID: what the engine's graph validates
    /// settings against.
    pub fn descriptors(&self) -> Vec<KindDescriptor> {
        self.kinds
            .values()
            .map(|k| k.descriptor().clone())
            .collect()
    }

    /// One kind's descriptor.
    pub fn descriptor(&self, kind_id: &str) -> Option<&KindDescriptor> {
        self.kinds.get(kind_id).map(|k| k.descriptor())
    }

    /// Creates a source of `kind_id` from a complete settings map, as the graph stores
    /// it. This may block on reading and decoding an asset.
    ///
    /// # Errors
    /// [`SourceError::UnknownKind`] for an unregistered kind, and the kind's own errors.
    pub fn create(
        &self,
        kind_id: &str,
        settings: &Settings,
        ctx: &Context,
    ) -> Result<Box<dyn Source>, SourceError> {
        self.kinds
            .get(kind_id)
            .ok_or_else(|| SourceError::UnknownKind {
                kind_id: kind_id.to_owned(),
            })?
            .create(settings, ctx)
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.kinds.keys()).finish()
    }
}
