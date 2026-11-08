//! A keyframe's timing of one item (ADR-0129): the delay, easing and duration the item
//! takes in moves into that keyframe, each in place of the item's own or the preset's.
//! The Inspector shows it while a keyframe scene is shown, and a field left at "Item's"
//! or blank follows the item and the preset.

use jstream_control::protocol::{
    Bezier, Edit, ItemEasing, ItemId, KeyframeTiming, MAX_DELAY_MS, MAX_DURATION_MS, OverrideField,
    SceneId, SceneInfo,
};
use leptos::prelude::*;

use super::curve::CurveEditor;
use super::inspector::ITEM_EASINGS;
use super::number::NumberField;

/// `scene`'s timing of `item`, `None` when it sets none.
pub fn timing_of(scene: &SceneInfo, item: &ItemId) -> Option<KeyframeTiming> {
    scene
        .overrides
        .iter()
        .find(|o| o.item_id == *item)
        .and_then(|o| o.timing)
}

/// The easing select's value for no easing of the keyframe's own: the item's applies.
pub const OWN: &str = "";

/// The easing options, as `(value, label)`: the item's own first, then every item easing,
/// "Inherit" reading as the preset's, since it takes the preset's easing there.
pub fn easing_options() -> Vec<(&'static str, &'static str)> {
    std::iter::once((OWN, "Item's"))
        .chain(ITEM_EASINGS.iter().map(|(_, value, label)| {
            if *value == "inherit" {
                (*value, "Preset's")
            } else {
                (*value, *label)
            }
        }))
        .collect()
}

/// The option value of a keyframe's easing: [`OWN`] for none, and every curve the custom
/// one.
pub fn easing_value(easing: Option<ItemEasing>) -> &'static str {
    let Some(easing) = easing else {
        return OWN;
    };
    ITEM_EASINGS
        .iter()
        .find(|(v, ..)| {
            *v == easing
                || matches!(
                    (v, easing),
                    (ItemEasing::CubicBezier(_), ItemEasing::CubicBezier(_))
                )
        })
        .map_or(OWN, |(_, n, _)| n)
}

/// `timing` with its easing set to the option `value`, or `None` for a value with no
/// option. Picking the custom curve again keeps its points; a new one starts from
/// [`Bezier::BACK_OUT`].
pub fn with_easing(timing: KeyframeTiming, value: &str) -> Option<KeyframeTiming> {
    if value == OWN {
        return Some(KeyframeTiming {
            easing: None,
            ..timing
        });
    }
    if easing_value(timing.easing) == value {
        return Some(timing);
    }
    let easing = ITEM_EASINGS.iter().find(|(_, n, _)| *n == value)?.0;
    Some(KeyframeTiming {
        easing: Some(easing),
        ..timing
    })
}

/// A whole number of milliseconds from a field, kept to `[0, max]`.
fn ms(value: f64, max: u32) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "kept to [0, max] first"
    )]
    let ms = value.round().clamp(0.0, f64::from(max)) as u32;
    ms
}

/// `timing` with its delay set to `value` milliseconds, kept to the engine's range.
pub fn with_delay(timing: KeyframeTiming, value: f64) -> KeyframeTiming {
    KeyframeTiming {
        delay_ms: Some(ms(value, MAX_DELAY_MS)),
        ..timing
    }
}

/// `timing` with its duration set to `value` milliseconds, kept to the engine's range.
pub fn with_duration(timing: KeyframeTiming, value: f64) -> KeyframeTiming {
    KeyframeTiming {
        duration_ms: Some(ms(value, MAX_DURATION_MS)),
        ..timing
    }
}

/// The edit that takes the keyframe's timing of an item from `before` to `next`:
/// `item.set_timing`, or `item.clear_override` of `timing` when `next` sets nothing.
/// `None` when nothing changes.
pub fn timing_edit(
    scene_id: &SceneId,
    item_id: &ItemId,
    before: Option<KeyframeTiming>,
    next: KeyframeTiming,
) -> Option<Edit> {
    if before.unwrap_or_default() == next {
        return None;
    }
    Some(if next.is_empty() {
        Edit::ItemClearOverride {
            scene_id: scene_id.clone(),
            item_id: item_id.clone(),
            field: OverrideField::Timing,
        }
    } else {
        Edit::ItemSetTiming {
            scene_id: scene_id.clone(),
            item_id: item_id.clone(),
            timing: next,
        }
    })
}

/// A short summary for a folded section: what the keyframe sets, or nothing.
pub fn timing_hint(timing: Option<KeyframeTiming>) -> String {
    let Some(t) = timing.filter(|t| !t.is_empty()) else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some(ms) = t.delay_ms {
        parts.push(format!("delay {ms} ms"));
    }
    if t.easing.is_some() {
        parts.push("easing".to_owned());
    }
    if let Some(ms) = t.duration_ms {
        parts.push(format!("{ms} ms"));
    }
    parts.join(", ")
}

/// The fields of a keyframe's timing of one item: Delay, Easing (with a custom curve's
/// points) and Duration, each blank or "Item's" until set, and Clear, which removes the
/// timing. `timing` is the keyframe's timing, `None` while none is set; `send` gets each
/// edit.
#[component]
pub fn KeyframeTimingFields(
    /// The keyframe's timing of the item.
    #[prop(into)]
    timing: Signal<Option<KeyframeTiming>>,
    /// The keyframe scene.
    scene_id: Signal<SceneId>,
    /// The item.
    item_id: Signal<ItemId>,
    /// Sends an edit.
    send: impl Fn(Edit) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let apply = move |next: KeyframeTiming| {
        let edit = timing_edit(
            &scene_id.get_untracked(),
            &item_id.get_untracked(),
            timing.get_untracked(),
            next,
        );
        edit.map(send).is_some()
    };
    let current = move || timing.get_untracked().unwrap_or_default();
    let delay = Signal::derive(move || {
        Some(f64::from(
            timing.get().and_then(|t| t.delay_ms).unwrap_or_default(),
        ))
    });
    let duration = Signal::derive(move || {
        Some(f64::from(
            timing.get().and_then(|t| t.duration_ms).unwrap_or_default(),
        ))
    });
    let curve = Signal::derive(move || match timing.get().and_then(|t| t.easing) {
        Some(ItemEasing::CubicBezier(c)) => Some(c),
        _ => None,
    });
    let send_curve = move |c: Bezier| {
        apply(KeyframeTiming {
            easing: Some(ItemEasing::CubicBezier(c)),
            ..current()
        })
    };
    let has = move |f: fn(&KeyframeTiming) -> bool| move || timing.get().is_some_and(|t| f(&t));
    let clear_field = move |f: fn(KeyframeTiming) -> KeyframeTiming| {
        move |_| {
            apply(f(current()));
        }
    };
    view! {
        <div class="field-grid keyframe-timing">
            <div class="timing-field" class:unset=move || !has(|t| t.delay_ms.is_some())()>
                <NumberField label="Delay" unit="ms" value=delay step=10.0 decimals=0 min=0.0
                    max=f64::from(MAX_DELAY_MS)
                    send=move |v| apply(with_delay(current(), v)) />
                <Show when=has(|t| t.delay_ms.is_some())>
                    <button class="link" title="Use the item's delay"
                        on:click=clear_field(|t| KeyframeTiming { delay_ms: None, ..t })>
                        "Item's"
                    </button>
                </Show>
            </div>
            <label class="select-field">
                <span class="field-label">"Easing"</span>
                <select
                    aria-label="Keyframe easing"
                    prop:value=move || easing_value(timing.get().and_then(|t| t.easing))
                    on:change=move |event| {
                        if let Some(next) = with_easing(current(), &event_target_value(&event)) {
                            apply(next);
                        }
                    }
                >
                    {easing_options()
                        .into_iter()
                        .map(|(value, name)| view! { <option value=value>{name}</option> })
                        .collect_view()}
                </select>
            </label>
            <div class="timing-field" class:unset=move || !has(|t| t.duration_ms.is_some())()>
                <NumberField label="Duration" unit="ms" value=duration step=10.0 decimals=0
                    min=0.0 max=f64::from(MAX_DURATION_MS)
                    send=move |v| apply(with_duration(current(), v)) />
                <Show when=has(|t| t.duration_ms.is_some())>
                    <button class="link" title="Use the preset's duration"
                        on:click=clear_field(|t| KeyframeTiming { duration_ms: None, ..t })>
                        "Preset's"
                    </button>
                </Show>
            </div>
        </div>
        <Show when=move || curve.with(Option::is_some)>
            <CurveEditor curve=curve send=send_curve />
        </Show>
        <Show when=move || timing.with(Option::is_some)>
            <div class="buttons">
                <button on:click=move |_| { apply(KeyframeTiming::default()); }>
                    "Clear timing"
                </button>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (SceneId, ItemId) {
        (SceneId::new("k"), ItemId::new("i"))
    }

    /// Setting a field sends `item.set_timing` whole; clearing the last one clears the
    /// override; an unchanged timing sends nothing.
    #[test]
    fn a_timing_is_set_whole_and_an_empty_one_clears_it() {
        let (scene, item) = ids();
        let set = with_delay(KeyframeTiming::default(), 240.4);
        assert_eq!(
            timing_edit(&scene, &item, None, set),
            Some(Edit::ItemSetTiming {
                scene_id: scene.clone(),
                item_id: item.clone(),
                timing: KeyframeTiming {
                    delay_ms: Some(240),
                    ..KeyframeTiming::default()
                },
            })
        );
        assert_eq!(timing_edit(&scene, &item, Some(set), set), None);
        assert_eq!(
            timing_edit(&scene, &item, Some(set), KeyframeTiming::default()),
            Some(Edit::ItemClearOverride {
                scene_id: scene.clone(),
                item_id: item.clone(),
                field: OverrideField::Timing,
            })
        );
        assert_eq!(
            timing_edit(&scene, &item, None, KeyframeTiming::default()),
            None
        );
    }

    /// Delay and duration are kept to the engine's 60 s.
    #[test]
    fn delay_and_duration_are_kept_to_the_engines_range() {
        let t = with_duration(with_delay(KeyframeTiming::default(), 90_000.0), -5.0);
        assert_eq!(t.delay_ms, Some(MAX_DELAY_MS));
        assert_eq!(t.duration_ms, Some(0));
    }

    /// "Item's" is no easing of the keyframe's own; a custom curve starts from the
    /// back-out curve and keeps its points when picked again.
    #[test]
    fn the_easing_select_reads_and_writes_a_keyframes_easing() {
        let t = KeyframeTiming::default();
        assert_eq!(easing_value(t.easing), OWN);
        let eased = with_easing(t, "ease_in").expect("an option");
        assert_eq!(eased.easing, Some(ItemEasing::EaseIn));
        assert_eq!(easing_value(eased.easing), "ease_in");
        assert_eq!(with_easing(eased, OWN).map(|t| t.easing), Some(None));
        let curve = with_easing(t, "cubic_bezier").expect("an option");
        assert_eq!(
            curve.easing,
            Some(ItemEasing::CubicBezier(Bezier::BACK_OUT))
        );
        let moved = KeyframeTiming {
            easing: Some(ItemEasing::CubicBezier(
                crate::panels::curve::with_point(
                    &Bezier::BACK_OUT,
                    crate::panels::curve::Point::X1,
                    0.5,
                )
                .expect("a change"),
            )),
            ..t
        };
        assert_eq!(with_easing(moved, "cubic_bezier"), Some(moved));
        assert_eq!(with_easing(t, "bounce"), None);
        assert_eq!(easing_options()[0], (OWN, "Item's"));
        assert!(easing_options().contains(&("inherit", "Preset's")));
    }

    /// The folded hint names what the keyframe sets.
    #[test]
    fn the_hint_names_what_the_keyframe_sets() {
        assert_eq!(timing_hint(None), "");
        let t = KeyframeTiming {
            delay_ms: Some(100),
            easing: Some(ItemEasing::Hold),
            duration_ms: None,
        };
        assert_eq!(timing_hint(Some(t)), "delay 100 ms, easing");
    }
}
