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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jstream_effects::{Effect, PackWatcher, ShaderSource};
use jstream_protocol::{KindDescriptor, ProtocolError, Settings};
use jstream_render::VideoFilter;
use serde::{Deserialize, Serialize};

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

/// One loaded shader source (ADR-0093), as a source's chain draws it: its program is
/// the chain's first filter. Two are equal only when they are the same load, as
/// [`PackEffect`]s are.
#[derive(Clone)]
pub(crate) struct PackSource(Arc<ShaderSource>);

impl PackSource {
    pub(crate) fn new(source: ShaderSource) -> Self {
        Self(Arc::new(source))
    }

    /// A render filter drawing this source's program with `settings`.
    pub(crate) fn video_filter(&self, settings: &Settings) -> VideoFilter {
        let program: Arc<dyn jstream_render::ExternalProgram> = self.0.clone();
        VideoFilter::program(self.0.kind_id(), program, settings)
    }

    /// Its kind's descriptor.
    #[cfg(test)]
    pub(crate) fn descriptor(&self) -> jstream_protocol::KindDescriptor {
        self.0.descriptor().clone()
    }

    /// Its `scene` parameters, in the order they fill the program's input slots.
    pub(crate) fn scene_params(&self) -> &[String] {
        self.0.scene_params()
    }
}

impl PartialEq for PackSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for PackSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PackSource")
            .field(&self.0.kind_id())
            .finish()
    }
}

/// The loaded shader sources, by kind ID (`<pack>/<source>`), as plans read them.
pub(crate) type PackSources = Arc<BTreeMap<String, PackSource>>;

/// The file in the packs folder that keeps the quarantine across restarts. The watcher
/// reads only sub-folders, so it is never taken for a pack.
pub(crate) const QUARANTINE_FILE: &str = "quarantine.json";

/// What a poll, a blame or a re-enable changed, for the engine to apply under the
/// collection lock.
#[derive(Debug)]
pub(crate) struct Update {
    /// Kinds whose pack is gone, or whose effect left its manifest.
    pub(crate) removed: Vec<String>,
    /// Kinds added or changed, with their new descriptors.
    pub(crate) described: Vec<KindDescriptor>,
    /// Every loaded effect that draws from now on: the quarantined ones left out.
    pub(crate) effects: PackEffects,
    /// Every loaded shader source that draws from now on, as `effects` holds effects.
    pub(crate) sources: PackSources,
    /// The kinds quarantined now, loaded or not, for `kind.list`.
    pub(crate) quarantined: BTreeSet<String>,
    /// Counts up with each update [`Packs`] makes. The engine installs each under the
    /// packs' lock, so in order, and ignores one older than the last it installed: a
    /// stale update can't put a blamed effect back (#976 round 2).
    pub(crate) seq: u64,
}

/// A device loss blamed on pack effects ([`Packs::blame`]).
#[derive(Debug)]
pub(crate) struct Blame {
    /// The drawn kinds the loss is blamed on, ordered by ID.
    pub(crate) blamed: Vec<String>,
    /// The kinds the loss quarantined, ordered by ID.
    pub(crate) quarantined: Vec<String>,
    /// What changed for the plans, when any kind was quarantined.
    pub(crate) update: Option<Update>,
}

/// The effects and packs a person has to re-enable (#349): an effect a GPU device loss
/// was blamed on, and a pack one of whose effects was blamed twice in one run. Kept in
/// [`QUARANTINE_FILE`], not in the collection, so it isn't undoable and survives a
/// collection change. A pack's files changing doesn't lift it: a pack author's loop bug
/// survives most edits. It follows the shader too (ADR-0097): the digests of what a
/// blamed kind drew quarantine any kind that draws one of them, under whatever pack or
/// kind ID.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Quarantine {
    /// Quarantined kinds, `<pack>/<effect>`.
    #[serde(default)]
    kinds: BTreeSet<String>,
    /// Quarantined packs, every effect of each.
    #[serde(default)]
    packs: BTreeSet<String>,
    /// The digests ([`digests`]) of the WGSL modules each quarantined kind drew, by kind.
    /// A file from before ADR-0097 has none; a kind gains them when its pack loads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    digests: BTreeMap<String, BTreeSet<String>>,
}

impl Quarantine {
    /// Whether `kind_id`, drawing the modules `drawn` digests, may not draw.
    fn holds(&self, kind_id: &str, drawn: &[String]) -> bool {
        self.kinds.contains(kind_id)
            || self.packs.contains(pack_of(kind_id))
            || self
                .digests
                .values()
                .any(|held| drawn.iter().any(|d| held.contains(d)))
    }
}

/// The hex BLAKE3 digest of each WGSL module `kind_id` draws, as validated: for an
/// effect, the filter prelude and its header and shader; for a shader source, each pass's
/// program prelude, header, `common` and pass text (ADR-0097 decision 6).
fn digests(effect: Option<&Effect>, source: Option<&ShaderSource>) -> Vec<String> {
    let module = |prelude: &str, wgsl: &str| {
        let mut hasher = blake3::Hasher::new();
        hasher.update(prelude.as_bytes());
        hasher.update(wgsl.as_bytes());
        hasher.finalize().to_hex().to_string()
    };
    let mut out: Vec<String> = effect
        .map(|e| module(jstream_render::FILTER_PRELUDE, e.wgsl()))
        .into_iter()
        .collect();
    if let Some(source) = source {
        out.extend(
            source
                .passes()
                .iter()
                .map(|pass| module(jstream_render::PROGRAM_PRELUDE, &pass.wgsl)),
        );
    }
    out
}

/// The pack of `kind_id`, `<pack>/<effect>`.
fn pack_of(kind_id: &str) -> &str {
    kind_id.split_once('/').map_or(kind_id, |(pack, _)| pack)
}

/// The engine's packs folder, the effects loaded from it, and their quarantine.
#[derive(Debug)]
pub(crate) struct Packs {
    watcher: PackWatcher,
    /// Every loaded effect, quarantined or not.
    loaded: BTreeMap<String, PackEffect>,
    /// Every loaded shader source, quarantined or not.
    sources: BTreeMap<String, PackSource>,
    quarantine: Quarantine,
    /// The digests of the modules each loaded kind draws ([`digests`]).
    drawn: BTreeMap<String, Vec<String>>,
    /// How many losses each kind was blamed for in this run.
    blamed: BTreeMap<String, u32>,
    /// The last [`Update::seq`] made.
    seq: u64,
}

impl Packs {
    /// Watches `root`, a folder of pack folders, with the quarantine its
    /// [`QUARANTINE_FILE`] keeps. Nothing loads until the first [`Self::poll`].
    pub(crate) fn new(root: PathBuf) -> Self {
        let quarantine = read_quarantine(&root.join(QUARANTINE_FILE));
        Self {
            watcher: PackWatcher::new(root, jstream_render::FILTER_PRELUDE),
            loaded: BTreeMap::new(),
            sources: BTreeMap::new(),
            quarantine,
            drawn: BTreeMap::new(),
            blamed: BTreeMap::new(),
            seq: 0,
        }
    }

    /// Loads, reloads and drops packs to match the folder. `None` when no kind changed.
    /// A pack that fails to load is logged; one that fails a reload keeps its last good
    /// version. A pack whose load panics (naga parsing or validating its WGSL, which is
    /// untrusted) is one that failed to load: the watcher catches the panic around that
    /// pack, so the others keep loading and a pack can't crash the engine.
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
        let loaded: BTreeMap<String, PackEffect> = self
            .watcher
            .packs()
            .flat_map(jstream_effects::Pack::effects)
            .map(|effect| {
                let id = effect.kind_id();
                let kept = self
                    .loaded
                    .get(id)
                    .filter(|_| !changed.contains(&id))
                    .cloned();
                (
                    id.to_owned(),
                    kept.unwrap_or_else(|| PackEffect::new(effect.clone())),
                )
            })
            .collect();
        let sources: BTreeMap<String, PackSource> = self
            .watcher
            .packs()
            .flat_map(jstream_effects::Pack::sources)
            .map(|source| {
                let id = source.kind_id();
                let kept = self
                    .sources
                    .get(id)
                    .filter(|_| !changed.contains(&id))
                    .cloned();
                (
                    id.to_owned(),
                    kept.unwrap_or_else(|| PackSource::new(source.clone())),
                )
            })
            .collect();
        let removed = self
            .kinds()
            .filter(|id| !loaded.contains_key(*id) && !sources.contains_key(*id))
            .cloned()
            .collect();
        let described = changed
            .iter()
            .filter_map(|id| {
                self.watcher
                    .effect(id)
                    .map(Effect::descriptor)
                    .or_else(|| self.watcher.source(id).map(ShaderSource::descriptor))
            })
            .cloned()
            .collect();
        self.loaded = loaded;
        self.sources = sources;
        self.drawn = self
            .kinds()
            .map(|id| {
                let effect = self.loaded.get(id).map(|e| &*e.0);
                let source = self.sources.get(id).map(|s| &*s.0);
                (id.clone(), digests(effect, source))
            })
            .collect();
        // A kind quarantined before ADR-0097 has no digests: it gains them the first
        // time its pack loads, so renaming it can't put it back on air.
        let unrecorded: Vec<String> = self
            .quarantine
            .kinds
            .iter()
            .filter(|id| !self.quarantine.digests.contains_key(*id))
            .filter(|id| self.drawn.contains_key(*id))
            .cloned()
            .collect();
        if !unrecorded.is_empty() {
            for id in unrecorded {
                let drawn = self.drawn[&id].iter().cloned().collect();
                self.quarantine.digests.insert(id, drawn);
            }
            self.save();
        }
        for change in &changes.kinds {
            tracing::info!(?change, "effect pack kind");
        }
        Some(self.update(removed, described))
    }

    /// Blames a GPU device loss on the pack kinds in `drawn`, the ones drawn in the
    /// frames before it: each loaded one is quarantined, and the whole pack of one
    /// blamed for a second loss in this run. Kinds no loaded pack has are not blamed:
    /// they drew nothing.
    pub(crate) fn blame(&mut self, drawn: &BTreeSet<String>) -> Blame {
        let blamed: Vec<String> = drawn
            .iter()
            .filter(|id| self.is_loaded(id))
            .cloned()
            .collect();
        let mut quarantined = BTreeSet::new();
        for kind_id in &blamed {
            let count = self.blamed.entry(kind_id.clone()).or_default();
            *count += 1;
            if *count >= 2 {
                let pack = pack_of(kind_id).to_owned();
                quarantined.extend(
                    self.kinds()
                        .filter(|id| pack_of(id) == pack && !self.holds(id))
                        .cloned(),
                );
                quarantined.insert(kind_id.clone());
                self.quarantine.packs.insert(pack);
            } else {
                quarantined.insert(kind_id.clone());
            }
            self.quarantine.kinds.insert(kind_id.clone());
            let drawn = self.drawn_by(kind_id).iter().cloned().collect();
            self.quarantine.digests.insert(kind_id.clone(), drawn);
        }
        let update = (!blamed.is_empty()).then(|| {
            self.save();
            tracing::warn!(
                ?blamed,
                ?quarantined,
                "GPU device loss blamed on effect-pack kinds; quarantined"
            );
            self.update(Vec::new(), Vec::new())
        });
        Blame {
            blamed,
            quarantined: quarantined.into_iter().collect(),
            update,
        }
    }

    /// Lifts the quarantine of `kind_id`, and of its pack, and every quarantine whose
    /// digests it draws (one blamed under another ID). `Ok(None)` when it wasn't
    /// quarantined. Fails with `invalid_argument` for a kind no loaded pack has and no
    /// quarantine names.
    pub(crate) fn enable(&mut self, kind_id: &str) -> Result<Option<Update>, ProtocolError> {
        if !self.holds(kind_id) {
            if self.is_loaded(kind_id) {
                return Ok(None);
            }
            return Err(ProtocolError::invalid_argument(
                "kind_id",
                format!("no effect pack has a kind `{kind_id}`, and none is quarantined"),
            ));
        }
        self.quarantine.kinds.remove(kind_id);
        self.quarantine.packs.remove(pack_of(kind_id));
        self.quarantine.digests.remove(kind_id);
        let drawn = self.drawn_by(kind_id).to_vec();
        let shared: Vec<String> = self
            .quarantine
            .digests
            .iter()
            .filter(|(_, held)| drawn.iter().any(|d| held.contains(d)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in shared {
            self.quarantine.digests.remove(&id);
            self.quarantine.kinds.remove(&id);
        }
        self.save();
        tracing::info!(kind_id, "quarantined effect re-enabled");
        Ok(Some(self.update(Vec::new(), Vec::new())))
    }

    /// Whether `kind_id` may not draw: quarantined by ID, by its pack, or by a digest of
    /// a module it draws.
    fn holds(&self, kind_id: &str) -> bool {
        self.quarantine.holds(kind_id, self.drawn_by(kind_id))
    }

    /// The digests of the modules loaded `kind_id` draws: none when it isn't loaded.
    fn drawn_by(&self, kind_id: &str) -> &[String] {
        self.drawn.get(kind_id).map_or(&[], Vec::as_slice)
    }

    /// Every loaded kind, effect or shader source, quarantined or not.
    fn kinds(&self) -> impl Iterator<Item = &String> {
        self.loaded.keys().chain(self.sources.keys())
    }

    /// Whether a loaded pack has `kind_id`, an effect or a shader source.
    fn is_loaded(&self, kind_id: &str) -> bool {
        self.loaded.contains_key(kind_id) || self.sources.contains_key(kind_id)
    }

    /// The update for the loaded effects and quarantine as they are now.
    fn update(&mut self, removed: Vec<String>, described: Vec<KindDescriptor>) -> Update {
        self.seq += 1;
        let effects = self
            .loaded
            .iter()
            .filter(|(id, _)| !self.holds(id))
            .map(|(id, effect)| (id.clone(), effect.clone()))
            .collect();
        let sources = self
            .sources
            .iter()
            .filter(|(id, _)| !self.holds(id))
            .map(|(id, source)| (id.clone(), source.clone()))
            .collect();
        let quarantined = self
            .quarantine
            .kinds
            .iter()
            .cloned()
            .chain(self.kinds().filter(|id| self.holds(id)).cloned())
            .collect();
        Update {
            removed,
            described,
            effects: Arc::new(effects),
            sources: Arc::new(sources),
            quarantined,
            seq: self.seq,
        }
    }

    /// Writes the quarantine to its file, logging a failure: it then lasts this run.
    fn save(&self) {
        let path = self.watcher.root().join(QUARANTINE_FILE);
        let written = serde_json::to_vec_pretty(&self.quarantine)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                std::fs::create_dir_all(self.watcher.root())?;
                crate::collection::write_atomically(&path, &bytes)
            });
        if let Err(error) = written {
            tracing::warn!(path = %path.display(), %error, "effect quarantine not saved; it lasts until the engine stops");
        }
    }
}

/// The quarantine `path` keeps: none when it is missing. One that can't be read is
/// logged and read as none, so a broken file never keeps every pack off.
fn read_quarantine(path: &Path) -> Quarantine {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            tracing::warn!(path = %path.display(), %error, "effect quarantine file unreadable; nothing is quarantined");
            Quarantine::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Quarantine::default(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "effect quarantine file unreadable; nothing is quarantined");
            Quarantine::default()
        }
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

    /// A shader source kind, `glow/ramp` (ADR-0093): 16 by 9, each frame a quarter
    /// brighter than the one before (its feedback), and showing the top-left texel of its
    /// `sky` scene where that has any alpha.
    pub(crate) const SOURCE_KIND: &str = "glow/ramp";

    const SOURCE_MANIFEST: &str = r##"
[pack]
id = "glow"
name = "Glow"

[[source]]
id = "ramp"
name = "Ramp"
version = 1
width = 16
height = 16

[[source.param]]
id = "sky"
label = "Sky"
type = "scene"

[[source.pass]]
id = "ramp"
shader = "ramp.wgsl"
inputs = ["ramp", "sky"]
"##;

    const RAMP: &str = "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n    let last = ramp(in.uv).r;\n    let s = sky(vec2<f32>(0.0));\n    let up = vec4<f32>(vec3<f32>(min(last + 0.25, 1.0)), 1.0);\n    return select(up, s, s.a > 0.0);\n}\n";

    /// A shader source that hears audio, `glow/ear`: white while its level and the band
    /// a 1 kHz tone falls in are up, black otherwise.
    pub(crate) const EAR_KIND: &str = "glow/ear";

    const EAR_MANIFEST: &str = r##"

[[source]]
id = "ear"
name = "Ear"
version = 1
width = 16
height = 16
audio = true

[[source.pass]]
id = "ear"
shader = "ear.wgsl"
inputs = ["audio"]
"##;

    const EAR: &str = "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n    let on = sys.level > 0.1 && audio_band(36) > 0.5;\n    return vec4<f32>(vec3<f32>(select(0.0, 1.0, on)), 1.0);\n}\n";

    /// The `glow/ear` source, loaded from a pack in `temp` beside `glow/ramp`.
    pub(crate) fn ear(temp: &Temp) -> super::PackSource {
        let dir = temp.0.join("glow");
        std::fs::create_dir_all(&dir).expect("create pack dir");
        std::fs::write(
            dir.join("effect.toml"),
            format!("{SOURCE_MANIFEST}{EAR_MANIFEST}"),
        )
        .expect("write manifest");
        std::fs::write(dir.join("ramp.wgsl"), RAMP).expect("write shader");
        std::fs::write(dir.join("ear.wgsl"), EAR).expect("write shader");
        let pack = jstream_effects::Pack::load(&dir, jstream_render::FILTER_PRELUDE)
            .expect("the test source pack loads");
        super::PackSource::new(pack.source(EAR_KIND).expect("glow/ear").clone())
    }

    /// The `glow/ramp` source, loaded from a pack in `temp`.
    pub(crate) fn source(temp: &Temp) -> super::PackSource {
        let dir = temp.0.join("glow");
        std::fs::create_dir_all(&dir).expect("create pack dir");
        std::fs::write(dir.join("effect.toml"), SOURCE_MANIFEST).expect("write manifest");
        std::fs::write(dir.join("ramp.wgsl"), RAMP).expect("write shader");
        let pack = jstream_effects::Pack::load(&dir, jstream_render::FILTER_PRELUDE)
            .expect("the test source pack loads");
        super::PackSource::new(pack.source(SOURCE_KIND).expect("glow/ramp").clone())
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
    use super::test_pack::{self, KIND, Temp};
    use super::{Packs, QUARANTINE_FILE};
    use std::collections::BTreeSet;

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

    /// A blamed effect is quarantined and left out of the drawn effects; a second loss
    /// blamed on it in one run quarantines its whole pack; the quarantine is kept in the
    /// packs folder, and `enable` lifts the effect's and its pack's.
    #[test]
    fn a_blamed_effect_is_quarantined_until_it_is_enabled() {
        use std::collections::BTreeSet;

        let temp = Temp::new("blame");
        test_pack::write(&temp.pack(), test_pack::MUL);
        let mut packs = Packs::new(temp.0.clone());
        let loaded = packs.poll().expect("the pack loads");
        assert!(loaded.effects.contains_key(KIND) && loaded.quarantined.is_empty());

        let none = packs.blame(&BTreeSet::new());
        assert!(
            none.blamed.is_empty() && none.update.is_none(),
            "no pack drew"
        );
        let unknown = packs.blame(&["tint/gone".to_owned()].into_iter().collect());
        assert!(unknown.blamed.is_empty(), "a kind no pack has drew nothing");

        let drawn: BTreeSet<String> = [KIND.to_owned()].into_iter().collect();
        let first = packs.blame(&drawn);
        assert_eq!(
            (first.blamed.as_slice(), first.quarantined.as_slice()),
            ([KIND.to_owned()].as_slice(), [KIND.to_owned()].as_slice())
        );
        let update = first.update.expect("the plans change");
        assert!(
            !update.effects.contains_key(KIND),
            "a quarantined effect doesn't draw"
        );
        assert!(update.quarantined.contains(KIND));

        let reread = Packs::new(temp.0.clone());
        assert_eq!(
            reread.quarantine, packs.quarantine,
            "kept in the packs folder"
        );
        assert!(packs.poll().is_none(), "the quarantine file is not a pack");

        assert!(packs.enable(KIND).expect("enable").is_some());
        assert!(
            packs.enable(KIND).expect("again").is_none(),
            "nothing to lift"
        );
        assert!(packs.enable("tint/gone").is_err());

        let second = packs.blame(&drawn);
        assert_eq!(second.quarantined, [KIND.to_owned()]);
        assert!(
            packs.quarantine.packs.contains("tint"),
            "blamed twice in one run"
        );
        let lifted = packs.enable(KIND).expect("enable").expect("lifted");
        assert!(lifted.effects.contains_key(KIND) && lifted.quarantined.is_empty());
    }

    /// A pack's shader source is a kind like its effects: described on load, drawn
    /// until a device loss is blamed on it, then quarantined, and forgotten with its
    /// pack.
    /// The `tint` pack's manifest and shader under another pack ID, `copy`.
    fn write_copy(temp: &Temp, shader: &str) {
        let dir = temp.0.join("copy");
        test_pack::write(&dir, shader);
        let manifest = std::fs::read_to_string(dir.join("effect.toml")).expect("manifest");
        std::fs::write(
            dir.join("effect.toml"),
            manifest.replacen("id = \"tint\"", "id = \"copy\"", 1),
        )
        .expect("rename the pack");
    }

    /// A quarantine follows the shader (ADR-0097): the same WGSL under another pack ID
    /// loads quarantined, another shader there doesn't, and re-enabling the copy lifts
    /// the original.
    #[test]
    fn a_blamed_shader_is_quarantined_under_another_id() {
        let temp = Temp::new("digest");
        test_pack::write(&temp.pack(), test_pack::MUL);
        let mut packs = Packs::new(temp.0.clone());
        packs.poll().expect("the pack loads");
        let drawn: BTreeSet<String> = [KIND.to_owned()].into_iter().collect();
        assert!(packs.blame(&drawn).update.is_some());
        assert_eq!(
            packs.quarantine.digests[KIND].len(),
            1,
            "one module, digested"
        );

        write_copy(&temp, test_pack::MUL);
        let copied = packs.poll().expect("the copy loads");
        assert!(copied.quarantined.contains("copy/mul"), "{copied:?}");
        assert!(!copied.effects.contains_key("copy/mul"));

        write_copy(&temp, test_pack::BLUE);
        let other = packs.poll().expect("the copy reloads");
        assert!(
            other.effects.contains_key("copy/mul"),
            "another shader draws"
        );
        write_copy(&temp, test_pack::MUL);
        packs.poll().expect("the copy reloads");

        let lifted = packs.enable("copy/mul").expect("enable").expect("lifted");
        assert!(lifted.effects.contains_key("copy/mul") && lifted.effects.contains_key(KIND));
        assert!(lifted.quarantined.is_empty() && packs.quarantine.digests.is_empty());
    }

    /// A quarantine file from before ADR-0097, which names kinds only, gains the digests
    /// of what they draw when their pack loads, and keeps them in the file.
    #[test]
    fn an_old_quarantine_gains_digests_when_its_pack_loads() {
        let temp = Temp::new("digest-old");
        test_pack::write(&temp.pack(), test_pack::MUL);
        std::fs::write(
            temp.0.join(QUARANTINE_FILE),
            format!(r#"{{"kinds":["{KIND}"],"packs":[]}}"#),
        )
        .expect("an old quarantine");
        let mut packs = Packs::new(temp.0.clone());
        let loaded = packs.poll().expect("the pack loads");
        assert!(loaded.quarantined.contains(KIND));
        assert_eq!(
            Packs::new(temp.0.clone()).quarantine.digests.len(),
            1,
            "saved"
        );
        write_copy(&temp, test_pack::MUL);
        let copied = packs.poll().expect("the copy loads");
        assert!(copied.quarantined.contains("copy/mul"));
    }

    #[test]
    fn a_shader_source_is_described_quarantined_and_forgotten() {
        use test_pack::SOURCE_KIND;

        let temp = Temp::new("source");
        let _ = test_pack::source(&temp);
        let mut packs = Packs::new(temp.0.clone());
        let loaded = packs.poll().expect("the pack loads");
        let described = &loaded.described;
        assert_eq!(described.len(), 1);
        assert_eq!(described[0].kind_id, SOURCE_KIND);
        assert_eq!(
            described[0].category,
            jstream_protocol::KindCategory::Source
        );
        assert!(loaded.sources.contains_key(SOURCE_KIND) && loaded.effects.is_empty());

        let drawn = [SOURCE_KIND.to_owned()].into_iter().collect();
        let blame = packs.blame(&drawn);
        assert_eq!(blame.quarantined, [SOURCE_KIND.to_owned()]);
        let update = blame.update.expect("the plans change");
        assert!(!update.sources.contains_key(SOURCE_KIND));
        assert!(update.quarantined.contains(SOURCE_KIND));
        assert_eq!(
            packs.quarantine.digests[SOURCE_KIND].len(),
            1,
            "one digest per pass"
        );
        let lifted = packs.enable(SOURCE_KIND).expect("enable").expect("lifted");
        assert!(lifted.sources.contains_key(SOURCE_KIND));

        std::fs::remove_dir_all(temp.0.join("glow")).expect("remove the pack");
        let removed = packs.poll().expect("the pack is gone");
        assert_eq!(removed.removed, [SOURCE_KIND]);
        assert!(removed.sources.is_empty());
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
