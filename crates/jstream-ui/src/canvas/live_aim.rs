//! The live aim of a 3D scene's camera, on screen (A19, ADR-0152): in camera mode the
//! canvas toolbar gains pan, tilt and zoom buttons that drive the camera while held
//! (`scene.drive_camera`), Home (`scene.aim_camera` back to the stored camera) and a
//! readout of the aim (`scene.camera_aim`, read about three times a second while it
//! shows). A live aim is not an edit: nothing is stored or undone, as from a VISCA
//! controller. Shift holds a button at a slow speed. A button holds its axis from a press,
//! captured so the pointer may wander, to its release, and Space or Enter holds it from the
//! keyboard until the key is let go or the button loses the focus; passing over a button
//! sends nothing.

use crate::app::SessionContext;
use crate::store::{Studio, query};
use jstream_control::protocol::{
    Action, CameraAim, CameraAimInfo, Finite32, Query, QueryResult, SceneId,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use std::time::Duration;

/// How often the readout reads the aim.
const READ_EVERY: Duration = Duration::from_millis(300);

/// One axis a button drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Left (negative) and right.
    Pan,
    /// Down (negative) and up.
    Tilt,
    /// Wide (negative) and tele.
    Zoom,
}

/// The speed a held button drives `axis` at, signed by `sign`: a brisk one, or a slow
/// one with Shift. Within each axis's VISCA range.
pub fn speed(axis: Axis, sign: i8, slow: bool) -> i8 {
    let magnitude = match (axis, slow) {
        (Axis::Zoom, false) => 5,
        (Axis::Zoom, true) => 2,
        (_, false) => 12,
        (_, true) => 3,
    };
    sign.signum() * magnitude
}

/// The `scene.drive_camera` that drives only `axis` at `speed` (0 stops it), leaving the
/// other axes as they move.
pub fn drive(scene_id: SceneId, axis: Axis, speed: i8) -> Action {
    let on = |a: Axis| (a == axis).then_some(speed);
    Action::SceneDriveCamera {
        scene_id,
        pan: on(Axis::Pan),
        tilt: on(Axis::Tilt),
        zoom: on(Axis::Zoom),
    }
}

/// The `scene.aim_camera` that glides back to the stored camera itself.
pub fn home(scene_id: SceneId) -> Action {
    Action::SceneAimCamera {
        scene_id,
        aim: CameraAim {
            pan_deg: Some(Finite32::ZERO),
            tilt_deg: Some(Finite32::ZERO),
            zoom: Some(Finite32::ONE),
        },
        speed: None,
    }
}

/// The readout: "Pan 12° · Tilt -3° · ×1.4", or "Home" at the stored camera.
pub fn aim_text(aim: &CameraAimInfo) -> String {
    let (pan, tilt, zoom) = (aim.pan_deg.get(), aim.tilt_deg.get(), aim.zoom.get());
    if pan.abs() < 0.05 && tilt.abs() < 0.05 && (zoom - 1.0).abs() < 0.005 {
        return "Home".into();
    }
    format!("Pan {pan:.0}° · Tilt {tilt:.0}° · ×{zoom:.1}")
}

/// The live aim's buttons and readout for `scene`, for the camera mode toolbar.
#[component]
pub fn LiveAim(
    /// The scene the canvas shows: a keyframe drives its family's camera.
    #[prop(into)]
    scene: Signal<Option<SceneId>>,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = use_context::<SessionContext>();
    let aim = RwSignal::new(None::<CameraAimInfo>);
    let read = move || {
        let (Some(s), Some(scene_id)) = (session.and_then(|s| s.get()), scene.get_untracked())
        else {
            return;
        };
        spawn_local(async move {
            let read = query(&s, Query::SceneCameraAim { scene_id }).await;
            // A scene drawn through no camera reads nothing: the readout stays blank.
            let now = match read {
                Ok(QueryResult::SceneCameraAim { aim }) => Some(aim),
                _ => None,
            };
            _ = aim.try_set(now);
        });
    };
    read();
    if let Ok(handle) = set_interval_with_handle(read, READ_EVERY) {
        on_cleanup(move || handle.clear());
    }
    let act = move |action: Action| {
        if let Some(s) = session.and_then(|s| s.get()) {
            studio.act(&s, action);
        }
    };
    let button = move |axis: Axis, sign: i8, label: &'static str, title: &'static str| {
        // Whether this button holds its axis: a press drives it once and its release
        // stops it once, so a pointer passing over, or a second way of letting go, sends
        // nothing that would stop an axis a controller or a binding drives.
        let held = StoredValue::new(false);
        let press = move |slow: bool| {
            if held.get_value() {
                return;
            }
            if let Some(scene_id) = scene.get_untracked() {
                held.set_value(true);
                act(drive(scene_id, axis, speed(axis, sign, slow)));
            }
        };
        let release = move || {
            if !held.get_value() {
                return;
            }
            held.set_value(false);
            if let Some(scene_id) = scene.get_untracked() {
                act(drive(scene_id, axis, 0));
            }
        };
        // The pointer is captured on the press, so it holds the button until it is let
        // go wherever it has moved.
        let pointer_down = move |event: leptos::ev::PointerEvent| {
            if event.button() != 0 {
                return;
            }
            if let Some(el) = event
                .current_target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            {
                _ = el.set_pointer_capture(event.pointer_id());
            }
            press(event.shift_key());
        };
        // Space or Enter holds the button from the keyboard; a repeat is the same hold.
        let held_key =
            |event: &leptos::ev::KeyboardEvent| matches!(event.key().as_str(), " " | "Enter");
        let key_down = move |event: leptos::ev::KeyboardEvent| {
            if !held_key(&event) {
                return;
            }
            event.prevent_default();
            if !event.repeat() {
                press(event.shift_key());
            }
        };
        let key_up = move |event: leptos::ev::KeyboardEvent| {
            if held_key(&event) {
                event.prevent_default();
                release();
            }
        };
        view! {
            <button
                class="canvas-tool canvas-tool-text live-aim"
                aria-label=title
                title=format!("{title} while held (Shift: slowly) · live only, nothing is saved")
                on:pointerdown=pointer_down
                on:pointerup=move |_| release()
                on:pointercancel=move |_| release()
                on:lostpointercapture=move |_| release()
                on:keydown=key_down
                on:keyup=key_up
                on:blur=move |_| release()
            >
                {label}
            </button>
        }
    };
    view! {
        <span class="canvas-tool-gap" aria-hidden="true"></span>
        {button(Axis::Pan, -1, "◀", "Pan left")}
        {button(Axis::Tilt, 1, "▲", "Tilt up")}
        {button(Axis::Tilt, -1, "▼", "Tilt down")}
        {button(Axis::Pan, 1, "▶", "Pan right")}
        {button(Axis::Zoom, -1, "−", "Zoom out")}
        {button(Axis::Zoom, 1, "+", "Zoom in")}
        <button
            class="canvas-tool canvas-tool-text"
            title="Glide the live aim back to the scene's own camera"
            on:click=move |_| {
                if let Some(scene_id) = scene.get_untracked() {
                    act(home(scene_id));
                }
            }
        >
            "⌂ Home"
        </button>
        <span class="live-aim-readout" aria-live="polite">
            {move || aim.get().map(|a| aim_text(&a))}
        </span>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_button_drives_its_axis_alone() {
        let scene = SceneId::new("s");
        assert_eq!(
            drive(scene.clone(), Axis::Tilt, speed(Axis::Tilt, -1, false)),
            Action::SceneDriveCamera {
                scene_id: scene.clone(),
                pan: None,
                tilt: Some(-12),
                zoom: None,
            }
        );
        assert_eq!(speed(Axis::Zoom, 1, true), 2);
        assert_eq!(speed(Axis::Pan, 1, true), 3);
        let Action::SceneAimCamera { aim, .. } = home(scene) else {
            panic!("an aim");
        };
        assert_eq!(aim.problem(), None, "home is a valid aim");
    }

    #[test]
    fn the_readout_says_home_at_the_stored_camera() {
        let at = |pan: f32, tilt: f32, zoom: f32| CameraAimInfo {
            pan_deg: Finite32::new(pan).expect("finite"),
            tilt_deg: Finite32::new(tilt).expect("finite"),
            zoom: Finite32::new(zoom).expect("finite"),
            moving: false,
        };
        assert_eq!(aim_text(&at(0.0, 0.0, 1.0)), "Home");
        assert_eq!(aim_text(&at(12.4, -3.0, 1.43)), "Pan 12° · Tilt -3° · ×1.4");
    }
}
