//! "Warm scenes" on the Settings dialog's General page (`settings.rs`, ADR-0117 §7):
//! how many scenes besides the program the engine keeps warm, so showing one starts
//! nothing. The Preview scene is always one of them. It reads the number from
//! `studio.get` each time the page opens and sets it with `studio.set_warm`; the engine
//! keeps it on its machine, whatever collection is open. An engine older than 0.115
//! reports none, and the control hides. Beside it, "Warm scenes' GPU memory" sets the
//! limit on what the warm scenes are estimated to hold on the GPU (`gpu_mib`,
//! ADR-0150), hidden from an engine older than 0.117.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::app::SessionContext;
use jstream_control::Control;
use jstream_protocol::{Action, Call, CallResult, MAX_WARM_SCENES, Query, QueryResult};

/// The GPU-memory limits the choice offers, in MiB: 0 for no limit.
const GPU_CHOICES: [u32; 7] = [0, 512, 1024, 2048, 4096, 8192, 16_384];
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The engine's warm budget (`studio.get`), or `None` from an engine that reports none.
///
/// # Errors
/// The call's failure, as text.
pub async fn budget(control: &dyn Control) -> Result<Option<u32>, String> {
    Ok(read(control).await?.0)
}

/// The engine's warm budget and its GPU-memory limit in MiB (`studio.get`), each `None`
/// from an engine too old to report it.
///
/// # Errors
/// The call's failure, as text.
pub async fn read(control: &dyn Control) -> Result<(Option<u32>, Option<u32>), String> {
    let call = Call::Query {
        query: Query::StudioGet,
    };
    match control.call(call).await {
        Ok(response) => match response.result {
            CallResult::Query {
                result:
                    QueryResult::StudioGet {
                        warm_scenes,
                        warm_gpu_mib,
                        ..
                    },
            } => Ok((warm_scenes, warm_gpu_mib)),
            _ => Err("the engine answered `studio.get` with something else".into()),
        },
        Err(error) => Err(error.to_string()),
    }
}

/// Sets the engine's warm budget (`studio.set_warm`), and its GPU-memory limit when
/// `gpu_mib` names one.
///
/// # Errors
/// The call's failure, as text.
pub async fn set_budget(
    control: &dyn Control,
    scenes: u32,
    gpu_mib: Option<u32>,
) -> Result<(), String> {
    let call = Call::Action {
        action: Action::StudioSetWarm { scenes, gpu_mib },
    };
    control
        .call(call)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// What the GPU-memory choice for `mib` reads.
pub fn gpu_label(mib: u32) -> String {
    match mib {
        0 => "No limit".to_owned(),
        mib if mib % 1024 == 0 => format!("{} GB", mib / 1024),
        mib => format!("{mib} MB"),
    }
}

/// What the choice for `scenes` reads.
pub fn choice_label(scenes: u32) -> String {
    match scenes {
        0 => "Only the Preview scene".to_owned(),
        1 => "1 scene".to_owned(),
        n => format!("{n} scenes"),
    }
}

/// The "Warm scenes" choice, shown once the engine has reported its budget.
#[component]
pub fn WarmScenesSetting() -> impl IntoView {
    let session = use_context::<SessionContext>().and_then(|context| context.get());
    let scenes = RwSignal::new(None::<u32>);
    let gpu = RwSignal::new(None::<u32>);
    let error = RwSignal::new(None::<String>);
    if let Some(session) = &session {
        let control = session.control.clone();
        spawn_local(async move {
            match read(&*control).await {
                Ok((kept, limit)) => {
                    scenes.try_set(kept);
                    gpu.try_set(limit);
                }
                Err(message) => {
                    error.try_set(Some(message));
                }
            }
        });
    }
    let control = StoredValue::new_local(session.map(|session| session.control));
    let choose = move |chosen: u32| {
        let before = scenes.get_untracked();
        scenes.set(Some(chosen));
        error.set(None);
        let Some(control) = control.get_value() else {
            return;
        };
        spawn_local(async move {
            if let Err(message) = set_budget(&*control, chosen, None).await {
                scenes.try_set(before);
                error.try_set(Some(message));
            }
        });
    };
    let choose_gpu = move |chosen: u32| {
        let (Some(kept), before) = (scenes.get_untracked(), gpu.get_untracked()) else {
            return;
        };
        gpu.set(Some(chosen));
        error.set(None);
        let Some(control) = control.get_value() else {
            return;
        };
        spawn_local(async move {
            if let Err(message) = set_budget(&*control, kept, Some(chosen)).await {
                gpu.try_set(before);
                error.try_set(Some(message));
            }
        });
    };
    // The engine's limit is offered even when it isn't one of the usual ones.
    let gpu_choices = move || {
        let mut choices = GPU_CHOICES.to_vec();
        if let Some(current) = gpu.get()
            && !choices.contains(&current)
        {
            choices.push(current);
            choices.sort_unstable();
        }
        choices
    };
    view! {
        <Show when=move || scenes.get().is_some()>
            <label class="settings-field warm-scenes">
                <span class="settings-check-label">"Warm scenes"</span>
                <select
                    aria-label="Warm scenes"
                    prop:value=move || scenes.get().unwrap_or_default().to_string()
                    on:change=move |event| {
                        if let Ok(chosen) = event_target_value(&event).parse::<u32>() {
                            choose(chosen);
                        }
                    }
                >
                    {(0..=MAX_WARM_SCENES).map(|n| view! {
                        <option value=n.to_string()>{choice_label(n)}</option>
                    }).collect_view()}
                </select>
                <span class="settings-check-note">
                    "How many scenes besides the one on program stay ready: their videos, pages and cameras keep running, so showing one starts nothing. The Preview scene is always one of them, then the scenes set to Keep warm, then those a hotkey switches to, then the scenes shown most recently. More scenes use more memory and CPU. 3 until you change it."
                </span>
            </label>
        </Show>
        <Show when=move || scenes.get().is_some() && gpu.get().is_some()>
            <label class="settings-field warm-gpu">
                <span class="settings-check-label">"Warm scenes' GPU memory"</span>
                <select
                    aria-label="Warm scenes' GPU memory"
                    prop:value=move || gpu.get().unwrap_or_default().to_string()
                    on:change=move |event| {
                        if let Ok(chosen) = event_target_value(&event).parse::<u32>() {
                            choose_gpu(chosen);
                        }
                    }
                >
                    {move || gpu_choices().into_iter().map(|mib| view! {
                        <option value=mib.to_string()>{gpu_label(mib)}</option>
                    }).collect_view()}
                </select>
                <span class="settings-check-note">
                    "The most video memory the warm scenes may use, as JStream estimates it from the size of each video, page and camera they show. Scenes are kept in the order above until the next one would go past it; the Preview scene always stays ready. The estimate is a lower bound. No limit until you change it."
                </span>
            </label>
        </Show>
        {move || error.get().map(|message| view! {
            <p class="settings-error" role="alert">{format!("Warm scenes: {message}")}</p>
        })}
    }
}

#[cfg(test)]
mod tests {
    use super::{budget, choice_label, gpu_label, read, set_budget};
    use jstream_control::protocol::{Hello, PROTOCOL_VERSION};
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
    fn the_budget_is_read_and_set_and_a_refusal_is_reported() {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        assert_eq!(now(budget(&control)), Ok(Some(3)), "the default");
        now(set_budget(&control, 0, None)).expect("0 is allowed");
        assert_eq!(now(budget(&control)), Ok(Some(0)));
        let refused = now(set_budget(&control, 9, None)).expect_err("past the most");
        assert!(refused.contains("scenes"), "{refused}");
        assert_eq!(now(budget(&control)), Ok(Some(0)), "unchanged");
    }

    #[test]
    fn the_gpu_limit_is_read_and_set_with_the_count() {
        let control = MockEngine::new().connect();
        now(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        }))
        .expect("hello");
        assert_eq!(now(read(&control)), Ok((Some(3), Some(0))), "no limit");
        now(set_budget(&control, 3, Some(2048))).expect("a limit");
        assert_eq!(now(read(&control)), Ok((Some(3), Some(2048))));
        now(set_budget(&control, 5, None)).expect("the count alone");
        assert_eq!(
            now(read(&control)),
            Ok((Some(5), Some(2048))),
            "setting the count keeps the limit"
        );
        let refused = now(set_budget(&control, 5, Some(70_000))).expect_err("past the most");
        assert!(refused.contains("gpu_mib"), "{refused}");
    }

    #[test]
    fn each_gpu_choice_says_how_much() {
        assert_eq!(gpu_label(0), "No limit");
        assert_eq!(gpu_label(512), "512 MB");
        assert_eq!(gpu_label(2048), "2 GB");
    }

    #[test]
    fn each_choice_says_how_many_scenes() {
        assert_eq!(choice_label(0), "Only the Preview scene");
        assert_eq!(choice_label(1), "1 scene");
        assert_eq!(choice_label(8), "8 scenes");
    }
}
