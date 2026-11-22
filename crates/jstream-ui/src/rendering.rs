//! The Settings dialog's 3D rendering page (`settings.rs`, ADR-0170 decision 1): one
//! switch per costly 3D effect, capping what every scene may draw. It reads the switches
//! from `studio.get` each time the page opens and sets each one with
//! `studio.set_rendering`; the engine keeps them on its machine, whatever collection is
//! open. An engine older than 0.136 reports none, and the page says it has none.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::app::SessionContext;
use jstream_control::Control;
use jstream_protocol::{
    Action, Call, CallResult, Query, QueryResult, RenderFeatures, RenderFeaturesChange,
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
}

impl Switch {
    /// Every switch, in the order the page lists them.
    pub const ALL: [Switch; 4] = [
        Switch::Shadows,
        Switch::AmbientOcclusion,
        Switch::VolumetricFog,
        Switch::FloorReflections,
    ];

    /// What the switch reads.
    pub fn label(self) -> &'static str {
        match self {
            Switch::Shadows => "Shadows",
            Switch::AmbientOcclusion => "Ambient occlusion",
            Switch::VolumetricFog => "Volumetric fog",
            Switch::FloorReflections => "Floor reflections",
        }
    }

    /// The line under it.
    pub fn note(self) -> &'static str {
        match self {
            Switch::Shadows => {
                "Lights set to cast shadows draw a shadow map each. Off, no light casts a shadow in any scene."
            }
            Switch::AmbientOcclusion => {
                "The Ambient occlusion filter darkens creases and corners. Off, it changes nothing in any scene."
            }
            Switch::VolumetricFog => {
                "The Volumetric fog filter draws light shafts through fog. Off, it changes nothing in any scene."
            }
            Switch::FloorReflections => {
                "The Floor reflection filter draws its scene a second time, mirrored. Off, it changes nothing in any scene."
            }
        }
    }

    /// Whether the switch is on in `features`.
    pub fn get(self, features: &RenderFeatures) -> bool {
        match self {
            Switch::Shadows => features.shadows,
            Switch::AmbientOcclusion => features.ambient_occlusion,
            Switch::VolumetricFog => features.volumetric_fog,
            Switch::FloorReflections => features.floor_reflections,
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
        }
        change
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
        let Some(before) = features.get_untracked() else {
            return;
        };
        let change = switch.change(on);
        features.set(Some(before.with(&change)));
        error.set(None);
        let Some(control) = control.get_value() else {
            return;
        };
        spawn_local(async move {
            if let Err(message) = set(&*control, change).await {
                features.try_set(Some(before));
                error.try_set(Some(message));
            }
        });
    };
    view! {
        <p class="settings-intro">
            "These switches apply to every scene at once, so a heavy scene can be lightened without editing it. Off, an effect is drawn nowhere, whatever a scene asks for; on again, each scene gets back what it asked for. All on until you change them."
        </p>
        <Show when=move || features.get().is_some()>
            {Switch::ALL.into_iter().map(|switch| view! {
                <label class="settings-check rendering-switch" data-switch=switch.label()>
                    <input
                        type="checkbox"
                        aria-label=switch.label()
                        prop:checked=move || features.get().is_some_and(|now| switch.get(&now))
                        on:change=move |event| toggle(switch, event_target_checked(&event))
                    />
                    <span>
                        <span class="settings-check-label">{switch.label()}</span>
                        <span class="settings-check-note">{switch.note()}</span>
                    </span>
                </label>
            }).collect_view()}
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
        assert_eq!(now(read(&control)), Ok(Some(RenderFeatures::ALL)), "all on");
        now(set(&control, Switch::Shadows.change(false))).expect("set");
        now(set(&control, Switch::VolumetricFog.change(false))).expect("set");
        let read = now(read(&control)).expect("read").expect("reported");
        assert_eq!(
            read,
            RenderFeatures {
                shadows: false,
                volumetric_fog: false,
                ..RenderFeatures::ALL
            },
            "each change keeps the other switches"
        );
        for switch in Switch::ALL {
            let on = switch.get(&RenderFeatures::ALL.with(&switch.change(false)));
            assert!(!on, "{switch:?} turns itself off");
        }
    }
}
