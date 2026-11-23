//! Models drawn in their scenes' passes (ADR-0191): a 3D model whose layer nothing has
//! to read is made ready when its box is drawn and shaded straight into the pass that
//! shows it, in place of a layer the size of the canvas that the pass then blends.
//!
//! A model keeps its layer, drawn as ADR-0107 decision 6 says, while its item has
//! effects or its source has filters, its scene's camera has depth of field, a transition
//! is running, its layer has fewer pixels than the canvas (ADR-0197), or the compositor
//! mixes encoded colour and the model has a blended primitive. Otherwise its draw is made ready
//! ([`jstream_render::Compositor::prepare_model`]) and each pass that shows it settles
//! how ([`fill_shown`]): the first pass of the frame carries it on its item, when the
//! item is fully opaque in the normal blend and no item before it in the pass draws the
//! same upload; any other pass has it drawn into its layer first, once a frame, and
//! places the layer. A model more than one pass showed in the last frame is drawn into
//! its layer up front, so it is shaded once however many passes show it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use jstream_media::{FrameSize, MediaTime};
use jstream_protocol::{BlendMode, ItemId, SceneCamera, SceneId};
use jstream_render::{Compositor, Image, Item, Placement, PreparedModel, ResourceCache};

use super::{Captures, GpuKey, LiveParams, Pass, Shot, layer_item, pass_camera};
use crate::chain::Chains;

/// A model item: its scene and itself.
pub(crate) type ModelItem = (SceneId, ItemId);

/// The model draws one compositor has made ready, and how its passes have shown them.
#[derive(Default)]
pub(crate) struct DirectModels {
    /// The draws made ready since the chains last began a frame.
    ready: BTreeMap<ModelItem, PreparedModel>,
    /// The frame before's, whose buffers this frame's draws take over.
    spare: BTreeMap<ModelItem, PreparedModel>,
    /// The models more than one pass showed in the frame before.
    crowded: BTreeSet<ModelItem>,
    shown: RefCell<Shown>,
    /// How many models were carried by a pass's item, and how many made-ready draws
    /// were drawn into their layer, for tests.
    #[cfg(test)]
    pub(crate) counts: std::cell::Cell<(u64, u64)>,
}

#[derive(Default)]
struct Shown {
    /// How many passes have shown each model since the chains last began a frame.
    passes: BTreeMap<ModelItem, u32>,
    /// The made-ready models drawn into their layer since then.
    layered: BTreeSet<ModelItem>,
    /// The models whose layer holds a picture, from this frame or an earlier one.
    pictured: BTreeSet<ModelItem>,
}

impl DirectModels {
    /// Starts a frame: nothing is ready, and no pass has shown a model.
    pub(crate) fn begin(&mut self) {
        self.spare = std::mem::take(&mut self.ready);
        let shown = self.shown.get_mut();
        self.crowded = std::mem::take(&mut shown.passes)
            .into_iter()
            .filter(|(_, passes)| *passes > 1)
            .map(|(model, _)| model)
            .collect();
        shown.layered.clear();
    }

    /// Whether `model` is better made ready than drawn into its layer now: no more than
    /// one pass showed it in the frame before.
    pub(crate) fn wanted(&self, model: &ModelItem) -> bool {
        !self.crowded.contains(model)
    }

    /// The draw made ready for `model` in the frame before, to make this frame's from.
    pub(crate) fn recycle(&mut self, model: &ModelItem) -> Option<PreparedModel> {
        self.spare.remove(model)
    }

    /// Keeps `prepared`, `model`'s draw made ready this frame, for the passes.
    pub(crate) fn keep(&mut self, model: ModelItem, prepared: PreparedModel) {
        self.ready.insert(model, prepared);
    }

    /// Records that `model`'s layer was drawn this frame, as ADR-0107 draws it.
    pub(crate) fn drew_layer(&mut self, model: &ModelItem) {
        self.shown.get_mut().pictured.insert(model.clone());
    }

    /// Whether `model`'s layer holds a picture no frame drew into it this frame, and
    /// forgets it: the caller empties the layer, so nothing that reads it shows an old
    /// frame's model.
    pub(crate) fn takes_old_picture(&mut self, model: &ModelItem) -> bool {
        self.shown.get_mut().pictured.remove(model)
    }

    /// Settles how a pass shows `model` through `item`, which places its layer `layer`
    /// after `earlier` on a canvas of `canvas`: returns the draw the item carries, or
    /// `None` when it places its layer, which is drawn first if this frame hasn't.
    fn place<'a>(
        &'a self,
        compositor: &mut Compositor,
        model: &ModelItem,
        (item, layer): (&Item<'_>, &Image),
        earlier: &[Item<'a>],
        canvas: FrameSize,
    ) -> Option<&'a PreparedModel> {
        let mut shown = self.shown.borrow_mut();
        let passes = shown.passes.entry(model.clone()).or_default();
        *passes += 1;
        let first = *passes == 1;
        let prepared = self.ready.get(model)?;
        if shown.layered.contains(model) {
            return None;
        }
        let carried = first
            && matches!(item.placement, Placement::Layer(_))
            && item.opacity == 1.0
            && item.blend == BlendMode::Normal
            && prepared.size() == (canvas.width(), canvas.height())
            && !earlier.iter().any(|earlier| {
                earlier
                    .model
                    .is_some_and(|other| other.shares_model(prepared))
            });
        if carried {
            #[cfg(test)]
            self.counts
                .set((self.counts.get().0 + 1, self.counts.get().1));
            return Some(prepared);
        }
        shown.layered.insert(model.clone());
        shown.pictured.insert(model.clone());
        #[cfg(test)]
        self.counts
            .set((self.counts.get().0, self.counts.get().1 + 1));
        if let Err(error) = compositor.draw_prepared(prepared, layer) {
            tracing::debug!(scene = %model.0, item = %model.1, %error, "model layer not drawn this frame");
        }
        None
    }
}

/// [`super::fill_items`] for a pass that draws `pass`'s colour: each model made ready
/// this frame is carried by its item or drawn into the layer its item places, as the
/// module says.
pub(super) fn fill_shown<'a>(
    items: &mut Vec<Item<'a>>,
    compositor: &mut Compositor,
    (cache, chains): (&'a ResourceCache<GpuKey>, &'a Chains),
    (shot, pass): (&Shot, &Pass),
    (at, canvas): (MediaTime, FrameSize),
    (live, captures): (&LiveParams, &dyn Captures),
) -> Option<SceneCamera> {
    let size = [canvas.width() as f32, canvas.height() as f32];
    let camera = pass_camera(chains, (shot, pass), at, (live, captures));
    items.clear();
    for layer in &pass.layers {
        let Some(mut item) = layer_item(layer, (cache, chains), shot, (at, size), live, camera)
        else {
            continue;
        };
        if let (true, GpuKey::ItemBox(scene, id)) =
            (layer.boxed.is_some_and(|boxed| boxed.model), &layer.image)
        {
            item.model = chains.direct.place(
                compositor,
                &(scene.clone(), id.clone()),
                (&item, item.image),
                items,
                canvas,
            );
        }
        items.push(item);
    }
    camera
}
