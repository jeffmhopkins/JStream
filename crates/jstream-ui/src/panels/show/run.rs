//! The Show panel's Run view, for show time: its bar (the show picker, Edit / Run, Stop
//! show, Lock, Rehearse on preview, the Next monitor's Live / Still / Off and the status),
//! the Now and Next cards over a big Continue, the bookmarks as buttons in show order, the
//! clocks, what is up right now, and the tracks folded into one strip.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::{ModeSwitch, ShowPanel};
use leptos::prelude::*;

/// The Run view.
#[component]
pub fn RunView() -> impl IntoView {
    let _panel = expect_context::<ShowPanel>();
    view! {
        <div class="show-run">
            <div class="show-bar">
                <ModeSwitch />
            </div>
        </div>
    }
}
