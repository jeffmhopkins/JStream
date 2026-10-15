//! What an applied batch touched, for undo conflict detection (ADR-0032).
//!
//! [`Graph::apply`](crate::Graph::apply) returns it with every batch. The engine's undo
//! history logs it and checks a step's touched set against other clients' later ones.

use jstream_protocol::{
    BusId, Edit, EncoderProfileId, ItemId, OutputId, SceneId, SourceId, SourceKind,
    TransitionPresetId,
};
use std::collections::BTreeSet;

/// An object of the collection, named by its ID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Object {
    Scene(SceneId),
    Item(ItemId),
    Source(SourceId),
    Output(OutputId),
    Encoder(EncoderProfileId),
    Transition(TransitionPresetId),
    Bus(BusId),
    /// One source's send into one bus.
    Send(SourceId, BusId),
}

/// A property an edit sets, on its object.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Prop {
    Name,
    /// A scene's role.
    Role,
    /// An item's placement: its legacy transform or its geometry (ADR-0022), which
    /// replace each other.
    Placement,
    Crop,
    Visible,
    /// An item's stacking position.
    Index,
    /// Which source an item places (`item.set_source`, ADR-0023).
    Source,
    /// One parameter of a source's settings.
    Setting(String),
    /// An output's publish target.
    Target,
    /// The encoder profile an output uses.
    EncoderChoice,
    /// An encoder profile's encoding.
    Encoding,
    /// A transition preset's kind, timing and settings: what a switch with it runs.
    Run,
    /// A scene's `transition_in` and `transition_out`, or an item's transition settings.
    Transitions,
    /// Whether a source's audio is open.
    AudioEnabled,
    /// When a source's audio is heard.
    AudioFollow,
    /// A source's or bus's fader, or a send's gain.
    Gain,
    /// A source's or bus's mute.
    Muted,
}

/// What a batch touched: computed from its edits and its inverse's, so a delete also
/// names what it removed (the inverse re-creates it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Touched {
    /// Objects created or deleted.
    exists: BTreeSet<Object>,
    /// Objects referred to without being changed (the scene and source of an item).
    uses: BTreeSet<Object>,
    /// Properties set.
    props: BTreeSet<(Object, Prop)>,
    /// Whether the scenes' display order was set.
    scene_order: bool,
    /// Whether the canvas size was set.
    canvas: bool,
    /// Whether the default transition preset was set.
    default_preset: bool,
    /// An edit this module doesn't know: it conflicts with everything.
    unknown: bool,
}

impl Touched {
    /// What `forward` and `inverse` (the edits that undo it) touch together.
    pub fn of(forward: &[Edit], inverse: &[Edit]) -> Self {
        let mut touched = Touched::default();
        for edit in forward.iter().chain(inverse) {
            touched.add(edit);
        }
        touched
    }

    fn add(&mut self, edit: &Edit) {
        match edit {
            Edit::SceneCreate { scene_id, .. } | Edit::SceneDelete { scene_id } => {
                self.exists.insert(Object::Scene(scene_id.clone()));
            }
            Edit::SceneRename { scene_id, .. } => {
                self.prop(Object::Scene(scene_id.clone()), Prop::Name);
            }
            Edit::SceneSetRole { scene_id, .. } => {
                self.prop(Object::Scene(scene_id.clone()), Prop::Role);
            }
            Edit::SceneReorder { .. } => self.scene_order = true,
            Edit::CanvasSetSize { .. } => self.canvas = true,
            Edit::ItemAdd {
                scene_id,
                item_id,
                source_id,
                ..
            } => {
                self.exists.insert(Object::Item(item_id.clone()));
                self.uses.insert(Object::Scene(scene_id.clone()));
                self.uses.insert(Object::Source(source_id.clone()));
            }
            Edit::ItemRemove { item_id } => {
                self.exists.insert(Object::Item(item_id.clone()));
            }
            Edit::ItemSetTransform { item_id, .. } | Edit::ItemSetGeometry { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Placement);
            }
            Edit::ItemSetCrop { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Crop);
            }
            Edit::ItemSetVisible { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Visible);
            }
            Edit::ItemReorder { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Index);
            }
            Edit::ItemSetSource { item_id, source_id } => {
                self.prop(Object::Item(item_id.clone()), Prop::Source);
                self.uses.insert(Object::Source(source_id.clone()));
            }
            // A move sets the item's position, in the scene it names; its inverse names
            // the scene it left.
            Edit::ItemMove {
                item_id, scene_id, ..
            } => {
                self.prop(Object::Item(item_id.clone()), Prop::Index);
                self.uses.insert(Object::Scene(scene_id.clone()));
            }
            Edit::SourceCreate {
                source_id, kind, ..
            } => {
                self.exists.insert(Object::Source(source_id.clone()));
                if let SourceKind::Scene { scene_id } = kind {
                    self.uses.insert(Object::Scene(scene_id.clone()));
                }
            }
            Edit::SourceRename { source_id, .. } => {
                self.prop(Object::Source(source_id.clone()), Prop::Name);
            }
            Edit::SourceSetSettings { source_id, patch } => {
                for param in patch.0.keys() {
                    self.prop(
                        Object::Source(source_id.clone()),
                        Prop::Setting(param.clone()),
                    );
                }
            }
            Edit::SourceDuplicate {
                source_id,
                new_source_id,
                ..
            } => {
                self.uses.insert(Object::Source(source_id.clone()));
                self.exists.insert(Object::Source(new_source_id.clone()));
            }
            Edit::SourceDelete { source_id } => {
                self.exists.insert(Object::Source(source_id.clone()));
            }
            Edit::OutputCreate { output_id, .. } | Edit::OutputDelete { output_id } => {
                self.exists.insert(Object::Output(output_id.clone()));
            }
            Edit::OutputSetTarget { output_id, .. } => {
                self.prop(Object::Output(output_id.clone()), Prop::Target);
            }
            Edit::OutputSetEncoder {
                output_id,
                profile_id,
            } => {
                self.prop(Object::Output(output_id.clone()), Prop::EncoderChoice);
                if let Some(profile_id) = profile_id {
                    self.uses.insert(Object::Encoder(profile_id.clone()));
                }
            }
            Edit::EncoderCreate { profile_id, .. } | Edit::EncoderDelete { profile_id } => {
                self.exists.insert(Object::Encoder(profile_id.clone()));
            }
            Edit::EncoderUpdate { profile_id, .. } => {
                let encoder = Object::Encoder(profile_id.clone());
                self.prop(encoder.clone(), Prop::Name);
                self.prop(encoder, Prop::Encoding);
            }
            Edit::TransitionCreate { preset_id, .. } | Edit::TransitionDelete { preset_id } => {
                self.exists.insert(Object::Transition(preset_id.clone()));
            }
            Edit::TransitionUpdate {
                preset_id, name, ..
            } => {
                let preset = Object::Transition(preset_id.clone());
                if name.is_some() {
                    self.prop(preset.clone(), Prop::Name);
                }
                self.prop(preset, Prop::Run);
            }
            Edit::TransitionSetDefault { preset_id } => {
                self.default_preset = true;
                self.uses.insert(Object::Transition(preset_id.clone()));
            }
            Edit::SceneSetTransitions {
                scene_id,
                transition_in,
                transition_out,
            } => {
                self.prop(Object::Scene(scene_id.clone()), Prop::Transitions);
                for id in [transition_in, transition_out].into_iter().flatten() {
                    self.uses.insert(Object::Transition(id.clone()));
                }
            }
            Edit::ItemSetTransition { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Transitions);
            }
            Edit::SourceSetAudio {
                source_id,
                enabled,
                follow,
                gain_db,
                muted,
            } => {
                let source = Object::Source(source_id.clone());
                let set = [
                    (enabled.is_some(), Prop::AudioEnabled),
                    (follow.is_some(), Prop::AudioFollow),
                    (gain_db.is_some(), Prop::Gain),
                    (muted.is_some(), Prop::Muted),
                ];
                for (_, prop) in set.into_iter().filter(|(set, _)| *set) {
                    self.prop(source.clone(), prop);
                }
            }
            Edit::BusCreate { bus_id, .. } | Edit::BusDelete { bus_id } => {
                self.exists.insert(Object::Bus(bus_id.clone()));
            }
            Edit::BusRename { bus_id, .. } => {
                self.prop(Object::Bus(bus_id.clone()), Prop::Name);
            }
            Edit::BusSetFader {
                bus_id,
                gain_db,
                muted,
            } => {
                let bus = Object::Bus(bus_id.clone());
                if gain_db.is_some() {
                    self.prop(bus.clone(), Prop::Gain);
                }
                if muted.is_some() {
                    self.prop(bus, Prop::Muted);
                }
            }
            // Setting a send creates it or sets its gain: the inverse of a new one is
            // `send.remove`, which names it as created or deleted.
            Edit::SendSet {
                source_id, bus_id, ..
            } => {
                self.prop(Object::Send(source_id.clone(), bus_id.clone()), Prop::Gain);
                self.uses.insert(Object::Source(source_id.clone()));
                self.uses.insert(Object::Bus(bus_id.clone()));
            }
            Edit::SendRemove { source_id, bus_id } => {
                self.exists
                    .insert(Object::Send(source_id.clone(), bus_id.clone()));
                self.uses.insert(Object::Source(source_id.clone()));
                self.uses.insert(Object::Bus(bus_id.clone()));
            }
            _ => self.unknown = true,
        }
    }

    /// Adds everything `other` touches.
    pub fn extend(&mut self, other: &Touched) {
        self.exists.extend(other.exists.iter().cloned());
        self.uses.extend(other.uses.iter().cloned());
        self.props.extend(other.props.iter().cloned());
        self.scene_order |= other.scene_order;
        self.canvas |= other.canvas;
        self.default_preset |= other.default_preset;
        self.unknown |= other.unknown;
    }

    fn prop(&mut self, object: Object, prop: Prop) {
        self.props.insert((object, prop));
    }

    /// Every object this touches in any way.
    fn objects(&self) -> impl Iterator<Item = &Object> {
        self.exists
            .iter()
            .chain(&self.uses)
            .chain(self.props.iter().map(|(object, _)| object))
    }

    /// Whether undoing or redoing one of these would overwrite the other: one creates or
    /// deletes an object the other touches at all, both set the same property, or both
    /// set the scenes' order, the canvas size or the default transition preset.
    pub fn conflicts_with(&self, other: &Touched) -> bool {
        self.unknown
            || other.unknown
            || (self.scene_order && other.scene_order)
            || (self.canvas && other.canvas)
            || (self.default_preset && other.default_preset)
            || other.objects().any(|o| self.exists.contains(o))
            || self.objects().any(|o| other.exists.contains(o))
            || !self.props.is_disjoint(&other.props)
    }
}

#[cfg(test)]
mod tests {
    use super::Touched;
    use jstream_protocol::{
        BusId, Edit, EncoderProfile, EncoderProfileId, ItemGeometry, ItemId, ItemTransform,
        OutputId, SceneId, SourceId,
    };

    fn add(scene: &str, item: &str, source: &str) -> Edit {
        Edit::ItemAdd {
            scene_id: SceneId::new(scene),
            item_id: ItemId::new(item),
            source_id: SourceId::new(source),
            transform: None,
            geometry: None,
        }
    }

    fn remove(item: &str) -> Edit {
        Edit::ItemRemove {
            item_id: ItemId::new(item),
        }
    }

    fn transform(item: &str) -> Edit {
        Edit::ItemSetTransform {
            item_id: ItemId::new(item),
            scene_id: None,
            transform: ItemTransform::IDENTITY,
        }
    }

    fn geometry(item: &str) -> Edit {
        Edit::ItemSetGeometry {
            item_id: ItemId::new(item),
            scene_id: None,
            geometry: ItemGeometry::covering_canvas(1920, 1080).expect("invariant: a valid canvas"),
        }
    }

    fn canvas() -> Edit {
        Edit::CanvasSetSize {
            width: 1280,
            height: 720,
        }
    }

    fn visible(item: &str) -> Edit {
        Edit::ItemSetVisible {
            item_id: ItemId::new(item),
            scene_id: None,
            visible: false,
        }
    }

    fn set_source(item: &str, source: &str) -> Edit {
        Edit::ItemSetSource {
            item_id: ItemId::new(item),
            source_id: SourceId::new(source),
        }
    }

    /// `item.set_source` sets the item's source and uses both sources (the inverse names
    /// the old one), so a step that deletes either source conflicts with it.
    #[test]
    fn set_source_conflicts_with_its_own_property_and_both_sources() {
        let retarget = Touched::of(&[set_source("i", "clone")], &[set_source("i", "cam")]);
        assert!(retarget.conflicts_with(&Touched::of(
            &[set_source("i", "other")],
            &[set_source("i", "cam")]
        )));
        assert!(!retarget.conflicts_with(&Touched::of(&[transform("i")], &[transform("i")])));
        for source in ["cam", "clone"] {
            let deleted = Touched::of(
                &[Edit::SourceDelete {
                    source_id: SourceId::new(source),
                }],
                &[],
            );
            assert!(retarget.conflicts_with(&deleted), "{source}");
        }
    }

    fn item_move(item: &str, scene: &str) -> Edit {
        Edit::ItemMove {
            item_id: ItemId::new(item),
            scene_id: SceneId::new(scene),
            index: 0,
        }
    }

    #[test]
    fn moves_and_roles_are_known_edits() {
        let moved = Touched::of(&[item_move("a", "g")], &[item_move("a", "s")]);
        assert!(!moved.unknown);
        // Another item's transform, elsewhere, doesn't conflict with the move.
        let other = Touched::of(&[transform("b")], &[transform("b")]);
        assert!(!moved.conflicts_with(&other));
        // Reordering the same item does, and so does deleting either scene.
        let reorder = Touched::of(
            &[Edit::ItemReorder {
                item_id: ItemId::new("a"),
                index: 1,
            }],
            &[],
        );
        assert!(moved.conflicts_with(&reorder));
        let delete = Touched::of(
            &[Edit::SceneDelete {
                scene_id: SceneId::new("g"),
            }],
            &[],
        );
        assert!(moved.conflicts_with(&delete));
        let role = |r| Edit::SceneSetRole {
            scene_id: SceneId::new("g"),
            role: r,
        };
        let grouped = Touched::of(
            &[role(jstream_protocol::SceneRole::Group)],
            &[role(jstream_protocol::SceneRole::Scene)],
        );
        assert!(!grouped.unknown);
        assert!(!grouped.conflicts_with(&other));
        assert!(grouped.conflicts_with(&grouped.clone()));
    }

    /// Encoder edits are known: a profile's update conflicts with another update of it
    /// and with deleting it, and pointing an output at a profile uses the profile.
    #[test]
    fn encoder_edits_conflict_only_on_their_profile_or_output() {
        let profile = |id: &str| EncoderProfileId::new(id);
        let update = |id: &str| Edit::EncoderUpdate {
            profile_id: profile(id),
            name: "HQ".into(),
            profile: EncoderProfile::DEFAULT,
        };
        let point = |output: &str, id: Option<&str>| Edit::OutputSetEncoder {
            output_id: OutputId::new(output),
            profile_id: id.map(profile),
        };
        let updated = Touched::of(&[update("hq")], &[update("hq")]);
        assert!(!updated.unknown);
        assert!(updated.conflicts_with(&updated.clone()));
        assert!(!updated.conflicts_with(&Touched::of(&[update("low")], &[update("low")])));
        assert!(!updated.conflicts_with(&Touched::of(&[transform("i")], &[transform("i")])));

        let pointed = Touched::of(&[point("yt", Some("hq"))], &[point("yt", None)]);
        assert!(!pointed.unknown);
        let deleted = Touched::of(
            &[Edit::EncoderDelete {
                profile_id: profile("hq"),
            }],
            &[Edit::EncoderCreate {
                profile_id: profile("hq"),
                name: "HQ".into(),
                profile: EncoderProfile::DEFAULT,
            }],
        );
        assert!(pointed.conflicts_with(&deleted));
        assert!(pointed.conflicts_with(&Touched::of(
            &[point("yt", None)],
            &[point("yt", Some("hq"))]
        )));
        assert!(!pointed.conflicts_with(&Touched::of(&[point("tw", None)], &[point("tw", None)])));
    }

    #[test]
    fn adding_items_to_one_scene_does_not_conflict() {
        let a = Touched::of(&[add("s", "a", "cam")], &[remove("a")]);
        let b = Touched::of(&[add("s", "b", "cam")], &[remove("b")]);
        assert!(!a.conflicts_with(&b));
    }

    #[test]
    fn the_same_property_conflicts_and_another_does_not() {
        let a = Touched::of(&[transform("i")], &[transform("i")]);
        assert!(a.conflicts_with(&Touched::of(&[transform("i")], &[transform("i")])));
        assert!(!a.conflicts_with(&Touched::of(&[visible("i")], &[visible("i")])));
        assert!(!a.conflicts_with(&Touched::of(&[transform("j")], &[transform("j")])));
    }

    #[test]
    fn deleting_an_object_conflicts_with_anything_on_it() {
        let moved = Touched::of(&[transform("i")], &[transform("i")]);
        let removed = Touched::of(&[remove("i")], &[add("s", "i", "cam")]);
        assert!(moved.conflicts_with(&removed));
        assert!(removed.conflicts_with(&moved));
        // Placing an item in a scene the step created.
        let created = Touched::of(
            &[Edit::SceneCreate {
                scene_id: SceneId::new("s"),
                name: "S".into(),
                role: None,
            }],
            &[Edit::SceneDelete {
                scene_id: SceneId::new("s"),
            }],
        );
        let placed = Touched::of(&[add("s", "j", "cam")], &[remove("j")]);
        assert!(created.conflicts_with(&placed));
    }

    /// `item.set_geometry` and `item.set_transform` replace the same placement, and two
    /// canvas sizes conflict; neither is an unknown edit that conflicts with everything.
    #[test]
    fn placements_and_canvas_sizes_conflict_only_with_their_own_kind() {
        let boxed = Touched::of(&[geometry("i")], &[transform("i")]);
        assert!(boxed.conflicts_with(&Touched::of(&[transform("i")], &[transform("i")])));
        assert!(!boxed.conflicts_with(&Touched::of(&[visible("i")], &[visible("i")])));
        assert!(!boxed.conflicts_with(&Touched::of(&[geometry("j")], &[geometry("j")])));
        let sized = Touched::of(&[canvas()], &[canvas()]);
        assert!(sized.conflicts_with(&Touched::of(&[canvas()], &[canvas()])));
        assert!(!sized.conflicts_with(&boxed));
    }

    /// Audio edits are known: faders conflict per property, a send with its own gain and
    /// with deleting its bus, and two sources' sends into one bus don't conflict.
    #[test]
    fn audio_edits_conflict_on_their_source_bus_or_send() {
        let db = |v| jstream_protocol::Finite32::new(v).expect("finite");
        let send = |source: &str, gain| Edit::SendSet {
            source_id: SourceId::new(source),
            bus_id: BusId::new("vod"),
            gain_db: db(gain),
        };
        let gain = |source: &str| Edit::SourceSetAudio {
            source_id: SourceId::new(source),
            enabled: None,
            follow: None,
            gain_db: Some(db(-3.0)),
            muted: None,
        };
        let mute = Edit::SourceSetAudio {
            source_id: SourceId::new("mic"),
            enabled: None,
            follow: None,
            gain_db: None,
            muted: Some(true),
        };
        let faded = Touched::of(&[gain("mic")], &[gain("mic")]);
        assert!(!faded.unknown);
        assert!(faded.conflicts_with(&faded.clone()));
        assert!(!faded.conflicts_with(&Touched::of(
            std::slice::from_ref(&mute),
            std::slice::from_ref(&mute)
        )));
        assert!(!faded.conflicts_with(&Touched::of(&[gain("aux")], &[gain("aux")])));

        let mic = Touched::of(&[send("mic", -3.0)], &[send("mic", 0.0)]);
        let aux = Touched::of(&[send("aux", -3.0)], &[send("aux", 0.0)]);
        assert!(!mic.unknown);
        assert!(mic.conflicts_with(&mic.clone()));
        assert!(!mic.conflicts_with(&aux));
        let removed = Touched::of(
            &[Edit::SendRemove {
                source_id: SourceId::new("mic"),
                bus_id: BusId::new("vod"),
            }],
            &[send("mic", -3.0)],
        );
        assert!(mic.conflicts_with(&removed));
        assert!(!aux.conflicts_with(&removed));
        let deleted = Touched::of(
            &[Edit::BusDelete {
                bus_id: BusId::new("vod"),
            }],
            &[Edit::BusCreate {
                bus_id: BusId::new("vod"),
                name: "VOD".into(),
            }],
        );
        assert!(mic.conflicts_with(&deleted));
        let renamed = Touched::of(
            &[Edit::BusRename {
                bus_id: BusId::new("vod"),
                name: "VOD".into(),
            }],
            &[],
        );
        assert!(!renamed.unknown);
        assert!(!renamed.conflicts_with(&mic));
    }
}
