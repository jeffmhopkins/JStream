//! The PTZ panel (ADR-0146, [ADR-0208](../../../../docs/adr/0208-value-bindings-aim-ptz-cameras.md)):
//! live control of the machine's PTZ camera heads, one at a time.
//!
//! - **The camera** is picked from `ptz.list`, which the panel reads about twice a second
//!   while it is shown, with a dot for whether the head answered lately and its last
//!   error. Cameras… opens Settings › Devices on "Control PTZ cameras" in the desktop
//!   app (`ptz_cameras.rs`); with no camera listed the panel says how to add one (in a
//!   browser, the daemon's `--ptz-camera`).
//! - **Pan and tilt** are a pad of eight arrows around Home. Holding an arrow drives the
//!   head (`ptz.drive`) at the panel's Speed; letting go, or the pointer leaving, stops
//!   it. With the pad focused the arrow keys do the same. Home sends `ptz.home`.
//! - **Zoom** has Wide and Tele, held as the arrows are. **Focus** has Auto, One push
//!   and Manual, and Near and Far held while it is manual (`ptz.focus`).
//! - **Speed** is the pan step, 1 to 24; tilt takes the same step up to 23, and zoom and
//!   focus the step scaled to 1 to 8.
//! - **Position** shows the pan, tilt, zoom and focus the head last answered. Each is a
//!   number field that aims the head there (`ptz.aim`), and each has the control button
//!   (`learn.rs`), so a MIDI control, a channel or a key can drive that axis: a value
//!   binding on `ptz_axis`, over a typical head's range, which the mapping can narrow.
//! - **Presets** are buttons 1 to 16, and a field for any other number, 0 to 127. A
//!   click recalls one (`ptz.recall`). Save arms the next click to store the head's
//!   position there instead (`ptz.store`), and Escape or Save again disarms it. A
//!   preset's menu (right-click) recalls or saves it, names it, binds a key that recalls
//!   it (`hotkey.set`), and makes it the selected scene's shot for this camera. Names
//!   are kept per machine in the UI settings (`ptz.preset_names`), never in the
//!   collection.
//! - **Scene shot** shows the selected scene's shot for this camera, with Use position
//!   (the head's pose now), and Clear, sent as `scene.set_ptz_shots`.
//!
//! Nothing here but the shot is an edit: control is runtime actions, never undone.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#live-panels

use crate::app::SessionContext;
use crate::icons::Icon;
use crate::store::{Studio, query};
use jstream_control::protocol::{
    Action, Chord, Edit, MAX_PTZ_PAN_SPEED, MAX_PTZ_PRESET, MAX_PTZ_TILT_SPEED, MAX_PTZ_ZOOM_SPEED,
    PtzAxis, PtzCameraInfo, PtzFocus, PtzPose, PtzShot, Query, QueryResult, SceneId, ValueTarget,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::time::Duration;

/// How often the panel reads `ptz.list` while it is shown.
const POLL: Duration = Duration::from_millis(500);
/// The presets the panel shows as buttons.
pub const PRESET_BUTTONS: u8 = 16;
/// The UI setting the preset names are kept under: `{camera: {preset: name}}`.
pub const PRESET_NAMES_KEY: &str = "ptz.preset_names";
/// The speed the panel starts at.
const START_SPEED: u8 = 12;

/// A direction on the pad: pan (-1 left, 1 right) and tilt (-1 down, 1 up).
pub type Direction = (i8, i8);

/// The pad's arrows, in its grid's reading order, Home (none) in the middle.
const PAD: [Option<(Direction, &str, i32)>; 9] = [
    Some(((-1, 1), "Up and left", -45)),
    Some(((0, 1), "Up", 0)),
    Some(((1, 1), "Up and right", 45)),
    Some(((-1, 0), "Left", -90)),
    None,
    Some(((1, 0), "Right", 90)),
    Some(((-1, -1), "Down and left", -135)),
    Some(((0, -1), "Down", 180)),
    Some(((1, -1), "Down and right", 135)),
];

/// The pan and tilt speeds of a drive in `direction` at pan step `speed`: tilt takes the
/// same step, up to its own fastest.
pub fn drive_speeds(direction: Direction, speed: u8) -> (i8, i8) {
    let pan = speed.clamp(1, MAX_PTZ_PAN_SPEED);
    let tilt = pan.min(MAX_PTZ_TILT_SPEED);
    let signed = |d: i8, s: u8| d.signum() * i8::try_from(s).unwrap_or(i8::MAX);
    (signed(direction.0, pan), signed(direction.1, tilt))
}

/// The zoom or focus step for pan step `speed`: 1 to 24 scaled to 1 to 8, rounding up.
pub fn zoom_speed(speed: u8) -> u8 {
    let speed = u32::from(speed.clamp(1, MAX_PTZ_PAN_SPEED));
    let scaled = (speed * u32::from(MAX_PTZ_ZOOM_SPEED)).div_ceil(u32::from(MAX_PTZ_PAN_SPEED));
    u8::try_from(scaled).unwrap_or(MAX_PTZ_ZOOM_SPEED)
}

/// The direction an arrow key drives, if it is one.
pub fn key_direction(key: &str) -> Option<Direction> {
    match key {
        "ArrowLeft" => Some((-1, 0)),
        "ArrowRight" => Some((1, 0)),
        "ArrowUp" => Some((0, 1)),
        "ArrowDown" => Some((0, -1)),
        _ => None,
    }
}

/// The range the control button maps a controller onto for `axis`: a typical head's
/// (Sony-style VISCA counts). A head's own range differs, so the mapping's range is
/// where it is narrowed or widened, within [`PtzAxis::range`].
pub fn mapping_range(axis: PtzAxis) -> (f64, f64) {
    match axis {
        PtzAxis::Pan => (-2448.0, 2448.0),
        PtzAxis::Tilt => (-1200.0, 1200.0),
        PtzAxis::Zoom => (0.0, 16384.0),
        PtzAxis::Focus => (4096.0, 61440.0),
    }
}

/// How an axis reads.
pub fn axis_label(axis: PtzAxis) -> &'static str {
    match axis {
        PtzAxis::Pan => "Pan",
        PtzAxis::Tilt => "Tilt",
        PtzAxis::Zoom => "Zoom",
        PtzAxis::Focus => "Focus",
    }
}

/// `axis` of `pose`, if the head answered it.
pub fn axis_value(pose: &PtzPose, axis: PtzAxis) -> Option<f64> {
    match axis {
        PtzAxis::Pan => pose.pan.map(f64::from),
        PtzAxis::Tilt => pose.tilt.map(f64::from),
        PtzAxis::Zoom => pose.zoom.map(f64::from),
        PtzAxis::Focus => pose.focus.map(f64::from),
    }
}

/// The pose that aims `axis` at `value`, from the head's last pose `now`: pan and tilt go
/// together, so setting one keeps the other where the head is. `None` when pan or tilt
/// is set and the head hasn't answered the other.
pub fn aim_pose(now: &PtzPose, axis: PtzAxis, value: f64) -> Option<PtzPose> {
    let (min, max) = axis.range();
    // Kept within the axis's `i32` range, so the casts are exact.
    let count = value.round().clamp(f64::from(min), f64::from(max)) as i32;
    let unsigned = u16::try_from(count.max(0)).unwrap_or(u16::MAX);
    let mut pose = PtzPose::default();
    match axis {
        PtzAxis::Pan => {
            pose.pan = Some(count);
            pose.tilt = Some(now.tilt?);
        }
        PtzAxis::Tilt => {
            pose.tilt = Some(count);
            pose.pan = Some(now.pan?);
        }
        PtzAxis::Zoom => pose.zoom = Some(unsigned),
        PtzAxis::Focus => pose.focus = Some(unsigned),
    }
    Some(pose)
}

/// `shots` with `camera`'s shot replaced by `shot`, or removed for `None`, keeping the
/// others' order; a new shot goes last.
pub fn shots_with(shots: &[PtzShot], camera: &str, shot: Option<PtzShot>) -> Vec<PtzShot> {
    let mut next: Vec<PtzShot> = Vec::with_capacity(shots.len() + 1);
    let mut placed = false;
    for existing in shots {
        if existing.camera == camera {
            if let Some(shot) = &shot
                && !placed
            {
                next.push(shot.clone());
                placed = true;
            }
        } else {
            next.push(existing.clone());
        }
    }
    if let Some(shot) = shot
        && !placed
    {
        next.push(shot);
    }
    next
}

/// How a shot reads: its preset (by name, when it has one) or its pose.
pub fn shot_label(shot: &PtzShot, name: Option<&str>) -> String {
    match (shot.preset, name) {
        (Some(p), Some(name)) => format!("Preset {p} · {name}"),
        (Some(p), None) => format!("Preset {p}"),
        (None, _) => {
            let mut parts = Vec::new();
            if let (Some(pan), Some(tilt)) = (shot.pose.pan, shot.pose.tilt) {
                parts.push(format!("pan {pan}, tilt {tilt}"));
            }
            if let Some(zoom) = shot.pose.zoom {
                parts.push(format!("zoom {zoom}"));
            }
            if let Some(focus) = shot.pose.focus {
                parts.push(format!("focus {focus}"));
            }
            format!("Position: {}", parts.join(", "))
        }
    }
}

/// The name kept for `camera`'s `preset` in the UI setting's value `names`.
pub fn preset_name(names: &Value, camera: &str, preset: u8) -> Option<String> {
    names
        .get(camera)?
        .get(preset.to_string())?
        .as_str()
        .map(str::to_owned)
        .filter(|n| !n.is_empty())
}

/// `names` with `camera`'s `preset` named `name`, or its name removed for an empty one.
pub fn with_preset_name(names: &Value, camera: &str, preset: u8, name: &str) -> Value {
    let mut all = names.as_object().cloned().unwrap_or_default();
    let mut theirs = all
        .get(camera)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let name = name.trim();
    if name.is_empty() {
        theirs.remove(&preset.to_string());
    } else {
        theirs.insert(preset.to_string(), Value::from(name));
    }
    if theirs.is_empty() {
        all.remove(camera);
    } else {
        all.insert(camera.to_owned(), Value::Object(theirs));
    }
    Value::Object(all)
}

/// The camera the panel shows: `picked` while it is listed, else the first listed.
pub fn shown_camera(cameras: &[PtzCameraInfo], picked: Option<&str>) -> Option<String> {
    picked
        .filter(|p| cameras.iter().any(|c| c.camera == *p))
        .map(str::to_owned)
        .or_else(|| cameras.first().map(|c| c.camera.clone()))
}

/// A preset's menu: which preset, at a point in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Menu {
    /// The menu.
    Open(u8, f64, f64),
    /// Waiting for the key that recalls the preset.
    Key(u8, f64, f64),
}

/// What the panel reads from the engine.
#[derive(Clone, Debug, Default, PartialEq)]
struct Listed {
    /// `ptz.list`'s cameras; empty when the machine lists none.
    cameras: Vec<PtzCameraInfo>,
    /// Whether it has read the list once.
    read: bool,
    /// Why it couldn't.
    problem: Option<String>,
}

/// The panel.
#[component]
pub fn Ptz() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let settings = use_context::<crate::settings::SettingsDialog>();
    let listed = RwSignal::new(Listed::default());
    let picked = RwSignal::new(None::<String>);
    let speed = RwSignal::new(START_SPEED);
    let saving = RwSignal::new(false);
    let manual = RwSignal::new(false);
    let other = RwSignal::new(None::<f64>);
    let names = RwSignal::new(Value::Null);
    let menu = RwSignal::new(None::<Menu>);
    let renaming = RwSignal::new(None::<(u8, String)>);
    // The drive the pointer or a key holds now, so letting go stops only that one.
    let driving = StoredValue::new(false);

    let read = move || {
        let Some(s) = session.get() else {
            return;
        };
        spawn_local(async move {
            let next = match query(&s, Query::PtzList).await {
                Ok(QueryResult::PtzList { cameras }) => Listed {
                    cameras,
                    read: true,
                    problem: None,
                },
                Ok(_) => Listed {
                    read: true,
                    problem: Some("The engine answered `ptz.list` with something else.".into()),
                    ..Listed::default()
                },
                Err(error) => Listed {
                    read: true,
                    problem: Some(error.to_string()),
                    ..Listed::default()
                },
            };
            listed.try_update(|now| {
                if *now != next {
                    *now = next;
                }
            });
        });
    };
    read();
    if let Ok(timer) = set_interval_with_handle(read, POLL) {
        on_cleanup(move || timer.clear());
    }
    if let Some(s) = session.get() {
        spawn_local(async move {
            if let Ok(crate::ui_settings::Kept::Value(value)) =
                crate::ui_settings::get(&*s.control, PRESET_NAMES_KEY).await
            {
                names.try_set(value);
            }
        });
    }

    let camera =
        Memo::new(move |_| listed.with(|l| shown_camera(&l.cameras, picked.get().as_deref())));
    let info = move || {
        let name = camera.get()?;
        listed.with(|l| l.cameras.iter().find(|c| c.camera == name).cloned())
    };
    // What the views follow, each changing only when it does: `ptz.list` is read twice
    // a second and its counters move on every read, so a view that read the list itself
    // would be redrawn (and a held button lost) each time.
    let status =
        Memo::new(move |_| listed.with(|l| (l.problem.clone(), l.cameras.is_empty(), l.read)));
    let names_listed = Memo::new(move |_| {
        listed.with(|l| {
            l.cameras
                .iter()
                .map(|c| c.camera.clone())
                .collect::<Vec<_>>()
        })
    });
    let reach = Memo::new(move |_| info().map(|c| (c.reachable, c.error)));
    let posed = Memo::new(move |_| info().and_then(|c| c.pose).unwrap_or_default());
    let pose = move || posed.get();

    let act = move |action: Action| {
        if let Some(s) = session.get() {
            studio.act(&s, action);
        }
    };
    let drive = move |pan: i8, tilt: i8, zoom: i8| {
        let Some(name) = camera.get_untracked() else {
            return;
        };
        driving.set_value((pan, tilt, zoom) != (0, 0, 0));
        act(Action::PtzDrive {
            camera: name,
            pan,
            tilt,
            zoom,
        });
    };
    let stop = move || {
        if driving.get_value() {
            drive(0, 0, 0);
        }
    };
    on_cleanup(move || {
        if driving.try_get_value() == Some(true)
            && let (Some(s), Some(name)) = (session.get(), camera.try_get_untracked().flatten())
        {
            let control = s.control.clone();
            spawn_local(async move {
                let _ = control
                    .call(jstream_control::protocol::Call::Action {
                        action: Action::PtzDrive {
                            camera: name,
                            pan: 0,
                            tilt: 0,
                            zoom: 0,
                        },
                    })
                    .await;
            });
        }
    });
    let pan_tilt = move |direction: Direction| {
        let (pan, tilt) = drive_speeds(direction, speed.get_untracked());
        drive(pan, tilt, 0);
    };
    let zoom = move |sign: i8| {
        let step = i8::try_from(zoom_speed(speed.get_untracked())).unwrap_or(1);
        drive(0, 0, sign * step);
    };
    let focus = move |mode: PtzFocus| {
        let Some(name) = camera.get_untracked() else {
            return;
        };
        let speed = matches!(mode, PtzFocus::Near | PtzFocus::Far)
            .then(|| zoom_speed(speed.get_untracked()));
        act(Action::PtzFocus {
            camera: name,
            focus: mode,
            speed,
        });
    };
    let home = move || {
        if let Some(name) = camera.get_untracked() {
            act(Action::PtzHome { camera: name });
        }
    };
    let recall_or_store = move |preset: u8| {
        let Some(name) = camera.get_untracked() else {
            return;
        };
        if saving.get_untracked() {
            saving.set(false);
            act(Action::PtzStore {
                camera: name,
                preset,
            });
            studio.notify(format!("Saved the camera's position as preset {preset}."));
        } else {
            act(Action::PtzRecall {
                camera: name,
                preset,
            });
        }
    };
    let store = move |preset: u8| {
        if let Some(name) = camera.get_untracked() {
            act(Action::PtzStore {
                camera: name,
                preset,
            });
            studio.notify(format!("Saved the camera's position as preset {preset}."));
        }
    };
    let aim = move |axis: PtzAxis, value: f64| -> bool {
        let Some(name) = camera.get_untracked() else {
            return false;
        };
        let Some(pose) = aim_pose(&untrack(pose), axis, value) else {
            studio.problem.set(Some(
                "The camera hasn't said where it is yet, so it can't move one axis alone.".into(),
            ));
            return false;
        };
        // Shown at once where it was aimed, as the head will answer, so the field and
        // Use position needn't wait for the next read.
        listed.update(|l| {
            if let Some(c) = l.cameras.iter_mut().find(|c| c.camera == name) {
                let now = c.pose.get_or_insert_with(PtzPose::default);
                now.pan = pose.pan.or(now.pan);
                now.tilt = pose.tilt.or(now.tilt);
                now.zoom = pose.zoom.or(now.zoom);
                now.focus = pose.focus.or(now.focus);
            }
        });
        act(Action::PtzAim {
            camera: name,
            pose,
            speed: Some(speed.get_untracked()),
        });
        true
    };
    let preset_label = move |preset: u8| {
        let name = camera.get()?;
        names.with(|n| preset_name(n, &name, preset))
    };
    let save_names = move |next: Value| {
        names.set(next.clone());
        if let Some(s) = session.get() {
            spawn_local(async move {
                if let Err(error) =
                    crate::ui_settings::set(&*s.control, PRESET_NAMES_KEY, next).await
                {
                    studio
                        .problem
                        .try_set(Some(format!("The preset names couldn't be kept: {error}")));
                }
            });
        }
    };
    let rename = move |preset: u8, name: String| {
        renaming.set(None);
        if let Some(camera) = camera.get_untracked() {
            let next = names.with_untracked(|n| with_preset_name(n, &camera, preset, &name));
            save_names(next);
        }
    };

    // The selected scene and its shot for this camera.
    let scene = move || studio.selection.with(|s| s.scene.clone());
    let scene_shots = move || -> Option<(SceneId, String, Vec<PtzShot>)> {
        let id = scene()?;
        studio.collection.with(|c| {
            let scene = c.as_ref()?.scene(&id)?;
            Some((id, scene.name.clone(), scene.ptz_shots.clone()))
        })
    };
    let shot = move || {
        let name = camera.get()?;
        scene_shots()?.2.into_iter().find(|s| s.camera == name)
    };
    let set_shot = move |shot: Option<PtzShot>| {
        let (Some(s), Some(name), Some((scene_id, _, shots))) =
            (session.get(), camera.get_untracked(), untrack(scene_shots))
        else {
            studio.problem.set(Some(
                "Select a scene first: a shot belongs to a scene.".into(),
            ));
            return;
        };
        studio.apply(
            &s,
            vec![Edit::SceneSetPtzShots {
                scene_id,
                shots: shots_with(&shots, &name, shot),
            }],
        );
    };
    let use_position = move || {
        let Some(name) = camera.get_untracked() else {
            return;
        };
        let now = untrack(pose);
        if now.is_empty() {
            studio.problem.set(Some(
                "The camera hasn't said where it is yet. Check that it is reachable.".into(),
            ));
            return;
        }
        set_shot(Some(PtzShot {
            camera: name,
            preset: None,
            pose: now,
        }));
    };
    let preset_shot = move |preset: u8| {
        if let Some(name) = camera.get_untracked() {
            set_shot(Some(PtzShot {
                camera: name,
                preset: Some(preset),
                pose: PtzPose::default(),
            }));
        }
    };
    // The key bound to recall `preset` on this camera, if any.
    let preset_key = move |preset: u8| -> Option<Chord> {
        let name = camera.get()?;
        let recall = Action::PtzRecall {
            camera: name,
            preset,
        };
        studio.collection.with(|c| {
            c.as_ref()?
                .hotkeys
                .iter()
                .find(|h| h.action().as_ref() == Some(&recall))
                .map(|h| h.chord.clone())
        })
    };
    let set_key = move |preset: u8, chord: Option<Chord>| {
        menu.set(None);
        let (Some(chord), Some(s), Some(name)) = (chord, session.get(), camera.get_untracked())
        else {
            return;
        };
        let recall = Action::PtzRecall {
            camera: name,
            preset,
        };
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| crate::hotkeys::bind_edits(c, &recall, &chord))
                .unwrap_or_default()
        });
        if !edits.is_empty() {
            studio.apply(&s, edits);
        }
    };
    let clear_key = move |preset: u8| {
        menu.set(None);
        if let (Some(chord), Some(s)) = (untrack(|| preset_key(preset)), session.get()) {
            studio.apply(&s, vec![Edit::HotkeyClear { chord }]);
        }
    };
    let open_settings = move || {
        if let Some(dialog) = settings {
            dialog.open(crate::settings::SettingsPage::Devices);
            request_animation_frame(|| {
                if let Some(el) = leptos::web_sys::window()
                    .and_then(|w| w.document())
                    .and_then(|d| d.get_element_by_id("ptz-camera-setting"))
                {
                    el.scroll_into_view();
                }
            });
        }
    };
    let desktop = crate::host::desktop_ptz_cameras().is_some();

    let pad_keys = move |e: leptos::ev::KeyboardEvent| {
        if let Some(direction) = key_direction(&e.key()) {
            e.prevent_default();
            if !e.repeat() {
                pan_tilt(direction);
            }
        } else if e.key() == "Home" {
            e.prevent_default();
            home();
        }
    };
    let pad_keys_up = move |e: leptos::ev::KeyboardEvent| {
        if key_direction(&e.key()).is_some() {
            e.prevent_default();
            stop();
        }
    };
    let panel_keys = move |e: leptos::ev::KeyboardEvent| {
        if e.key() == "Escape" && saving.get_untracked() {
            e.prevent_default();
            saving.set(false);
        }
    };

    // A button held to drive: `start` on press, a stop on release or leaving.
    let held = move |label: &'static str,
                     class: &'static str,
                     start: Box<dyn Fn() + Send + Sync>,
                     inner: AnyView| {
        let start = StoredValue::new(start);
        view! {
            <button
                type="button"
                class=class
                aria-label=label
                title=label
                on:pointerdown=move |e: leptos::ev::PointerEvent| {
                    if e.button() == 0 {
                        start.with_value(|f| f());
                    }
                }
                on:pointerup=move |_| stop()
                on:pointerleave=move |_| stop()
                on:pointercancel=move |_| stop()
                on:blur=move |_| stop()
            >
                {inner}
            </button>
        }
    };

    let header = move || {
        view! {
            <div class="ptz-head">
                <select
                    class="ptz-camera-pick"
                    aria-label="Camera"
                    prop:value=move || camera.get().unwrap_or_default()
                    on:change=move |e| picked.set(Some(event_target_value(&e)))
                >
                    {move || names_listed.get().into_iter().map(|name| {
                        let mine = name.clone();
                        view! {
                            <option value=name.clone() prop:selected=move || camera.get().as_deref() == Some(mine.as_str())>
                                {name.clone()}
                            </option>
                        }
                    }).collect_view()}
                </select>
                {move || reach.get().map(|(reachable, error)| {
                    let (class, text) = if reachable {
                        ("ptz-dot on", "Answering".to_owned())
                    } else {
                        ("ptz-dot", "Not answering".to_owned())
                    };
                    let title = error.clone().map_or(text.clone(), |e| format!("{text}: {e}"));
                    view! { <span class=class title=title.clone() aria-label=title role="status"></span> }
                })}
                {desktop.then(|| view! {
                    <button type="button" class="icon-button" title="Cameras…" aria-label="Cameras…" on:click=move |_| open_settings()>
                        <Icon name="gear" size=14 />
                    </button>
                })}
            </div>
        }
    };

    let pad = move || {
        view! {
            <div class="ptz-pad" role="group" aria-label="Pan and tilt" tabindex="0" on:keydown=pad_keys on:keyup=pad_keys_up>
                {PAD.iter().map(|cell| match *cell {
                    Some((direction, label, angle)) => held(
                        label,
                        "ptz-arrow",
                        Box::new(move || pan_tilt(direction)),
                        view! {
                            <span class="ptz-arrow-icon" style:transform=format!("rotate({angle}deg)")>
                                <Icon name="up" size=14 />
                            </span>
                        }.into_any(),
                    ).into_any(),
                    None => view! {
                        <button type="button" class="ptz-home" title="Home" aria-label="Home" on:click=move |_| home()>
                            "Home"
                        </button>
                    }.into_any(),
                }).collect_view()}
            </div>
        }
    };

    let lens = move || {
        view! {
            <div class="ptz-lens">
                <div class="ptz-lens-row">
                    <span class="ptz-lens-label">"Zoom"</span>
                    {held("Zoom out", "ptz-hold", Box::new(move || zoom(-1)), view! { "Wide" }.into_any())}
                    {held("Zoom in", "ptz-hold", Box::new(move || zoom(1)), view! { "Tele" }.into_any())}
                </div>
                <div class="ptz-lens-row">
                    <span class="ptz-lens-label">"Focus"</span>
                    <div class="segmented" role="group" aria-label="Focus mode">
                        <button type="button" class:on=move || !manual.get() on:click=move |_| { manual.set(false); focus(PtzFocus::Auto); }>"Auto"</button>
                        <button type="button" on:click=move |_| { manual.set(false); focus(PtzFocus::OnePush); }>"One push"</button>
                        <button type="button" class:on=move || manual.get() on:click=move |_| { manual.set(true); focus(PtzFocus::Manual); }>"Manual"</button>
                    </div>
                </div>
                {move || manual.get().then(|| {
                    let focusing = StoredValue::new(false);
                    let near = StoredValue::new(move || { focusing.set_value(true); focus(PtzFocus::Near); });
                    let far = StoredValue::new(move || { focusing.set_value(true); focus(PtzFocus::Far); });
                    let end = move || {
                        if focusing.get_value() {
                            focusing.set_value(false);
                            focus(PtzFocus::Stop);
                        }
                    };
                    view! {
                        <div class="ptz-lens-row">
                            <span class="ptz-lens-label"></span>
                            <button type="button" class="ptz-hold" title="Focus nearer"
                                on:pointerdown=move |_| near.with_value(|f| f())
                                on:pointerup=move |_| end() on:pointerleave=move |_| end()>"Near"</button>
                            <button type="button" class="ptz-hold" title="Focus farther"
                                on:pointerdown=move |_| far.with_value(|f| f())
                                on:pointerup=move |_| end() on:pointerleave=move |_| end()>"Far"</button>
                        </div>
                    }
                })}
                <label class="ptz-lens-row ptz-speed">
                    <span class="ptz-lens-label">"Speed"</span>
                    <input
                        type="range"
                        min="1"
                        max=MAX_PTZ_PAN_SPEED.to_string()
                        aria-label="Speed"
                        prop:value=move || speed.get().to_string()
                        on:input=move |e| {
                            if let Ok(v) = event_target_value(&e).parse::<u8>() {
                                speed.set(v.clamp(1, MAX_PTZ_PAN_SPEED));
                            }
                        }
                    />
                    <span class="ptz-speed-value">{move || speed.get().to_string()}</span>
                </label>
            </div>
        }
    };

    let position = move || {
        let Some(name) = camera.get() else {
            return ().into_any();
        };
        view! {
            <section class="ptz-section">
                <h4 class="eyebrow">"Position"</h4>
                {PtzAxis::ALL.into_iter().map(|axis| {
                    let (min, max) = axis.range();
                    let value = Signal::derive(move || axis_value(&pose(), axis));
                    let target = ValueTarget::PtzAxis { camera: name.clone(), axis };
                    view! {
                        <div class="ptz-axis">
                            <span class="ptz-axis-label">{axis_label(axis)}</span>
                            <crate::panels::number::NumberField
                                named=Signal::derive(move || axis_label(axis).to_owned())
                                value=value
                                step=16.0
                                decimals=0
                                min=f64::from(min)
                                max=f64::from(max)
                                send=move |v| aim(axis, v)
                            />
                            <crate::panels::learn::Learn
                                target=target
                                range=mapping_range(axis)
                                label=format!("{name} {}", axis_label(axis))
                                current=value
                            />
                        </div>
                    }
                }).collect_view()}
            </section>
        }
        .into_any()
    };

    let presets = move || {
        let Some(_) = camera.get() else {
            return ().into_any();
        };
        view! {
            <section class="ptz-section" on:keydown=panel_keys>
                <div class="ptz-section-head">
                    <h4 class="eyebrow">"Presets"</h4>
                    <button
                        type="button"
                        class="ptz-save"
                        class:on=move || saving.get()
                        aria-pressed=move || saving.get().to_string()
                        title="Click a preset next to save the camera's position there"
                        on:click=move |_| saving.update(|s| *s = !*s)
                    >
                        "Save"
                    </button>
                </div>
                {move || saving.get().then(|| view! {
                    <p class="ptz-hint" role="status">"Click a preset to save the camera's position there. Esc cancels."</p>
                })}
                <div class="ptz-presets" role="group" aria-label="Presets">
                    {(1..=PRESET_BUTTONS).map(|preset| {
                        let label = move || preset_label(preset);
                        let key = move || preset_key(preset).map(|c| crate::hotkeys::label(&c));
                        let describe = move || {
                            let what = if saving.get() { "Save to" } else { "Recall" };
                            let mut t = format!("{what} preset {preset}");
                            if let Some(name) = label() {
                                t.push_str(&format!(" · {name}"));
                            }
                            if let Some(key) = key() {
                                t.push_str(&format!(" ({key})"));
                            }
                            t
                        };
                        view! {
                            <button
                                type="button"
                                class="ptz-preset"
                                class:saving=move || saving.get()
                                title=describe
                                aria-label=describe
                                on:click=move |_| recall_or_store(preset)
                                on:contextmenu=move |e: leptos::ev::MouseEvent| {
                                    e.prevent_default();
                                    menu.set(Some(Menu::Open(preset, f64::from(e.client_x()), f64::from(e.client_y()))));
                                }
                            >
                                <span class="ptz-preset-n">{preset.to_string()}</span>
                                <span class="ptz-preset-name">{move || label().unwrap_or_default()}</span>
                            </button>
                        }
                    }).collect_view()}
                </div>
                <form class="ptz-other" on:submit=move |e| {
                    e.prevent_default();
                    if let Some(v) = other.get_untracked() {
                        recall_or_store(v.round().clamp(0.0, f64::from(MAX_PTZ_PRESET)) as u8);
                    }
                }>
                    <span>"Preset"</span>
                    <crate::panels::number::NumberField
                        named=Signal::derive(|| "Preset number".to_owned())
                        value=Signal::derive(move || Some(other.get().unwrap_or(0.0)))
                        step=1.0
                        decimals=0
                        min=0.0
                        max=f64::from(MAX_PTZ_PRESET)
                        send=move |v| { other.set(Some(v)); true }
                    />
                    <button type="submit">{move || if saving.get() { "Save" } else { "Recall" }}</button>
                </form>
            </section>
        }
        .into_any()
    };

    let scene_shot = move || {
        let Some(name) = camera.get() else {
            return ().into_any();
        };
        let Some((_, scene_name, _)) = scene_shots() else {
            return view! {
                <section class="ptz-section">
                    <h4 class="eyebrow">"Scene shot"</h4>
                    <p class="placeholder">"Select a scene to give it a shot of this camera."</p>
                </section>
            }
            .into_any();
        };
        let current = shot();
        let text = current.as_ref().map_or_else(
            || "None: switching to it leaves the camera where it is.".to_owned(),
            |s| {
                let n = s
                    .preset
                    .and_then(|p| names.with(|all| preset_name(all, &name, p)));
                shot_label(s, n.as_deref())
            },
        );
        let has = current.is_some();
        view! {
            <section class="ptz-section">
                <h4 class="eyebrow">{format!("Shot in “{scene_name}”")}</h4>
                <p class="ptz-shot">{text}</p>
                <div class="ptz-shot-actions">
                    <button type="button" title="Frame this scene where the camera is now" on:click=move |_| use_position()>"Use position"</button>
                    {has.then(|| view! {
                        <button type="button" on:click=move |_| set_shot(None)>"Clear"</button>
                    })}
                </div>
            </section>
        }
        .into_any()
    };

    // Below the pointer, kept inside the window.
    let place = |x: f64, y: f64| {
        let (w, h) = leptos::web_sys::window()
            .map(|w| {
                let n = |v: Result<
                    leptos::wasm_bindgen::JsValue,
                    leptos::wasm_bindgen::JsValue,
                >| { v.ok().and_then(|v| v.as_f64()) };
                (
                    n(w.inner_width()).unwrap_or(1600.0),
                    n(w.inner_height()).unwrap_or(1000.0),
                )
            })
            .unwrap_or((1600.0, 1000.0));
        let left = x.min(w - 240.0).max(4.0);
        let top = if y + 260.0 > h {
            (y - 260.0).max(4.0)
        } else {
            y
        };
        (format!("{left}px"), format!("{top}px"))
    };
    let act_recall = move |preset: u8| {
        if let Some(name) = camera.get_untracked() {
            act(Action::PtzRecall {
                camera: name,
                preset,
            });
        }
    };
    let close_menu = move || menu.set(None);
    let menu_keys = crate::menu::keys(close_menu);
    let preset_menu = move || {
        let node = NodeRef::<leptos::html::Div>::new();
        match menu.get()? {
            Menu::Open(preset, x, y) => {
                crate::menu::focus_first(node);
                let key = untrack(|| preset_key(preset));
                let has_scene = untrack(scene).is_some();
                Some(view! {
                    <div class="ctx-catcher" on:pointerdown=move |_| close_menu()></div>
                    <div class="context-menu" role="menu" aria-label=format!("Preset {preset}") node_ref=node on:keydown=menu_keys
                        style:left=place(x, y).0 style:top=place(x, y).1>
                        <div class="ctx-head">{format!("Preset {preset}")}</div>
                        <button type="button" class="menuitem" role="menuitem" on:click=move |_| { close_menu(); act_recall(preset); }>
                            <span class="ctx-check"></span>
                            <span class="ctx-label">"Recall"</span>
                        </button>
                        <button type="button" class="menuitem" role="menuitem" on:click=move |_| { close_menu(); store(preset); }>
                            <span class="ctx-check"></span>
                            <span class="ctx-label">"Save position here"</span>
                        </button>
                        <button type="button" class="menuitem" role="menuitem" on:click=move |_| {
                            close_menu();
                            renaming.set(Some((preset, untrack(|| preset_label(preset)).unwrap_or_default())));
                        }>
                            <span class="ctx-check"></span>
                            <span class="ctx-label">"Name…"</span>
                        </button>
                        <div class="menu-sep"></div>
                        <button type="button" class="menuitem" role="menuitem" on:click=move |_| menu.set(Some(Menu::Key(preset, x, y)))>
                            <span class="ctx-check"></span>
                            <span class="ctx-label">"Set key…"</span>
                            <span class="hint">{key.as_ref().map_or_else(|| "recalls it".to_owned(), crate::hotkeys::label)}</span>
                        </button>
                        {key.is_some().then(|| view! {
                            <button type="button" class="menuitem" role="menuitem" on:click=move |_| clear_key(preset)>
                                <span class="ctx-check"></span>
                                <span class="ctx-label">"Remove key"</span>
                            </button>
                        })}
                        {has_scene.then(|| view! {
                            <div class="menu-sep"></div>
                            <button type="button" class="menuitem" role="menuitem" on:click=move |_| { close_menu(); preset_shot(preset); }>
                                <span class="ctx-check"></span>
                                <span class="ctx-label">"Use as scene shot"</span>
                            </button>
                        })}
                    </div>
                }
                .into_any())
            }
            Menu::Key(preset, x, y) => Some(view! {
                <div class="ctx-catcher" on:pointerdown=move |_| close_menu()></div>
                <div class="context-menu" role="menu" style:left=place(x, y).0 style:top=place(x, y).1>
                    <div class="ctx-head">{format!("Key for preset {preset}")}</div>
                    <crate::panels::learn::KeyCapture done=move |chord| set_key(preset, chord) />
                </div>
            }
            .into_any()),
        }
    };
    let rename_form = move || {
        let (preset, start) = renaming.get()?;
        let text = RwSignal::new(start);
        let input = NodeRef::<leptos::html::Input>::new();
        input.on_load(|i| {
            request_animation_frame(move || {
                let _ = i.focus();
                i.select();
            });
        });
        Some(view! {
            <form class="ptz-rename" on:submit=move |e| { e.prevent_default(); rename(preset, text.get_untracked()); }>
                <span>{format!("Name preset {preset}")}</span>
                <input
                    type="text"
                    maxlength="32"
                    node_ref=input
                    aria-label=format!("Name of preset {preset}")
                    prop:value=move || text.get()
                    on:input=move |e| text.set(event_target_value(&e))
                    on:keydown=move |e: leptos::ev::KeyboardEvent| {
                        if e.key() == "Escape" {
                            e.prevent_default();
                            renaming.set(None);
                        }
                    }
                />
                <button type="submit">"OK"</button>
                <button type="button" on:click=move |_| renaming.set(None)>"Cancel"</button>
            </form>
        })
    };

    view! {
        <section class="ptz-panel">
            <h2 class="visually-hidden">"PTZ cameras"</h2>
            {move || {
                let (problem, empty, read) = status.get();
                if let Some(problem) = problem {
                    return view! { <p class="problem" role="alert">{problem}</p> }.into_any();
                }
                if empty {
                    if !read {
                        return ().into_any();
                    }
                    return view! {
                        <div class="empty-state">
                            <Icon name="webcam" size=18 />
                            <p class="empty-title">"No PTZ camera"</p>
                            {if desktop {
                                view! {
                                    <p class="empty-note">"Add the camera heads on your network to pan, tilt, zoom and recall presets from here."</p>
                                    <button type="button" class="primary" on:click=move |_| open_settings()>"Add camera…"</button>
                                }.into_any()
                            } else {
                                view! {
                                    <p class="empty-note">"This machine lists no PTZ camera. Start the engine with "<code>"--ptz-camera name@address:port"</code>" for each camera head."</p>
                                }.into_any()
                            }}
                        </div>
                    }
                    .into_any();
                }
                view! {
                    {header}
                    {pad}
                    {lens}
                    {position}
                    {presets}
                    {rename_form}
                    {scene_shot}
                }
                .into_any()
            }}
            {preset_menu}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::{
        aim_pose, drive_speeds, key_direction, mapping_range, preset_name, shot_label, shots_with,
        shown_camera, with_preset_name, zoom_speed,
    };
    use jstream_control::protocol::{PtzAxis, PtzCameraInfo, PtzPose, PtzShot, PtzTransport};
    use serde_json::{Value, json};

    fn shot(camera: &str, preset: Option<u8>) -> PtzShot {
        PtzShot {
            camera: camera.into(),
            preset,
            pose: PtzPose::default(),
        }
    }

    #[test]
    fn a_drive_takes_the_speed_within_each_axis() {
        assert_eq!(drive_speeds((1, 0), 12), (12, 0));
        assert_eq!(
            drive_speeds((-1, -1), 24),
            (-24, -23),
            "tilt tops out at 23"
        );
        assert_eq!(
            drive_speeds((0, 1), 0),
            (0, 1),
            "never below the slowest step"
        );
        assert_eq!(zoom_speed(1), 1);
        assert_eq!(zoom_speed(12), 4);
        assert_eq!(zoom_speed(24), 8);
        assert_eq!(key_direction("ArrowUp"), Some((0, 1)));
        assert_eq!(key_direction("a"), None);
    }

    #[test]
    fn an_aim_moves_pan_and_tilt_together() {
        let now = PtzPose {
            pan: Some(10),
            tilt: Some(-4),
            zoom: Some(100),
            focus: None,
        };
        let pan = aim_pose(&now, PtzAxis::Pan, 250.4).expect("aims");
        assert_eq!((pan.pan, pan.tilt, pan.zoom), (Some(250), Some(-4), None));
        let zoom = aim_pose(&PtzPose::default(), PtzAxis::Zoom, -5.0).expect("aims");
        assert_eq!(zoom.zoom, Some(0), "kept within the axis");
        assert_eq!(aim_pose(&PtzPose::default(), PtzAxis::Tilt, 1.0), None);
        for axis in PtzAxis::ALL {
            let (lo, hi) = mapping_range(axis);
            let (min, max) = axis.range();
            assert!(f64::from(min) <= lo && hi <= f64::from(max), "{axis:?}");
        }
    }

    #[test]
    fn a_scene_keeps_one_shot_per_camera() {
        let shots = vec![shot("a", Some(1)), shot("b", Some(2))];
        let replaced = shots_with(&shots, "a", Some(shot("a", Some(5))));
        assert_eq!(replaced, vec![shot("a", Some(5)), shot("b", Some(2))]);
        let added = shots_with(&shots, "c", Some(shot("c", Some(3))));
        assert_eq!(added.len(), 3);
        assert_eq!(added[2].camera, "c");
        assert_eq!(shots_with(&shots, "a", None), vec![shot("b", Some(2))]);
        assert_eq!(
            shot_label(&shot("a", Some(3)), Some("Lectern")),
            "Preset 3 · Lectern"
        );
        let pose = PtzShot {
            camera: "a".into(),
            preset: None,
            pose: PtzPose {
                pan: Some(1),
                tilt: Some(2),
                zoom: Some(3),
                focus: None,
            },
        };
        assert_eq!(shot_label(&pose, None), "Position: pan 1, tilt 2, zoom 3");
    }

    #[test]
    fn preset_names_are_kept_per_camera() {
        let names = with_preset_name(&Value::Null, "Wide", 3, " Lectern ");
        assert_eq!(names, json!({"Wide": {"3": "Lectern"}}));
        assert_eq!(preset_name(&names, "Wide", 3).as_deref(), Some("Lectern"));
        assert_eq!(preset_name(&names, "Close", 3), None);
        let cleared = with_preset_name(&names, "Wide", 3, "");
        assert_eq!(cleared, json!({}));
    }

    #[test]
    fn the_picked_camera_stays_while_listed() {
        let info = |name: &str| PtzCameraInfo {
            camera: name.into(),
            address: "127.0.0.1:52381".into(),
            transport: PtzTransport::ViscaIp,
            visca_address: 1,
            reachable: false,
            pose: None,
            error: None,
            sent: 0,
            replies: 0,
            dropped: 0,
        };
        let cameras = vec![info("Wide"), info("Close")];
        assert_eq!(
            shown_camera(&cameras, Some("Close")).as_deref(),
            Some("Close")
        );
        assert_eq!(
            shown_camera(&cameras, Some("Gone")).as_deref(),
            Some("Wide")
        );
        assert_eq!(shown_camera(&[], None), None);
    }
}
