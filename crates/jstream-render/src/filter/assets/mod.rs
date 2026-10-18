//! The filters that read assets and other scenes (roadmap V3): apply a LUT, image mask
//! and scene mask.
//!
//! Each reads one external image through [`Pass::set_reference`]: a LUT table made by
//! `lut::lut_image`, a decoded mask image, or the referenced scene's composited layer. Render
//! does no file or network I/O (R-RT-002), so these passes never open files: the chain
//! runtime (V0) resolves the asset or scene reference in the settings and hands over the
//! image. Until both a reference setting and an image are present, a kind passes its
//! input through unchanged.

#[expect(
    dead_code,
    reason = "the chain runtime loads LUT assets in V0 part 2 (#607), which makes the LUT API public"
)]
mod cube;
#[expect(
    dead_code,
    reason = "the chain runtime loads LUT assets in V0 part 2 (#607), which makes the LUT API public"
)]
mod lut;
mod mask;

use std::sync::Arc;

use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::Value;

use crate::error::RenderError;
use crate::filter::{Ctx, Pass};
use crate::image::Image;

/// This group's kinds.
pub(crate) fn descriptors() -> Vec<KindDescriptor> {
    vec![
        lut::descriptor(),
        mask::image_descriptor(),
        mask::scene_descriptor(),
    ]
}

/// The pass for one of this group's kinds, or `None` for any other kind.
pub(crate) fn build(kind_id: &str, settings: &Settings) -> Option<Box<dyn Pass>> {
    match kind_id {
        lut::KIND => Some(Box::new(lut::ApplyLut::new(settings))),
        mask::IMAGE_KIND => Some(Box::new(mask::Mask::new(mask::Source::Image, settings))),
        mask::SCENE_KIND => Some(Box::new(mask::Mask::new(mask::Source::Scene, settings))),
        _ => None,
    }
}

/// A live reference parameter (`null` by default) of type `ty`.
fn reference_param(id: &str, label: &str, ty: ParamType) -> ParamDescriptor {
    ParamDescriptor {
        id: id.into(),
        ty,
        default: Value::Null,
        label: label.into(),
        description: None,
        group: None,
        advanced: false,
        visible_if: None,
        live: true,
    }
}

/// A live asset parameter accepting the file extensions `accept`.
fn asset_param(id: &str, label: &str, accept: &[&str]) -> ParamDescriptor {
    reference_param(
        id,
        label,
        ParamType::Asset {
            accept: accept.iter().map(|&e| e.to_owned()).collect(),
        },
    )
}

/// The external image a pass reads, and whether its settings name one at all.
#[derive(Default)]
struct Reference {
    /// The setting is a reference, not `null`.
    named: bool,
    image: Option<Arc<Image>>,
}

impl Reference {
    /// Reads whether `settings` names a reference in `id`.
    fn update(&mut self, settings: &Settings, id: &str) {
        self.named = settings.0.get(id).is_some_and(|v| !v.is_null());
    }

    /// The image to read this frame, checked against `ctx`'s compositor and held for its
    /// submit (a frame hold is staged with the input's); `None` when the setting is `null`
    /// or no image was set.
    fn get(&self, ctx: &mut Ctx<'_>) -> Result<Option<&Image>, RenderError> {
        let Some(image) = self.image.as_deref().filter(|_| self.named) else {
            return Ok(None);
        };
        if !image.owner().is(ctx.maker.owner) {
            return Err(RenderError::Invalid {
                what: "image",
                detail: "the filter's reference image was made by another compositor".into(),
            });
        }
        ctx.hold(image);
        Ok(Some(image))
    }
}
