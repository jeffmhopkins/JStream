//! The Settings dialog's 3D rendering page (`settings.rs`, ADR-0170 decision 1): one
//! switch per costly 3D effect, capping what every scene may draw. It reads the switches
//! from `studio.get` each time the page opens and sets each one with
//! `studio.set_rendering`; the engine keeps them on its machine, whatever collection is
//! open. Since 0.165 it also has the reflection probes' switch and the face size they
//! capture at, at most (ADR-0170 decision 3), and since 0.172 the screen-space
//! reflections' switch and the steps their rays take (decision 4), and since 0.180 how
//! many pixels 3D models are drawn with (ADR-0197). An engine older than
//! the version that added one reports it as on (screen-space reflections as off,
//! ADR-0185), at the largest size or the fewest steps (models as Automatic, where it
//! draws them in full), and changing it does nothing there. An engine older than 0.147 reports no switches,
//! and the page says it has none.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::app::SessionContext;
use crate::dialog::Info;
use jstream_control::Control;
use jstream_protocol::{
    Action, Call, CallResult, MODEL_RESOLUTIONS, PROBE_RESOLUTIONS, Query, QueryResult,
    RenderFeatures, RenderFeaturesChange, SCREEN_SPACE_STEPS,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// One switch on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    /// Shadow maps.
    Shadows,
    /// The ambient occlusion filter.
    AmbientOcclusion,
    /// The volumetric fog filter.
    VolumetricFog,
    /// The floor reflection filter.
    FloorReflections,
    /// Reflection probes.
    ReflectionProbes,
    /// Screen-space reflections.
    ScreenSpaceReflections,
}

impl Switch {
    /// Every switch, in the order the page lists them.
    pub const ALL: [Switch; 6] = [
        Switch::Shadows,
        Switch::AmbientOcclusion,
        Switch::VolumetricFog,
        Switch::FloorReflections,
        Switch::ReflectionProbes,
        Switch::ScreenSpaceReflections,
    ];

    /// What the switch reads.
    pub fn label(self) -> &'static str {
        match self {
            Switch::Shadows => "Shadows",
            Switch::AmbientOcclusion => "Ambient occlusion",
            Switch::VolumetricFog => "Volumetric fog",
            Switch::FloorReflections => "Floor reflections",
            Switch::ReflectionProbes => "Reflection probes",
            Switch::ScreenSpaceReflections => "Screen-space reflections",
        }
    }

    /// The one line under it.
    pub fn help(self) -> &'static str {
        match self {
            Switch::Shadows => "Lights set to cast shadows draw them.",
            Switch::AmbientOcclusion => "The Ambient occlusion filter darkens creases.",
            Switch::VolumetricFog => "The Volumetric fog filter draws light shafts.",
            Switch::FloorReflections => "The Floor reflection filter mirrors its scene.",
            Switch::ReflectionProbes => {
                "Surfaces in a reflection probe's box reflect what it sees."
            }
            Switch::ScreenSpaceReflections => {
                "Surfaces in a scene with reflections reflect what is on screen."
            }
        }
    }

    /// What its (i) says: what turning it off saves.
    pub fn info(self) -> &'static str {
        match self {
            Switch::Shadows => {
                "Each light that casts shadows draws a shadow map. Off, no light casts a shadow in any scene."
            }
            Switch::AmbientOcclusion => "Off, the filter changes nothing in any scene.",
            Switch::VolumetricFog => "Off, the filter changes nothing in any scene.",
            Switch::FloorReflections => {
                "It draws its scene a second time. Off, it changes nothing in any scene, and the scene is drawn once."
            }
            Switch::ReflectionProbes => {
                "Each capture draws its scene six times, as often as the probe refreshes. Off, no probe captures, and surfaces reflect only the environment."
            }
            Switch::ScreenSpaceReflections => {
                "Each frame draws its scene's models once more and traces a ray per pixel, which can cost a modest GPU its frame rate. Off until turned on, surfaces reflect only their probes and the environment."
            }
        }
    }

    /// The id of its toggle, for its label.
    pub fn id(self) -> &'static str {
        match self {
            Switch::Shadows => "setting-rendering-shadows",
            Switch::AmbientOcclusion => "setting-rendering-ambient-occlusion",
            Switch::VolumetricFog => "setting-rendering-volumetric-fog",
            Switch::FloorReflections => "setting-rendering-floor-reflections",
            Switch::ReflectionProbes => "setting-rendering-reflection-probes",
            Switch::ScreenSpaceReflections => "setting-rendering-screen-space-reflections",
        }
    }

    /// Whether the switch is on in `features`.
    pub fn get(self, features: &RenderFeatures) -> bool {
        match self {
            Switch::Shadows => features.shadows,
            Switch::AmbientOcclusion => features.ambient_occlusion,
            Switch::VolumetricFog => features.volumetric_fog,
            Switch::FloorReflections => features.floor_reflections,
            Switch::ReflectionProbes => features.reflection_probes,
            Switch::ScreenSpaceReflections => features.screen_space_reflections,
        }
    }

    /// The change that sets this switch to `on` and leaves the rest.
    pub fn change(self, on: bool) -> RenderFeaturesChange {
        let mut change = RenderFeaturesChange::default();
        match self {
            Switch::Shadows => change.shadows = Some(on),
            Switch::AmbientOcclusion => change.ambient_occlusion = Some(on),
            Switch::VolumetricFog => change.volumetric_fog = Some(on),
            Switch::FloorReflections => change.floor_reflections = Some(on),
            Switch::ReflectionProbes => change.reflection_probes = Some(on),
            Switch::ScreenSpaceReflections => change.screen_space_reflections = Some(on),
        }
        change
    }
}

/// The change that caps the probes' face size at `size` and leaves the switches.
pub fn probe_cap(size: u32) -> RenderFeaturesChange {
    RenderFeaturesChange {
        max_probe_resolution: Some(size),
        ..RenderFeaturesChange::default()
    }
}

/// The change that sets the steps a screen-space reflection's ray takes and leaves the
/// switches.
pub fn reflection_steps(steps: u32) -> RenderFeaturesChange {
    RenderFeaturesChange {
        screen_space_steps: Some(steps),
        ..RenderFeaturesChange::default()
    }
}

/// The change that sets how many pixels 3D models are drawn with (0 lets the engine
/// choose) and leaves the switches.
pub fn model_resolution(percent: u32) -> RenderFeaturesChange {
    RenderFeaturesChange {
        model_resolution: Some(percent),
        ..RenderFeaturesChange::default()
    }
}

/// What a model resolution's option reads.
fn model_resolution_label(percent: u32) -> String {
    match percent {
        0 => "Automatic".into(),
        100 => "Full".into(),
        percent => format!("{percent}%"),
    }
}

/// What the steps option reads.
fn steps_label(steps: u32) -> &'static str {
    match SCREEN_SPACE_STEPS.iter().position(|&n| n == steps) {
        Some(0) => "Low",
        Some(1) => "Medium",
        _ => "High",
    }
}

/// The engine's 3D rendering switches (`studio.get`), or `None` from an engine too old
/// to report them.
///
/// # Errors
/// The call's failure, as text.
pub async fn read(control: &dyn Control) -> Result<Option<RenderFeatures>, String> {
    let call = Call::Query {
        query: Query::StudioGet,
    };
    match control.call(call).await {
        Ok(response) => match response.result {
            CallResult::Query {
                result: QueryResult::StudioGet { rendering, .. },
            } => Ok(rendering),
            _ => Err("the engine answered `studio.get` with something else".into()),
        },
        Err(error) => Err(error.to_string()),
    }
}

/// Sets the switches `change` names (`studio.set_rendering`).
///
/// # Errors
/// The call's failure, as text.
pub async fn set(control: &dyn Control, change: RenderFeaturesChange) -> Result<(), String> {
    let call = Call::Action {
        action: Action::StudioSetRendering { change },
    };
    control
        .call(call)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The 3D rendering page: its switches, once the engine has reported them.
#[component]
pub fn RenderingSettings() -> impl IntoView {
    let session = use_context::<SessionContext>().and_then(|context| context.get());
    let features = RwSignal::new(None::<RenderFeatures>);
    let loaded = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    if let Some(session) = &session {
        let control = session.control.clone();
        spawn_local(async move {
            match read(&*control).await {
                Ok(read) => {
                    features.try_set(read);
                }
                Err(message) => {
                    error.try_set(Some(message));
                }
            }
            loaded.try_set(true);
        });
    }
    let control = StoredValue::new_local(session.map(|session| session.control));
    let toggle = move |switch: Switch, on: bool| {
        if features.get_untracked().is_none() {
            return;
        }
        let change = switch.change(on);
        features.update(|now| *now = now.map(|now| now.with(&change)));
        error.set(None);
        let Some(control) = control.get_value() else {
            return;
        };
        spawn_local(async move {
            if let Err(message) = set(&*control, change).await {
                // Put back only this switch, so a failure doesn't undo another one
                // changed while this call was out.
                let back = switch.change(!on);
                features.try_update(|now| *now = now.map(|now| now.with(&back)));
                error.try_set(Some(message));
            }
        });
    };
    let cap = move |size: u32| {
        let Some(was) = features.get_untracked().map(|now| now.max_probe_resolution) else {
            return;
        };
        let change = probe_cap(size);
        features.update(|now| *now = now.map(|now| now.with(&change)));
        error.set(None);
        let Some(control) = control.get_value() else {
            return;
        };
        spawn_local(async move {
            if let Err(message) = set(&*control, change).await {
                let back = probe_cap(was);
                features.try_update(|now| *now = now.map(|now| now.with(&back)));
                error.try_set(Some(message));
            }
        });
    };
    let steps = move |chosen: u32| {
        let Some(was) = features.get_untracked().map(|now| now.screen_space_steps) else {
            return;
        };
        let change = reflection_steps(chosen);
        features.update(|now| *now = now.map(|now| now.with(&change)));
        error.set(None);
        let Some(control) = control.get_value() else {
            return;
        };
        spawn_local(async move {
            if let Err(message) = set(&*control, change).await {
                let back = reflection_steps(was);
                features.try_update(|now| *now = now.map(|now| now.with(&back)));
                error.try_set(Some(message));
            }
        });
    };
    let models = move |chosen: u32| {
        let Some(was) = features.get_untracked().map(|now| now.model_resolution) else {
            return;
        };
        let change = model_resolution(chosen);
        features.update(|now| *now = now.map(|now| now.with(&change)));
        error.set(None);
        let Some(control) = control.get_value() else {
            return;
        };
        spawn_local(async move {
            if let Err(message) = set(&*control, change).await {
                let back = model_resolution(was);
                features.try_update(|now| *now = now.map(|now| now.with(&back)));
                error.try_set(Some(message));
            }
        });
    };
    view! {
        <Show when=move || features.get().is_some()>
            <section class="prop-section">
                <h4 class="eyebrow">"Effects"</h4>
                <div class="prop-grid">
                    {Switch::ALL.into_iter().map(|switch| view! {
                        <div class="prop-row rendering-switch" data-switch=switch.label()>
                            <label class="prop-label check" for=switch.id()>{switch.label()}</label>
                            <div class="prop-control">
                                <input
                                    type="checkbox"
                                    class="toggle"
                                    id=switch.id()
                                    aria-label=switch.label()
                                    prop:checked=move || features.get().is_some_and(|now| switch.get(&now))
                                    on:change=move |event| toggle(switch, event_target_checked(&event))
                                />
                                <Info text=switch.info() />
                                <p class="help">{switch.help()}</p>
                            </div>
                        </div>
                    }).collect_view()}
                    <div class="prop-row rendering-probe-cap">
                        <label class="prop-label" for="setting-rendering-probe-resolution">
                            "Probe resolution"
                            <Info text="The most pixels a side each of a probe's six views is captured at. A probe set larger captures at this; lower draws faster and blurs its reflections." />
                        </label>
                        <div class="prop-control">
                            <select
                                id="setting-rendering-probe-resolution"
                                aria-label="Probe resolution"
                                prop:value=move || features.get().map(|now| now.max_probe_resolution.to_string()).unwrap_or_default()
                                on:change=move |event| {
                                    if let Ok(chosen) = event_target_value(&event).parse::<u32>() {
                                        cap(chosen);
                                    }
                                }
                            >
                                {PROBE_RESOLUTIONS.into_iter().map(|size| view! {
                                    <option value=size.to_string()>{format!("Up to {size} px")}</option>
                                }).collect_view()}
                            </select>
                            <p class="help">"The sharpest a reflection probe captures."</p>
                        </div>
                    </div>
                    <div class="prop-row rendering-reflection-steps">
                        <label class="prop-label" for="setting-rendering-reflection-steps">
                            "Reflection quality"
                            <Info text="How many steps a screen-space reflection's ray takes across the screen. More finds thinner things and fewer gaps; fewer draws faster." />
                        </label>
                        <div class="prop-control">
                            <select
                                id="setting-rendering-reflection-steps"
                                aria-label="Reflection quality"
                                prop:value=move || features.get().map(|now| now.screen_space_steps.to_string()).unwrap_or_default()
                                on:change=move |event| {
                                    if let Ok(chosen) = event_target_value(&event).parse::<u32>() {
                                        steps(chosen);
                                    }
                                }
                            >
                                {SCREEN_SPACE_STEPS.into_iter().map(|count| view! {
                                    <option value=count.to_string()>{format!("{} ({count} steps)", steps_label(count))}</option>
                                }).collect_view()}
                            </select>
                            <p class="help">"How closely a screen-space reflection looks."</p>
                        </div>
                    </div>
                    <div class="prop-row rendering-model-resolution">
                        <label class="prop-label" for="setting-rendering-model-resolution">
                            "3D model resolution"
                            <Info text="How many pixels 3D models are drawn with, across and down. Automatic draws them in full and with fewer only while the GPU can't keep up, so the outputs keep their frames; a size always draws them at that. Everything else keeps its pixels." />
                        </label>
                        <div class="prop-control">
                            <select
                                id="setting-rendering-model-resolution"
                                aria-label="3D model resolution"
                                prop:value=move || features.get().map(|now| now.model_resolution.to_string()).unwrap_or_default()
                                on:change=move |event| {
                                    if let Ok(chosen) = event_target_value(&event).parse::<u32>() {
                                        models(chosen);
                                    }
                                }
                            >
                                {MODEL_RESOLUTIONS.into_iter().map(|percent| view! {
                                    <option value=percent.to_string()>{model_resolution_label(percent)}</option>
                                }).collect_view()}
                            </select>
                            <p class="help">"Fewer pixels draw faster and look softer."</p>
                        </div>
                    </div>
                </div>
            </section>
        </Show>
        <Show when=move || loaded.get() && features.get().is_none() && error.get().is_none()>
            <p class="placeholder">"This engine has no 3D rendering switches."</p>
        </Show>
        {move || error.get().map(|message| view! {
            <p class="settings-error" role="alert">{format!("3D rendering: {message}")}</p>
        })}
    }
}

#[cfg(test)]
mod tests {
    use super::{Switch, read, set};
    use jstream_control::protocol::{Hello, PROTOCOL_VERSION, RenderFeatures};
    use jstream_control::{Control, MockEngine};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    #[test]
    fn a_switch_is_read_and_set_alone() {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        assert_eq!(
            now(read(&control)),
            Ok(Some(RenderFeatures::DEFAULT)),
            "the defaults"
        );
        now(set(&control, Switch::Shadows.change(false))).expect("set");
        now(set(&control, Switch::VolumetricFog.change(false))).expect("set");
        let read = now(read(&control)).expect("read").expect("reported");
        assert_eq!(
            read,
            RenderFeatures {
                shadows: false,
                volumetric_fog: false,
                ..RenderFeatures::DEFAULT
            },
            "each change keeps the other switches"
        );
        for switch in Switch::ALL {
            let on = switch.get(&RenderFeatures::ALL.with(&switch.change(false)));
            assert!(!on, "{switch:?} turns itself off");
        }
    }

    #[test]
    fn the_probe_cap_is_one_of_the_sizes() {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        now(set(&control, super::probe_cap(128))).expect("set");
        let read = now(read(&control)).expect("read").expect("reported");
        assert_eq!(read.max_probe_resolution, 128);
        assert!(read.reflection_probes, "the switches are kept");
        assert!(now(set(&control, super::probe_cap(300))).is_err());
    }

    #[test]
    fn the_reflection_steps_are_one_of_the_counts() {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        now(set(&control, super::reflection_steps(64))).expect("set");
        let read = now(read(&control)).expect("read").expect("reported");
        assert_eq!(read.screen_space_steps, 64);
        assert!(
            !read.screen_space_reflections,
            "the switch is kept, off by default"
        );
        assert!(now(set(&control, super::reflection_steps(20))).is_err());
        assert_eq!(super::steps_label(16), "Low");
        assert_eq!(super::steps_label(64), "High");
    }

    #[test]
    fn the_model_resolution_is_the_engines_choice_or_one_of_the_sizes() {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        let read_now = || now(read(&control)).expect("read").expect("reported");
        assert_eq!(read_now().model_resolution, 0, "the engine chooses");
        now(set(&control, super::model_resolution(75))).expect("set");
        assert_eq!(read_now().model_resolution, 75);
        assert!(now(set(&control, super::model_resolution(80))).is_err());
        assert_eq!(super::model_resolution_label(0), "Automatic");
        assert_eq!(super::model_resolution_label(100), "Full");
        assert_eq!(super::model_resolution_label(67), "67%");
    }
}
