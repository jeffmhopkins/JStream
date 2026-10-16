//! The confirmation the scene and source panels and the mixer's bus strips show before a
//! delete: the question, what else the `delete.plan` takes with it, and Delete or Cancel.
//!
//! Docs: crates/jstream-ui/README.md#design

use jstream_control::protocol::{Edit, Rev};
use leptos::prelude::*;

/// A delete waiting for the user's answer: the plan, made at `rev`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    /// What is asked: `Delete scene “Main”?`.
    pub question: String,
    /// What the plan does beyond deleting the target, if anything.
    pub detail: Option<String>,
    /// The revision the plan was made at. The batch is sent expecting it, so it is
    /// refused if the state changed in between.
    pub rev: Rev,
    /// The plan's edits.
    pub edits: Vec<Edit>,
}

/// Shows `pending` while it is set. Delete hands the plan to `confirm`; either answer
/// clears it.
#[component]
pub fn Confirm(
    pending: RwSignal<Option<Pending>>,
    confirm: impl Fn(Pending) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    move || {
        pending.get().map(|plan| {
            let question = plan.question.clone();
            let detail = plan.detail.clone();
            view! {
                <div class="confirm" role="alertdialog">
                    <p>{question}</p>
                    {detail.map(|d| view! { <p class="muted">{d}</p> })}
                    <button class="danger" on:click=move |_| {
                        pending.set(None);
                        confirm(plan.clone());
                    }>"Delete"</button>
                    <button on:click=move |_| pending.set(None)>"Cancel"</button>
                </div>
            }
        })
    }
}
