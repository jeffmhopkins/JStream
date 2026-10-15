//! A loaded pack: its manifest checked, every effect's WGSL validated, and each effect
//! ready as a `video_filter` kind.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use jstream_protocol::{KindCategory, KindDescriptor, Settings};

use crate::error::PackError;
use crate::manifest::{self, Manifest};
use crate::shader::{self, Layout};

/// The manifest's file name in a pack folder.
pub const MANIFEST: &str = "effect.toml";

/// The largest `effect.toml` read.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// The largest effect `.wgsl` file read.
pub const MAX_SHADER_BYTES: u64 = 256 * 1024;

/// One effect pack: a folder with an `effect.toml` and the WGSL files it names.
#[derive(Clone, Debug)]
pub struct Pack {
    id: String,
    name: String,
    description: Option<String>,
    dir: PathBuf,
    effects: Vec<Effect>,
}

impl Pack {
    /// Loads the pack in `dir`, validating every effect's WGSL against the host's
    /// `prelude` (see [`crate::shader`]). Any error fails the whole pack.
    pub fn load(dir: &Path, prelude: &str) -> Result<Self, PackError> {
        let manifest_path = dir.join(MANIFEST);
        let text = read(&manifest_path, MAX_MANIFEST_BYTES)?;
        let Manifest { pack, effects } = manifest::parse(&manifest_path, &text)?;
        let root = dir.canonicalize().map_err(|source| PackError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let mut loaded = Vec::with_capacity(effects.len());
        for effect in effects {
            let path = dir.join(&effect.shader);
            // The manifest's path rules keep it in the folder; this catches a symlink
            // that leads out of it.
            let real = path.canonicalize().map_err(|source| PackError::Io {
                path: path.clone(),
                source,
            })?;
            if !real.starts_with(&root) {
                return Err(PackError::Manifest {
                    path: manifest_path,
                    at: format!("effect.{}.shader", effect.id),
                    message: "leads outside the pack folder".into(),
                });
            }
            let source = read(&path, MAX_SHADER_BYTES)?;
            let params: Vec<_> = effect.params.iter().map(manifest::descriptor).collect();
            let layout = Layout::new(&params);
            let header = layout.header();
            shader::validate(prelude, &header, &source).map_err(|message| PackError::Shader {
                path: path.clone(),
                message,
            })?;
            let kind_id = format!("{}/{}", pack.id, effect.id);
            let descriptor = KindDescriptor {
                kind_id,
                category: KindCategory::VideoFilter,
                name: effect.name,
                version: effect.version,
                video: true,
                audio: false,
                params,
            };
            loaded.push(Effect {
                descriptor,
                wgsl: format!("{header}\n{source}").into(),
                layout,
                renames: effect.renames.into_iter().collect(),
                shader_path: path,
            });
        }
        Ok(Self {
            id: pack.id,
            name: pack.name,
            description: pack.description,
            dir: dir.to_owned(),
            effects: loaded,
        })
    }

    /// The pack ID, the first half of each of its kind IDs.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The manifest's description, if any.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The pack folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The effects, in manifest order.
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    /// The effect whose kind ID is `kind_id`.
    pub fn effect(&self, kind_id: &str) -> Option<&Effect> {
        self.effects.iter().find(|e| e.kind_id() == kind_id)
    }
}

/// One effect: a `video_filter` kind with the WGSL that draws it.
#[derive(Clone, Debug)]
pub struct Effect {
    descriptor: KindDescriptor,
    wgsl: Arc<str>,
    layout: Layout,
    renames: Vec<(String, String)>,
    shader_path: PathBuf,
}

impl Effect {
    /// The kind ID, `<pack>/<effect>`.
    pub fn kind_id(&self) -> &str {
        &self.descriptor.kind_id
    }

    /// The kind's descriptor (ADR-0025): a `video_filter`, every parameter live.
    pub fn descriptor(&self) -> &KindDescriptor {
        &self.descriptor
    }

    /// The WGSL the host appends to its prelude: the generated `Params` declaration,
    /// then the effect's file.
    pub fn wgsl(&self) -> &Arc<str> {
        &self.wgsl
    }

    /// Where each parameter sits in the `Params` uniform.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The `Params` uniform's bytes for `settings`, complete as the graph stores them.
    /// A missing or invalid value reads as its default.
    pub fn uniforms(&self, settings: &Settings) -> Vec<u8> {
        self.layout.write(&self.descriptor.params, settings)
    }

    /// The effect's `.wgsl` file.
    pub fn shader_path(&self) -> &Path {
        &self.shader_path
    }

    /// Migrates settings stored by version `from` of this kind (ADR-0025 › Versioning a
    /// kind): each declared rename moves its old ID's value to the new ID, unless the new
    /// ID already has one. Parameters added since take their defaults when the engine
    /// fills the map. `None` when `from` is newer than this version: the engine keeps
    /// those settings verbatim and reports them.
    pub fn migrate(&self, stored: &Settings, from: u32) -> Option<Settings> {
        if from > self.descriptor.version {
            return None;
        }
        let mut out = stored.clone();
        if from < self.descriptor.version {
            for (old, new) in &self.renames {
                if !out.0.contains_key(new)
                    && let Some(value) = out.0.remove(old)
                {
                    out.0.insert(new.clone(), value);
                }
            }
        }
        Some(out)
    }
}

impl jstream_render::ExternalFilter for Effect {
    fn label(&self) -> &str {
        self.kind_id()
    }

    fn wgsl(&self) -> &str {
        &self.wgsl
    }

    fn uniforms(&self, settings: &Settings) -> Vec<u8> {
        Effect::uniforms(self, settings)
    }
}

impl Effect {
    /// A render filter drawing this effect with `settings`, complete as the graph stores
    /// them. A filter made after a reload draws the new WGSL: the render side rebuilds
    /// the kind's pipeline when its source changes.
    pub fn video_filter(&self, settings: &Settings) -> jstream_render::VideoFilter {
        jstream_render::VideoFilter::external(self.kind_id(), Arc::new(self.clone()), settings)
    }
}

/// Reads a whole file of at most `limit` bytes as UTF-8.
fn read(path: &Path, limit: u64) -> Result<String, PackError> {
    let io = |source| PackError::Io {
        path: path.to_owned(),
        source,
    };
    let size = std::fs::metadata(path).map_err(io)?.len();
    if size > limit {
        return Err(PackError::TooLarge {
            path: path.to_owned(),
            size,
            limit,
        });
    }
    std::fs::read_to_string(path).map_err(io)
}
