//! WGSL effect packs in the engine (ADR-0035, ADR-0025 › Effect packs): the packs
//! folder watched with `jstream-effects`' [`PackWatcher`], each loaded effect a
//! `video_filter` kind in `kind.list`, and drawn by the chains like a built-in kind
//! ([`crate::chain`]).
//!
//! [`Packs::poll`] reloads what changed on disk and returns an [`Update`]: the kinds to
//! describe and to forget, and the effects plans read from then on. The engine applies
//! it under the collection lock and re-plans, so a source's chain draws a pack filter
//! once its pack loads, and a filter of a pack that's gone is bypassed again, as before
//! its pack loaded. A reloaded effect is a new [`PackEffect`], so each compositor makes
//! its filter again with the new WGSL and uniform layout.
//!
//! Docs: crates/jstream-core/README.md#design, crates/jstream-effects/README.md#purpose

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use jstream_effects::{Effect, PackWatcher};
use jstream_protocol::{KindDescriptor, Settings};
use jstream_render::VideoFilter;

/// One loaded effect, as a chain draws it. Two are equal only when they are the same
/// load: a reloaded effect is a new one, so the filters drawing it are made again.
#[derive(Clone)]
pub(crate) struct PackEffect(Arc<Effect>);

impl PackEffect {
    pub(crate) fn new(effect: Effect) -> Self {
        Self(Arc::new(effect))
    }

    /// A render filter drawing this effect with `settings`.
    pub(crate) fn video_filter(&self, settings: &Settings) -> VideoFilter {
        self.0.video_filter(settings)
    }
}

impl PartialEq for PackEffect {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for PackEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PackEffect")
            .field(&self.0.kind_id())
            .finish()
    }
}

/// The loaded effects, by kind ID (`<pack>/<effect>`), as plans read them.
pub(crate) type PackEffects = Arc<BTreeMap<String, PackEffect>>;

/// What a poll changed, for the engine to apply under the collection lock.
#[derive(Debug)]
pub(crate) struct Update {
    /// Kinds whose pack is gone, or whose effect left its manifest.
    pub(crate) removed: Vec<String>,
    /// Kinds added or changed, with their new descriptors.
    pub(crate) described: Vec<KindDescriptor>,
    /// Every loaded effect from now on.
    pub(crate) effects: PackEffects,
}

/// The engine's packs folder and the effects loaded from it.
#[derive(Debug)]
pub(crate) struct Packs {
    watcher: PackWatcher,
    effects: PackEffects,
}

impl Packs {
    /// Watches `root`, a folder of pack folders. Nothing loads until the first
    /// [`Self::poll`].
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            watcher: PackWatcher::new(root, jstream_render::FILTER_PRELUDE),
            effects: PackEffects::default(),
        }
    }

    /// Loads, reloads and drops packs to match the folder. `None` when no kind changed.
    /// A pack that fails to load is logged; one that fails a reload keeps its last good
    /// version.
    pub(crate) fn poll(&mut self) -> Option<Update> {
        let changes = self.watcher.poll();
        for error in &changes.errors {
            tracing::warn!(root = %self.watcher.root().display(), %error, "effect pack not loaded");
        }
        if changes.kinds.is_empty() {
            return None;
        }
        // Rebuilt from the watcher, keeping the effects that didn't change, so the
        // filters drawing them keep running.
        let changed: Vec<&str> = changes
            .kinds
            .iter()
            .filter_map(|change| match change {
                jstream_effects::KindChange::Added(id)
                | jstream_effects::KindChange::Changed(id) => Some(id.as_str()),
                jstream_effects::KindChange::Removed(_) => None,
            })
            .collect();
        let effects: BTreeMap<String, PackEffect> = self
            .watcher
            .packs()
            .flat_map(jstream_effects::Pack::effects)
            .map(|effect| {
                let id = effect.kind_id();
                let kept = self
                    .effects
                    .get(id)
                    .filter(|_| !changed.contains(&id))
                    .cloned();
                (
                    id.to_owned(),
                    kept.unwrap_or_else(|| PackEffect::new(effect.clone())),
                )
            })
            .collect();
        let removed = self
            .effects
            .keys()
            .filter(|id| !effects.contains_key(*id))
            .cloned()
            .collect();
        let described = changed
            .iter()
            .filter_map(|id| self.watcher.effect(id))
            .map(|effect| effect.descriptor().clone())
            .collect();
        self.effects = Arc::new(effects);
        for change in &changes.kinds {
            tracing::info!(?change, "effect pack kind");
        }
        Some(Update {
            removed,
            described,
            effects: Arc::clone(&self.effects),
        })
    }
}

/// A test pack, `tint`, with one effect, `tint/mul`, that multiplies its input by its
/// `tint` setting (default green).
#[cfg(test)]
pub(crate) mod test_pack {
    use std::path::{Path, PathBuf};

    pub(crate) const KIND: &str = "tint/mul";

    const MANIFEST: &str = r##"
[pack]
id = "tint"
name = "Tint"

[[effect]]
id = "mul"
name = "Multiply"
version = 1
shader = "mul.wgsl"

[[effect.param]]
id = "tint"
label = "Tint"
type = "color"
default = "#00ff00"
"##;

    /// The shader: the input times the tint.
    pub(crate) const MUL: &str = "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n    let c = src(in.uv);\n    return vec4<f32>(c.rgb * p.tint.rgb, c.a);\n}\n";

    /// Another shader, of another length: opaque blue, whatever the input.
    pub(crate) const BLUE: &str = "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n    return vec4<f32>(0.0, 0.0, 1.0, 1.0);\n}\n";

    /// A fresh, empty folder of pack folders under the system temp folder, removed on
    /// drop.
    pub(crate) struct Temp(pub(crate) PathBuf);

    impl Temp {
        pub(crate) fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("jstream-core-packs-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        /// The `tint` pack's folder.
        pub(crate) fn pack(&self) -> PathBuf {
            self.0.join("tint")
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes the `tint` pack into `dir`, drawing with `shader`.
    pub(crate) fn write(dir: &Path, shader: &str) {
        std::fs::create_dir_all(dir).expect("create pack dir");
        std::fs::write(dir.join("effect.toml"), MANIFEST).expect("write manifest");
        std::fs::write(dir.join("mul.wgsl"), shader).expect("write shader");
    }

    /// The `tint/mul` effect drawing with `shader`, loaded from a pack in `temp`.
    pub(crate) fn effect(temp: &Temp, shader: &str) -> super::PackEffect {
        write(&temp.pack(), shader);
        let pack = jstream_effects::Pack::load(&temp.pack(), jstream_render::FILTER_PRELUDE)
            .expect("the test pack loads");
        super::PackEffect::new(pack.effect(KIND).expect("tint/mul").clone())
    }

    /// The `tint/mul` kind's descriptor.
    pub(crate) fn descriptor() -> jstream_protocol::KindDescriptor {
        let temp = Temp::new("descriptor");
        write(&temp.pack(), MUL);
        let pack = jstream_effects::Pack::load(&temp.pack(), jstream_render::FILTER_PRELUDE)
            .expect("the test pack loads");
        pack.effect(KIND).expect("tint/mul").descriptor().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::Packs;
    use super::test_pack::{self, KIND, Temp};

    /// A poll reports the kinds a pack adds, changes and removes, and keeps an effect
    /// that didn't change, so the filters drawing it keep running.
    #[test]
    fn a_poll_describes_and_forgets_pack_kinds() {
        let temp = Temp::new("poll");
        let mut packs = Packs::new(temp.0.clone());
        assert!(packs.poll().is_none(), "an empty folder holds no packs");

        test_pack::write(&temp.pack(), test_pack::MUL);
        let added = packs.poll().expect("the pack loads");
        assert!(added.removed.is_empty());
        assert_eq!(
            added
                .described
                .iter()
                .map(|k| k.kind_id.as_str())
                .collect::<Vec<_>>(),
            [KIND]
        );
        let first = added.effects.get(KIND).cloned().expect("the effect");
        assert!(packs.poll().is_none(), "nothing changed");

        std::fs::write(temp.pack().join("mul.wgsl"), test_pack::BLUE).expect("edit");
        let changed = packs.poll().expect("the pack reloads");
        assert_eq!(changed.described.len(), 1);
        let second = changed.effects.get(KIND).cloned().expect("the effect");
        assert_ne!(first, second, "a reloaded effect is a new one");

        std::fs::remove_dir_all(temp.pack()).expect("remove the pack");
        let removed = packs.poll().expect("the pack is gone");
        assert_eq!(removed.removed, [KIND]);
        assert!(removed.described.is_empty() && removed.effects.is_empty());
    }

    /// A pack whose shader fails validation adds no kind.
    #[test]
    fn a_pack_that_fails_to_load_adds_nothing() {
        let temp = Temp::new("broken");
        test_pack::write(&temp.pack(), "@fragment fn fs(");
        let mut packs = Packs::new(temp.0.clone());
        assert!(packs.poll().is_none());
    }
}
