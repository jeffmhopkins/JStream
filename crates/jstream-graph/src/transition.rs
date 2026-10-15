//! Transition presets and the transition settings of scenes and items (ADR-0029,
//! ADR-0037): the `transition.*` edits, `scene.set_transitions` and
//! `item.set_transition`.
//!
//! A preset's kind is a transition kind the graph's kinds describe, and its settings are
//! that kind's complete map, checked as a source's are. Exactly one preset is the default,
//! and it can't be deleted. Deleting another preset clears the scene overrides that name
//! it in the same edit, and the inverse restores them.

use crate::apply::{
    Kinds, already_exists, check_id, check_name, check_value, complete_settings_in, invalid,
    not_found,
};
use crate::model::{Preset, State};
use jstream_protocol::{
    Easing, Edit, EntityKind, ItemId, ItemTransition, KindCategory, KindDescriptor, ProtocolError,
    SceneId, Settings, TransitionPresetId, check_preset_timing,
};
use serde_json::Value;

/// Applies the transition edits and returns the edits that undo them. `None` for any
/// other edit.
pub(crate) fn apply(
    state: &mut State,
    kinds: &Kinds,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::TransitionCreate {
            preset_id,
            name,
            kind,
            duration_ms,
            easing,
            settings,
        } => create(
            state,
            kinds,
            Create {
                id: preset_id,
                name,
                kind,
                duration_ms: *duration_ms,
                easing: *easing,
                settings,
            },
            at,
        ),
        Edit::TransitionUpdate {
            preset_id,
            name,
            kind,
            duration_ms,
            easing,
            patch,
        } => update(
            state,
            kinds,
            Update {
                id: preset_id,
                name: name.as_deref(),
                kind: kind.as_deref(),
                duration_ms: *duration_ms,
                easing: *easing,
                patch,
            },
            at,
        ),
        Edit::TransitionDelete { preset_id } => delete(state, preset_id, at),
        Edit::TransitionSetDefault { preset_id } => set_default(state, preset_id),
        Edit::SceneSetTransitions {
            scene_id,
            transition_in,
            transition_out,
        } => set_scene(
            state,
            scene_id,
            transition_in.as_ref(),
            transition_out.as_ref(),
        ),
        Edit::ItemSetTransition {
            item_id,
            transition,
        } => set_item(state, item_id, transition, at),
        _ => return None,
    })
}

struct Create<'a> {
    id: &'a TransitionPresetId,
    name: &'a str,
    kind: &'a str,
    duration_ms: u32,
    easing: Easing,
    settings: &'a Settings,
}

struct Update<'a> {
    id: &'a TransitionPresetId,
    name: Option<&'a str>,
    kind: Option<&'a str>,
    duration_ms: Option<u32>,
    easing: Option<Easing>,
    patch: &'a Settings,
}

/// The transition kind `kind`, or `invalid_argument` on `field` when the graph has no
/// such kind or it is not a transition kind.
fn transition_kind<'k>(
    kinds: &'k Kinds,
    kind: &str,
    at: usize,
    field: &str,
) -> Result<&'k KindDescriptor, ProtocolError> {
    match kinds.get(kind) {
        Some(descriptor) if descriptor.category == KindCategory::Transition => Ok(descriptor),
        Some(_) => Err(invalid(
            at,
            field,
            format!("`{kind}` is not a transition kind; `kind.list` names them"),
        )),
        None => Err(invalid(
            at,
            field,
            format!("there is no kind `{kind}`; `kind.list` names them"),
        )),
    }
}

fn check_duration(duration_ms: u32, at: usize) -> Result<(), ProtocolError> {
    match check_preset_timing(duration_ms) {
        Some(reason) => Err(invalid(at, "duration_ms", reason)),
        None => Ok(()),
    }
}

fn find<'s>(
    state: &'s mut State,
    id: &TransitionPresetId,
) -> Result<&'s mut Preset, ProtocolError> {
    state
        .transitions
        .presets
        .get_mut(id)
        .ok_or_else(|| not_found(EntityKind::Transition, id.as_str()))
}

fn create(
    state: &mut State,
    kinds: &Kinds,
    edit: Create<'_>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    check_id(edit.id.as_str(), at, "preset_id")?;
    if state.transitions.presets.contains_key(edit.id) {
        return Err(already_exists(EntityKind::Transition, edit.id.as_str()));
    }
    check_name(edit.name, at)?;
    let descriptor = transition_kind(kinds, edit.kind, at, "kind")?;
    check_duration(edit.duration_ms, at)?;
    let settings = complete_settings_in(
        state,
        kinds,
        descriptor,
        None,
        edit.settings,
        at,
        "settings",
    )?;
    state.transitions.presets.insert(
        edit.id.clone(),
        Preset {
            id: edit.id.clone(),
            name: edit.name.to_owned(),
            kind: edit.kind.to_owned(),
            duration_ms: edit.duration_ms,
            easing: edit.easing,
            settings,
        },
    );
    Ok(vec![Edit::TransitionDelete {
        preset_id: edit.id.clone(),
    }])
}

fn update(
    state: &mut State,
    kinds: &Kinds,
    edit: Update<'_>,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let current = find(state, edit.id)?.clone();
    if let Some(name) = edit.name {
        check_name(name, at)?;
    }
    if let Some(duration_ms) = edit.duration_ms {
        check_duration(duration_ms, at)?;
    }
    let kind_changes = edit.kind.is_some_and(|kind| kind != current.kind);
    let (settings, inverse_kind, inverse_patch) = if kind_changes {
        // A new kind starts from its own defaults, overlaid by the patch; the inverse
        // goes back to the old kind with every old setting.
        let kind = edit.kind.unwrap_or(&current.kind);
        let descriptor = transition_kind(kinds, kind, at, "kind")?;
        let settings =
            complete_settings_in(state, kinds, descriptor, None, edit.patch, at, "patch")?;
        (
            settings,
            Some(current.kind.clone()),
            current.settings.clone(),
        )
    } else {
        let descriptor = transition_kind(kinds, &current.kind, at, "preset_id")?;
        let mut settings = current.settings.clone();
        let mut old = Settings::new();
        for (id, value) in edit.patch.iter() {
            let field = format!("patch.{id}");
            let param = descriptor.param(id).ok_or_else(|| {
                invalid(
                    at,
                    &field,
                    format!("is not a parameter of kind `{}`", descriptor.kind_id),
                )
            })?;
            let value = if value.is_null() && !param.is_reference() {
                param.default.clone()
            } else {
                check_value(state, kinds, None, param, value, at, &field)?;
                value.clone()
            };
            let previous = settings.insert(id, value);
            old.insert(id, previous.unwrap_or(Value::Null));
        }
        (settings, edit.kind.map(str::to_owned), old)
    };
    let preset = find(state, edit.id)?;
    let inverse = Edit::TransitionUpdate {
        preset_id: edit.id.clone(),
        name: edit.name.map(|_| current.name.clone()),
        kind: inverse_kind,
        duration_ms: edit.duration_ms.map(|_| current.duration_ms),
        easing: edit.easing.map(|_| current.easing),
        patch: inverse_patch,
    };
    if let Some(name) = edit.name {
        name.clone_into(&mut preset.name);
    }
    if let Some(kind) = edit.kind {
        kind.clone_into(&mut preset.kind);
    }
    if let Some(duration_ms) = edit.duration_ms {
        preset.duration_ms = duration_ms;
    }
    if let Some(easing) = edit.easing {
        preset.easing = easing;
    }
    preset.settings = settings;
    Ok(vec![inverse])
}

fn delete(
    state: &mut State,
    id: &TransitionPresetId,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    find(state, id)?;
    if &state.transitions.default == id {
        return Err(invalid(
            at,
            "preset_id",
            format!(
                "preset `{id}` is the default and can't be deleted; make another preset the \
                 default first"
            ),
        ));
    }
    let preset = state
        .transitions
        .presets
        .remove(id)
        .ok_or_else(|| not_found(EntityKind::Transition, id.as_str()))?;
    let mut inverse = vec![Edit::TransitionCreate {
        preset_id: preset.id,
        name: preset.name,
        kind: preset.kind,
        duration_ms: preset.duration_ms,
        easing: preset.easing,
        settings: preset.settings,
    }];
    for scene in &mut state.scenes {
        let names = |slot: &Option<TransitionPresetId>| slot.as_ref() == Some(id);
        if !names(&scene.transition_in) && !names(&scene.transition_out) {
            continue;
        }
        inverse.push(Edit::SceneSetTransitions {
            scene_id: scene.id.clone(),
            transition_in: scene.transition_in.clone(),
            transition_out: scene.transition_out.clone(),
        });
        for slot in [&mut scene.transition_in, &mut scene.transition_out] {
            if names(slot) {
                *slot = None;
            }
        }
    }
    Ok(inverse)
}

fn set_default(state: &mut State, id: &TransitionPresetId) -> Result<Vec<Edit>, ProtocolError> {
    find(state, id)?;
    let old = std::mem::replace(&mut state.transitions.default, id.clone());
    Ok(vec![Edit::TransitionSetDefault { preset_id: old }])
}

fn set_scene(
    state: &mut State,
    scene_id: &SceneId,
    transition_in: Option<&TransitionPresetId>,
    transition_out: Option<&TransitionPresetId>,
) -> Result<Vec<Edit>, ProtocolError> {
    let si = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    for id in [transition_in, transition_out].into_iter().flatten() {
        if !state.transitions.presets.contains_key(id) {
            return Err(not_found(EntityKind::Transition, id.as_str()));
        }
    }
    let scene = &mut state.scenes[si];
    let old_in = std::mem::replace(&mut scene.transition_in, transition_in.cloned());
    let old_out = std::mem::replace(&mut scene.transition_out, transition_out.cloned());
    Ok(vec![Edit::SceneSetTransitions {
        scene_id: scene_id.clone(),
        transition_in: old_in,
        transition_out: old_out,
    }])
}

fn set_item(
    state: &mut State,
    item_id: &ItemId,
    transition: &ItemTransition,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let (si, ii) = state
        .item_index(item_id)
        .ok_or_else(|| not_found(EntityKind::Item, item_id.as_str()))?;
    if let Some((field, reason)) = transition.problem() {
        return Err(invalid(at, &format!("transition.{field}"), reason));
    }
    let item = &mut state.scenes[si].items[ii];
    let old = std::mem::replace(&mut item.transition, *transition);
    Ok(vec![Edit::ItemSetTransition {
        item_id: item_id.clone(),
        transition: old,
    }])
}

#[cfg(test)]
mod tests {
    use crate::Graph;
    use jstream_protocol::{
        Easing, Edit, EntityKind, Glide, ItemId, ItemTransition, ProtocolError, Rev, SceneId,
        Settings, SourceId, SourceKind, TransitionPresetId, TransitionsInfo,
    };
    use serde_json::json;

    fn id(id: &str) -> TransitionPresetId {
        TransitionPresetId::new(id)
    }

    fn create(preset: &str, kind: &str) -> Edit {
        Edit::TransitionCreate {
            preset_id: id(preset),
            name: preset.to_uppercase(),
            kind: kind.into(),
            duration_ms: 600,
            easing: Easing::EaseInOut,
            settings: Settings::new(),
        }
    }

    fn overrides(scene: &str, into: Option<&str>, out: Option<&str>) -> Edit {
        Edit::SceneSetTransitions {
            scene_id: SceneId::new(scene),
            transition_in: into.map(id),
            transition_out: out.map(id),
        }
    }

    /// Applies `edits`, checks that their inverse restores the state before them, and
    /// re-applies them.
    fn round_trip(graph: &mut Graph, edits: &[Edit]) {
        let before = graph.state().clone();
        let applied = graph.apply(edits, None).expect("the edits apply");
        let after = graph.state().clone();
        graph
            .apply(&applied.inverse.edits, None)
            .expect("the inverse applies");
        assert_eq!(graph.state(), &before);
        graph.apply(edits, None).expect("the edits apply again");
        assert_eq!(graph.state(), &after);
    }

    fn scenes() -> Graph {
        let mut graph = Graph::new();
        let scene = |id: &str| Edit::SceneCreate {
            scene_id: SceneId::new(id),
            name: id.into(),
            role: None,
        };
        graph
            .apply(&[scene("a"), scene("b")], None)
            .expect("the scenes apply");
        graph
    }

    #[test]
    fn a_new_collection_has_one_cut_preset_as_its_default() {
        let graph = Graph::new();
        assert_eq!(graph.state().transitions_info(), TransitionsInfo::default());
        assert_eq!(graph.state().default_preset(), &id("cut"));
    }

    #[test]
    fn presets_take_their_kinds_defaults_and_check_settings() {
        let mut graph = Graph::new();
        round_trip(&mut graph, &[create("smooth", "move")]);
        let preset = graph.state().preset(&id("smooth")).expect("created");
        assert_eq!(preset.settings().get("unmatched"), Some(&json!("fade")));
        assert_eq!(preset.settings().get("stagger_ms"), Some(&json!(0)));

        let refused = |graph: &mut Graph, edit: Edit| graph.apply(&[edit], None).err();
        let field = |error: Option<ProtocolError>| match error {
            Some(ProtocolError::InvalidArgument { field, .. }) => field,
            other => format!("{other:?}"),
        };
        assert_eq!(
            field(refused(&mut graph, create("x", "wipe"))),
            "edits.0.kind"
        );
        let mut with_webcam =
            Graph::new().with_kinds([jstream_protocol::parity::kinds().remove(0)]);
        assert_eq!(
            field(refused(&mut with_webcam, create("x", "webcam"))),
            "edits.0.kind"
        );
        let mut long = create("x", "fade");
        if let Edit::TransitionCreate { duration_ms, .. } = &mut long {
            *duration_ms = 60_001;
        }
        assert_eq!(field(refused(&mut graph, long)), "edits.0.duration_ms");
        let mut bad = create("x", "fade");
        if let Edit::TransitionCreate { settings, .. } = &mut bad {
            settings.insert("stagger_ms", json!(-5));
        }
        assert_eq!(
            field(refused(&mut graph, bad)),
            "edits.0.settings.stagger_ms"
        );
        assert_eq!(
            refused(&mut graph, create("smooth", "fade")),
            Some(ProtocolError::AlreadyExists {
                entity: EntityKind::Transition,
                id: "smooth".into(),
            })
        );
    }

    #[test]
    fn a_source_cannot_use_a_transition_kind() {
        let refused = Graph::new()
            .apply(
                &[Edit::SourceCreate {
                    source_id: SourceId::new("s"),
                    name: "S".into(),
                    kind: SourceKind::Builtin {
                        kind: "fade".into(),
                    },
                    settings: Settings::new(),
                    duplicated_from: None,
                }],
                None,
            )
            .err();
        assert!(
            matches!(&refused, Some(ProtocolError::InvalidArgument { field, .. }) if field == "edits.0.kind.kind"),
            "{refused:?}"
        );
    }

    #[test]
    fn updates_patch_settings_and_a_new_kind_starts_from_its_defaults() {
        let mut graph = Graph::new();
        graph.apply(&[create("p", "move")], None).expect("created");
        round_trip(
            &mut graph,
            &[Edit::TransitionUpdate {
                preset_id: id("p"),
                name: Some("Springy".into()),
                kind: None,
                duration_ms: Some(900),
                easing: Some(Easing::Spring),
                patch: [("stagger_ms", json!(40))].into_iter().collect(),
            }],
        );
        let preset = graph.state().preset(&id("p")).expect("kept");
        assert_eq!(
            (preset.name(), preset.duration_ms(), preset.easing()),
            ("Springy", 900, Easing::Spring)
        );
        assert_eq!(preset.settings().get("stagger_ms"), Some(&json!(40)));

        // A cut has no parameters; going back to `fade` starts from its defaults.
        round_trip(
            &mut graph,
            &[
                Edit::TransitionUpdate {
                    preset_id: id("p"),
                    name: None,
                    kind: Some("cut".into()),
                    duration_ms: None,
                    easing: None,
                    patch: Settings::new(),
                },
                Edit::TransitionUpdate {
                    preset_id: id("p"),
                    name: None,
                    kind: Some("fade".into()),
                    duration_ms: None,
                    easing: None,
                    patch: [("unmatched", json!("scale"))].into_iter().collect(),
                },
            ],
        );
        let preset = graph.state().preset(&id("p")).expect("kept");
        assert_eq!(preset.kind(), "fade");
        assert_eq!(preset.duration_ms(), 900, "a kind change keeps the timing");
        assert_eq!(preset.settings().get("stagger_ms"), Some(&json!(0)));
        assert_eq!(preset.settings().get("unmatched"), Some(&json!("scale")));
    }

    #[test]
    fn the_default_cannot_be_deleted_and_a_delete_clears_overrides() {
        let mut graph = scenes();
        graph
            .apply(
                &[
                    create("fade", "fade"),
                    create("move", "move"),
                    overrides("a", Some("fade"), Some("move")),
                    overrides("b", None, Some("fade")),
                ],
                None,
            )
            .expect("applied");
        assert!(matches!(
            graph.apply(
                &[Edit::TransitionDelete {
                    preset_id: id("cut"),
                }],
                None,
            ),
            Err(ProtocolError::InvalidArgument { ref field, .. }) if field == "edits.0.preset_id"
        ));
        round_trip(
            &mut graph,
            &[Edit::TransitionDelete {
                preset_id: id("fade"),
            }],
        );
        let scene = |s: &str| graph.state().scene(&SceneId::new(s)).expect("kept").clone();
        assert_eq!(scene("a").transition_in(), None);
        assert_eq!(scene("a").transition_out(), Some(&id("move")));
        assert_eq!(scene("b").transition_out(), None);

        // Once another preset is the default, the initial one can go.
        round_trip(
            &mut graph,
            &[
                Edit::TransitionSetDefault {
                    preset_id: id("move"),
                },
                Edit::TransitionDelete {
                    preset_id: id("cut"),
                },
            ],
        );
        assert_eq!(graph.state().default_preset(), &id("move"));
        assert_eq!(graph.state().presets().len(), 1);
    }

    #[test]
    fn overrides_and_the_default_name_existing_presets() {
        let mut graph = scenes();
        let missing = Err(ProtocolError::NotFound {
            entity: EntityKind::Transition,
            id: "ghost".into(),
        });
        assert_eq!(
            graph
                .apply(&[overrides("a", None, Some("ghost"))], None)
                .map(|a| a.rev),
            missing
        );
        assert_eq!(
            graph
                .apply(
                    &[Edit::TransitionSetDefault {
                        preset_id: id("ghost"),
                    }],
                    None,
                )
                .map(|a| a.rev),
            missing
        );
        assert_eq!(graph.rev(), Rev(1));
    }

    #[test]
    fn item_settings_round_trip_and_follow_the_item() {
        let mut graph = scenes();
        graph
            .apply(
                &[
                    Edit::SourceCreate {
                        source_id: SourceId::new("nested"),
                        name: "B".into(),
                        kind: SourceKind::Scene {
                            scene_id: SceneId::new("b"),
                        },
                        settings: Settings::new(),
                        duplicated_from: None,
                    },
                    Edit::ItemAdd {
                        scene_id: SceneId::new("a"),
                        item_id: ItemId::new("i"),
                        source_id: SourceId::new("nested"),
                        transform: None,
                        geometry: None,
                    },
                    overrides("a", Some("cut"), None),
                ],
                None,
            )
            .expect("applied");
        let settings = ItemTransition {
            delay_ms: 150,
            glide: Glide::Never,
            ..ItemTransition::default()
        };
        round_trip(
            &mut graph,
            &[Edit::ItemSetTransition {
                item_id: ItemId::new("i"),
                transition: settings,
            }],
        );
        // Removing the item, and deleting its scene, restore them on undo.
        round_trip(
            &mut graph,
            &[Edit::ItemRemove {
                item_id: ItemId::new("i"),
            }],
        );
        graph
            .apply(
                &[Edit::ItemAdd {
                    scene_id: SceneId::new("a"),
                    item_id: ItemId::new("i"),
                    source_id: SourceId::new("nested"),
                    transform: None,
                    geometry: None,
                }],
                None,
            )
            .expect("re-added");
        graph
            .apply(
                &[Edit::ItemSetTransition {
                    item_id: ItemId::new("i"),
                    transition: settings,
                }],
                None,
            )
            .expect("set");
        let late = ItemTransition {
            delay_ms: 60_001,
            ..ItemTransition::default()
        };
        assert!(matches!(
            graph.apply(
                &[Edit::ItemSetTransition {
                    item_id: ItemId::new("i"),
                    transition: late,
                }],
                None,
            ),
            Err(ProtocolError::InvalidArgument { ref field, .. })
                if field == "edits.0.transition.delay_ms"
        ));
        round_trip(
            &mut graph,
            &[Edit::SceneDelete {
                scene_id: SceneId::new("a"),
            }],
        );
    }
}
