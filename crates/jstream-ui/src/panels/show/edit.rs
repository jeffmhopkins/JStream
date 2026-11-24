//! The Show panel's Edit view: the timeline. Its bar (the show picker, Edit / Run, Stop
//! show, Back, Continue and what it will do, Rehearse, Lock, the status, + Overlay track and
//! Fit show), the tool strip, the ruler with the play head's flag, the tracks, the
//! bookmark lines with their flags along the bottom, the loops' bars under them, and the
//! zoom bar over the whole show.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::{ModeSwitch, ShowPanel};
use leptos::prelude::*;

/// The Edit view.
#[component]
pub fn EditView() -> impl IntoView {
    let _panel = expect_context::<ShowPanel>();
    view! {
        <div class="show-edit">
            <div class="show-bar">
                <ModeSwitch />
            </div>
        </div>
    }
}
