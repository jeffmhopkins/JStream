//! The files a source's video filters read (ADR-0026, roadmap V0): the table an
//! `apply_lut` filter applies, and the image an `image_mask` filter cuts by.
//!
//! `jstream-render` opens no files (R-RT-002), so its passes take these as reference
//! images. The live sources thread (`crate::live`) reads and decodes every asset a video
//! filter of the collection names, enabled or not, so enabling one needs no load, and
//! keeps the result until no filter names it. A plan hands each chain the
//! [`FilterAsset`] its filter names ([`crate::chain::Chain::of`]), and each compositor
//! that runs the chain makes its image once ([`reference_image`]). An asset that fails to
//! load is logged, and its filter passes its input through until the setting changes.
//!
//! Docs: crates/jstream-core/README.md#design, docs/adr/0026-source-filter-chains.md

use std::collections::HashMap;
use std::sync::Arc;

use jstream_protocol::{AssetRoot, Reference, Settings};
use jstream_render::{Compositor, Image, Lut, LutError, RenderError};
use jstream_sources::{Context, SourceError, StillImage, decode_still};

use crate::effect_packs::PackEffects;

/// What a filter reads its asset as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Use {
    /// A 3D lookup table: a `.cube` file, or a PNG of the tiled layout.
    Lut,
    /// An image whose alpha or luma is the mask.
    Mask,
}

/// One asset as a filter reads it: the same file read as a LUT and as a mask is two.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AssetKey {
    pub(crate) reads: Use,
    pub(crate) root: AssetRoot,
    pub(crate) path: String,
}

/// A loaded asset.
#[cfg_attr(test, derive(Clone))]
pub(crate) enum Loaded {
    Lut(Lut),
    Image(StillImage),
}

/// An asset ready for a chain, numbered so a compositor knows when to make its image
/// again.
pub(crate) struct FilterAsset {
    /// Unique per load: a reloaded file is a new version.
    pub(crate) version: u64,
    pub(crate) loaded: Loaded,
}

impl PartialEq for FilterAsset {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
    }
}

impl std::fmt::Debug for FilterAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match &self.loaded {
            Loaded::Lut(lut) => format!("lut {}", lut.size()),
            Loaded::Image(image) => format!("image {}x{}", image.width, image.height),
        };
        f.debug_struct("FilterAsset")
            .field("version", &self.version)
            .field("loaded", &what)
            .finish()
    }
}

/// What a plan's video filters read: the loaded assets, by key, and the effects of the
/// loaded effect packs ([`crate::effect_packs`]), by kind.
#[derive(Clone, Debug, Default)]
pub(crate) struct FilterAssets {
    images: HashMap<AssetKey, Arc<FilterAsset>>,
    effects: PackEffects,
}

impl FilterAssets {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// These assets with `effects` as the loaded pack effects.
    #[must_use]
    pub(crate) fn with_effects(self, effects: PackEffects) -> Self {
        Self { effects, ..self }
    }

    /// The loaded asset `key` names.
    pub(crate) fn get(&self, key: &AssetKey) -> Option<&Arc<FilterAsset>> {
        self.images.get(key)
    }

    /// Adds or replaces a loaded asset.
    #[cfg(test)]
    pub(crate) fn insert(&mut self, key: AssetKey, asset: Arc<FilterAsset>) {
        self.images.insert(key, asset);
    }

    /// The loaded effect packs' effects.
    pub(crate) fn effects(&self) -> &PackEffects {
        &self.effects
    }
}

impl FromIterator<(AssetKey, Arc<FilterAsset>)> for FilterAssets {
    fn from_iter<I: IntoIterator<Item = (AssetKey, Arc<FilterAsset>)>>(iter: I) -> Self {
        Self {
            images: iter.into_iter().collect(),
            effects: PackEffects::default(),
        }
    }
}

impl<const N: usize> From<[(AssetKey, Arc<FilterAsset>); N]> for FilterAssets {
    fn from(assets: [(AssetKey, Arc<FilterAsset>); N]) -> Self {
        assets.into_iter().collect()
    }
}

/// The asset a filter of `kind` with `settings` reads, or `None` when its kind reads
/// none or its setting is `null` (or not an asset, which the graph refuses).
pub(crate) fn wanted(kind: &str, settings: &Settings) -> Option<AssetKey> {
    let (reads, id) = match kind {
        "apply_lut" => (Use::Lut, "lut"),
        "image_mask" => (Use::Mask, "image"),
        _ => return None,
    };
    match Reference::from_value(settings.0.get(id)?)? {
        Reference::Asset { root, path } => Some(AssetKey { reads, root, path }),
        _ => None,
    }
}

/// Why an asset could not be loaded.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LoadError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("not a LUT: {0}")]
    Lut(#[from] LutError),
    #[error("a .cube file must be UTF-8 text")]
    NotText,
}

/// The PNG signature, which tells a tiled PNG table from a `.cube` file.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Reads and decodes `key` with `ctx`'s asset folders and limits. Blocks on I/O and
/// decoding: the live sources thread calls it.
pub(crate) fn load(key: &AssetKey, ctx: &Context) -> Result<Loaded, LoadError> {
    let bytes = ctx
        .assets
        .read(key.root, &key.path, ctx.limits.max_file_bytes)?;
    Ok(match key.reads {
        Use::Mask => Loaded::Image(decode_still(&bytes, &key.path, &ctx.limits)?),
        Use::Lut if bytes.starts_with(PNG) => {
            let image = decode_still(&bytes, &key.path, &ctx.limits)?;
            Loaded::Lut(Lut::from_tiled_rgba8(
                image.width,
                image.height,
                &image.rgba,
            )?)
        }
        Use::Lut => {
            let text = std::str::from_utf8(&bytes).map_err(|_| LoadError::NotText)?;
            Loaded::Lut(Lut::parse_cube(text)?)
        }
    })
}

/// The image a pass reads for `asset`, on `compositor`: the LUT's tile layout, or the
/// mask uploaded as straight sRGB.
///
/// # Errors
/// What the compositor reports for the upload.
pub(crate) fn reference_image(
    compositor: &mut Compositor,
    asset: &FilterAsset,
) -> Result<Image, RenderError> {
    match &asset.loaded {
        Loaded::Lut(lut) => jstream_render::lut_image(compositor, lut),
        Loaded::Image(image) => compositor.upload_rgba8(image.width, image.height, &image.rgba),
    }
}

#[cfg(test)]
mod tests {
    use jstream_protocol::{AssetRoot, Settings};
    use jstream_sources::{AssetRoots, Context};
    use serde_json::json;

    use super::{AssetKey, LoadError, Loaded, Use, load, wanted};

    fn settings(id: &str, value: serde_json::Value) -> Settings {
        [(id, value)].into_iter().collect()
    }

    fn asset(path: &str) -> serde_json::Value {
        json!({"type": "asset", "root": "collection", "path": path})
    }

    /// Only the reference kinds read an asset, each from its own setting.
    #[test]
    fn each_reference_kind_names_its_asset() {
        let key = |reads| AssetKey {
            reads,
            root: AssetRoot::Collection,
            path: "a.png".into(),
        };
        assert_eq!(
            wanted("apply_lut", &settings("lut", asset("a.png"))),
            Some(key(Use::Lut))
        );
        assert_eq!(
            wanted("image_mask", &settings("image", asset("a.png"))),
            Some(key(Use::Mask))
        );
        assert_eq!(wanted("image_mask", &settings("image", json!(null))), None);
        assert_eq!(wanted("image_mask", &Settings::new()), None);
        assert_eq!(
            wanted("scene_mask", &settings("scene", asset("a.png"))),
            None
        );
        assert_eq!(wanted("sharpen", &settings("lut", asset("a.png"))), None);
    }

    /// A `.cube` file is parsed as text, a bad one is an error, and a missing file is
    /// the source error the `image` kind gives.
    #[test]
    fn a_cube_file_loads_and_errors_are_kept() {
        let dir =
            std::env::temp_dir().join(format!("jstream-filter-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("id.cube"),
            "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n",
        )
        .expect("cube");
        std::fs::write(dir.join("bad.cube"), "LUT_3D_SIZE 2\n0 0 0\n").expect("cube");
        let ctx = Context {
            assets: AssetRoots {
                collection: Some(dir.clone()),
                app: None,
            },
            ..Context::default()
        };
        let key = |path: &str| AssetKey {
            reads: Use::Lut,
            root: AssetRoot::Collection,
            path: path.into(),
        };
        assert!(matches!(load(&key("id.cube"), &ctx), Ok(Loaded::Lut(lut)) if lut.size() == 2));
        assert!(matches!(
            load(&key("bad.cube"), &ctx),
            Err(LoadError::Lut(_))
        ));
        assert!(matches!(
            load(&key("gone.cube"), &ctx),
            Err(LoadError::Source(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
