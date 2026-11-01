//! Projector windows (DL-07): the program mix, one scene, or a multiview in a window of
//! its own, or full screen. A projector is this same page, opened by [`open`] with a
//! `?projector=` query ([`Request`]): the app connects as it always does and shows
//! [`Projector`] instead of the workspace. Each picture is a preview stream
//! (`preview.subscribe`, with `scene_id` for a scene), all on the session's one binary
//! channel, which [`Channels`] hands out by channel number.
//!
//! A double click fills the screen the window is on, or leaves full screen; Escape
//! closes a projector that isn't full screen. A projector opened full screen in the
//! desktop app opens as a full-screen window (`apps/jstream-desktop`); a browser only
//! lets a page fill the screen after a click on it, so there it opens as a window as big
//! as the screen and says so.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#dialogs

use crate::app::SessionContext;
use crate::canvas::screen::Screen;
use crate::store::Studio;
use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, BinaryFrame, Call, PreviewCodec, PreviewSettings, SceneId, SceneRole,
};
use jstream_control::{BinaryStream, Control};
use leptos::html::Canvas;
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// The query key that makes the page a projector.
const KEY: &str = "projector";
/// The scenes a multiview shows below program and preview, at most.
pub const MULTIVIEW_SCENES: usize = 8;

/// What a projector shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The program mix.
    Program,
    /// One scene, on program or not.
    Scene(SceneId),
    /// Program, preview in studio mode, and the scenes marked "Show in multiview".
    Multiview,
}

/// A projector page's query: what it shows, and whether it was opened full screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// What it shows.
    pub target: Target,
    /// Opened full screen.
    pub full: bool,
}

impl Request {
    /// The query string (with its `?`) of the page that shows this.
    pub fn query(&self) -> String {
        let target = match &self.target {
            Target::Program => "program".to_owned(),
            Target::Multiview => "multiview".to_owned(),
            Target::Scene(id) => format!("scene:{}", encode(id.as_str())),
        };
        let full = if self.full { "&full=1" } else { "" };
        format!("?{KEY}={target}{full}")
    }

    /// The request a page's query string (`location.search`, with or without its `?`)
    /// carries, or `None` for a page that is not a projector.
    pub fn parse(search: &str) -> Option<Request> {
        let mut target = None;
        let mut full = false;
        for pair in search.trim_start_matches('?').split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                KEY => {
                    target = Some(match value {
                        "program" => Target::Program,
                        "multiview" => Target::Multiview,
                        _ => {
                            let id = decode(value.strip_prefix("scene:")?)?;
                            if id.is_empty() {
                                return None;
                            }
                            Target::Scene(SceneId::new(id))
                        }
                    });
                }
                "full" => full = value == "1",
                _ => {}
            }
        }
        Some(Request {
            target: target?,
            full,
        })
    }

    /// The window name it opens under, so opening the same projector again brings that
    /// window back instead of opening another.
    pub fn window_name(&self) -> String {
        match &self.target {
            Target::Program => "jstream-projector-program".to_owned(),
            Target::Multiview => "jstream-projector-multiview".to_owned(),
            Target::Scene(id) => format!("jstream-projector-scene-{}", encode(id.as_str())),
        }
    }
}

/// Percent-encodes everything but the URL's unreserved characters.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Undoes [`encode`] (and any other percent-encoding); `None` for a malformed escape
/// or text that isn't UTF-8.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The request this page carries, if it is a projector.
pub fn page_request() -> Option<Request> {
    Request::parse(&window().location().search().ok()?)
}

/// Opens `request`'s projector: a window of its own, or, `full`, one as big as the
/// screen the page is on, which the desktop app makes full screen. Returns what to tell
/// the user, or the error when the browser refused to open a window.
pub fn open(request: &Request) -> Result<String, String> {
    let window = window();
    let screen = |key: &str| {
        let screen = js_sys_get(&window, "screen");
        js_sys_get(&screen, key).as_f64().unwrap_or(0.0).round() as i64
    };
    let features = if request.full {
        format!(
            "popup=yes,left={},top={},width={},height={}",
            screen("availLeft"),
            screen("availTop"),
            screen("width").max(320),
            screen("height").max(180),
        )
    } else {
        "popup=yes,width=960,height=540".to_owned()
    };
    let opened = window
        .open_with_url_and_target_and_features(&request.query(), &request.window_name(), &features)
        .ok()
        .flatten();
    match opened {
        Some(_) if request.full && crate::host::desktop().is_none() => {
            Ok("Projector opened. Double-click it to fill the screen; Esc closes it.".into())
        }
        Some(_) => Ok("Projector opened. Esc on it closes it.".into()),
        None => Err("The browser didn't open the projector window. Allow pop-ups for JStream, then try again.".into()),
    }
}

/// Opens `request`'s projector and reports how it went: a toast, or the problem bar.
pub fn open_and_report(studio: Studio, request: &Request) {
    match open(request) {
        Ok(note) => studio.notify(note),
        Err(problem) => studio.problem.set(Some(problem)),
    }
}

/// The Pop out submenu of a scene (spec-workspace.md 5.2): the scene in a window, or
/// full screen.
pub fn pop_out(studio: Studio, scene_id: Option<SceneId>) -> crate::menu::Entry {
    use crate::menu::Entry;
    let entries = scene_id
        .map(|scene_id| {
            let windowed = Request {
                target: Target::Scene(scene_id),
                full: false,
            };
            let full = Request {
                full: true,
                ..windowed.clone()
            };
            vec![
                Entry::item("Scene in a window", move || {
                    open_and_report(studio, &windowed)
                }),
                Entry::Head("FULL SCREEN".into()),
                Entry::item("Scene full screen", move || open_and_report(studio, &full)),
            ]
        })
        .unwrap_or_default();
    Entry::submenu("Pop out", entries)
}

/// `target[key]`, or `undefined`.
fn js_sys_get(target: &leptos::wasm_bindgen::JsValue, key: &str) -> leptos::wasm_bindgen::JsValue {
    leptos::web_sys::js_sys::Reflect::get(target, &leptos::wasm_bindgen::JsValue::from_str(key))
        .unwrap_or(leptos::wasm_bindgen::JsValue::UNDEFINED)
}

/// The `preview.subscribe` for one projector picture: `scene`, or the program mix for
/// `None`, at most `max` pixels and `fps` pictures a second, H.264 first when the
/// browser can decode it.
pub fn subscribe(
    channel: u32,
    scene: Option<SceneId>,
    (width, height): (u32, u32),
    fps: u32,
    h264: bool,
) -> Action {
    let codecs = if h264 {
        vec![PreviewCodec::H264, PreviewCodec::Jpeg]
    } else {
        vec![PreviewCodec::Jpeg]
    };
    Action::PreviewSubscribe {
        channel,
        max_width: Some(width.clamp(
            *PreviewSettings::WIDTH.start(),
            *PreviewSettings::WIDTH.end(),
        )),
        max_height: Some(height.clamp(
            *PreviewSettings::HEIGHT.start(),
            *PreviewSettings::HEIGHT.end(),
        )),
        fps: Some(fps.clamp(*PreviewSettings::FPS.start(), *PreviewSettings::FPS.end())),
        codecs,
        scene_id: scene,
        source: None,
    }
}

/// The scenes a multiview shows below program and preview: those marked "Show in
/// multiview", not groups, in display order, at most [`MULTIVIEW_SCENES`].
pub fn multiview_scenes<'a>(
    scenes: impl IntoIterator<Item = &'a jstream_control::protocol::SceneInfo>,
) -> Vec<(SceneId, String)> {
    scenes
        .into_iter()
        .filter(|scene| scene.role != SceneRole::Group && scene.meta.show_in_multiview)
        .take(MULTIVIEW_SCENES)
        .map(|scene| (scene.scene_id.clone(), scene.name.clone()))
        .collect()
}

/// What draws one channel's frames.
type Handler = Box<dyn FnMut(&BinaryFrame)>;

/// The session's one binary channel, shared by every picture of the projector: each
/// picture takes a channel number and gets the frames that carry it.
#[derive(Clone)]
pub struct Channels {
    handlers: Rc<RefCell<HashMap<u32, Handler>>>,
    next: Rc<Cell<u32>>,
}

impl Channels {
    /// Reads `frames` until it ends, handing each frame to its channel's handler.
    fn pump(frames: BinaryStream) -> Channels {
        let channels = Channels {
            handlers: Rc::default(),
            next: Rc::new(Cell::new(1)),
        };
        let handlers = Rc::downgrade(&channels.handlers);
        spawn_local(async move {
            let mut frames = frames;
            while let Some(frame) = frames.next().await {
                let Some(handlers) = handlers.upgrade() else {
                    return;
                };
                // Taken out while it runs, so a handler may add or drop another.
                let handler = handlers.borrow_mut().remove(&frame.header.channel);
                if let Some(mut handler) = handler {
                    handler(&frame);
                    if !frame.header.end {
                        handlers
                            .borrow_mut()
                            .entry(frame.header.channel)
                            .or_insert(handler);
                    }
                }
            }
        });
        channels
    }

    /// A channel number no picture of this page has used.
    fn take(&self) -> u32 {
        let channel = self.next.get();
        self.next.set(channel.wrapping_add(1).max(1));
        channel
    }
}

/// The page's [`Channels`], provided as context to its pictures.
#[derive(Clone, Copy)]
struct ChannelsContext(StoredValue<Channels, LocalStorage>);

/// How a picture stands, for its placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Starting,
    Live,
    Ended(String),
}

/// Calls `action` on `control`, if the session is still open.
async fn act(control: &Weak<dyn Control>, action: Action) -> Option<Result<(), String>> {
    let control = control.upgrade()?;
    Some(
        control
            .call(Call::Action { action })
            .await
            .map(drop)
            .map_err(|error| error.to_string()),
    )
}

/// One picture: a stream of `scene` (the program mix for `None`) drawn into a canvas
/// that fills its box, keeping its aspect ratio.
#[component]
fn Picture(
    scene: Option<SceneId>,
    /// The largest picture to ask for.
    max: (u32, u32),
    fps: u32,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let shared = expect_context::<ChannelsContext>();
    let channels = shared.0.get_value();
    let canvas = NodeRef::<Canvas>::new();
    let shown = RwSignal::new(Shown::Starting);
    let channel = channels.take();
    let control: Option<Weak<dyn Control>> = session.get().map(|s| Rc::downgrade(&s.control));
    canvas.on_load({
        let channels = channels.clone();
        let control = control.clone();
        move |element| {
            let Some(control) = control else {
                return;
            };
            let Some(screen) = Screen::new(element, move || {
                if shown.try_get_untracked() == Some(Shown::Starting) {
                    shown.try_set(Shown::Live);
                }
            }) else {
                shown.set(Shown::Ended("this browser can't draw on a canvas".into()));
                return;
            };
            let h264 = Rc::new(Cell::new(Screen::supports_h264()));
            let screen = Rc::new(RefCell::new(screen));
            let handler: Handler = {
                let control = control.clone();
                let h264 = Rc::clone(&h264);
                let scene = scene.clone();
                Box::new(move |frame| {
                    let header = frame.header;
                    if header.end {
                        let reason = String::from_utf8_lossy(&frame.payload).into_owned();
                        shown.try_set(Shown::Ended(reason));
                        return;
                    }
                    let mut screen = screen.borrow_mut();
                    if let Err(reason) = screen.show(&header, &frame.payload)
                        && !screen.h264_failed()
                    {
                        shown.try_set(Shown::Ended(reason));
                        return;
                    }
                    if h264.get() && screen.h264_failed() {
                        // Once, for JPEG: the channel is closed first so it can be
                        // reused.
                        h264.set(false);
                        let control = control.clone();
                        let scene = scene.clone();
                        spawn_local(async move {
                            let close = Action::PreviewUnsubscribe { channel };
                            let _closed = act(&control, close).await;
                            let again = subscribe(channel, scene, max, fps, false);
                            if let Some(Err(error)) = act(&control, again).await {
                                shown.try_set(Shown::Ended(error));
                            }
                        });
                    }
                })
            };
            channels.handlers.borrow_mut().insert(channel, handler);
            let scene = scene.clone();
            spawn_local(async move {
                let open = subscribe(channel, scene, max, fps, h264.get());
                if let Some(Err(error)) = act(&control, open).await {
                    shown.try_set(Shown::Ended(error));
                }
            });
        }
    });
    // Handles only: the cleanup runs where any other may.
    on_cleanup(move || {
        shared
            .0
            .try_with_value(|channels| channels.handlers.borrow_mut().remove(&channel));
        if let Some(open) = session.get() {
            let control = Rc::downgrade(&open.control);
            spawn_local(async move {
                let _closed = act(&control, Action::PreviewUnsubscribe { channel }).await;
            });
        }
    });
    view! {
        <div class="projector-picture">
            <canvas node_ref=canvas></canvas>
            {move || match shown.get() {
                Shown::Live => None,
                Shown::Starting => Some("Starting…".to_owned()),
                Shown::Ended(reason) => Some(reason),
            }.map(|note| view! { <p class="projector-note">{note}</p> })}
        </div>
    }
}

/// The largest picture a full-window projector asks for: the screen in device pixels,
/// at most what a stream may be.
fn screen_pixels() -> (u32, u32) {
    let window = window();
    let ratio = window.device_pixel_ratio().max(1.0);
    let screen = js_sys_get(&window, "screen");
    let side = |key: &str, fallback: f64| {
        let css = js_sys_get(&screen, key).as_f64().unwrap_or(fallback);
        (css * ratio).round().max(16.0) as u32
    };
    (side("width", 1920.0), side("height", 1080.0))
}

/// Whether the page fills the screen through the Fullscreen API.
fn is_full_screen() -> bool {
    !js_sys_get(&document(), "fullscreenElement").is_null()
        && !js_sys_get(&document(), "fullscreenElement").is_undefined()
}

/// Fills the screen, or leaves full screen.
fn toggle_full_screen() {
    use leptos::wasm_bindgen::JsCast;
    let (target, method) = if is_full_screen() {
        (
            leptos::wasm_bindgen::JsValue::from(document()),
            "exitFullscreen",
        )
    } else {
        let root = document()
            .document_element()
            .map(leptos::wasm_bindgen::JsValue::from);
        let Some(root) = root else {
            return;
        };
        (root, "requestFullscreen")
    };
    if let Ok(function) =
        js_sys_get(&target, method).dyn_into::<leptos::web_sys::js_sys::Function>()
    {
        // A refusal (no click to allow it) leaves the window as it is.
        let _asked = function.call0(&target);
    }
}

/// The projector page: what `request` shows, filling the window, once a session is open.
#[component]
pub fn Projector(
    /// What it shows.
    request: Request,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let studio = expect_context::<Studio>();
    let Some(open) = session.get() else {
        return ().into_any();
    };
    let channels = Channels::pump(open.control.binary());
    drop(open);
    provide_context(ChannelsContext(StoredValue::new_local(channels)));

    let name_of = move |id: &SceneId| {
        studio.collection.with(|c| {
            c.as_ref()
                .and_then(|c| c.scenes.iter().find(|s| &s.scene_id == id))
                .map(|s| s.name.clone())
        })
    };
    let title = {
        let target = request.target.clone();
        move || match &target {
            Target::Program => "Program".to_owned(),
            Target::Multiview => "Multiview".to_owned(),
            Target::Scene(id) => {
                format!("Scene: {}", name_of(id).unwrap_or_else(|| id.to_string()))
            }
        }
    };
    let page_title = title.clone();
    Effect::new(move |_| document().set_title(&format!("{} · JStream projector", page_title())));

    // The hint shows while the pointer moves, and a moment after.
    let hint = RwSignal::new(true);
    let moved = StoredValue::new(0_u64);
    let show_hint = move || {
        hint.set(true);
        moved.update_value(|n| *n += 1);
        let mine = moved.get_value();
        set_timeout(
            move || {
                if moved.try_get_value() == Some(mine) {
                    hint.try_set(false);
                }
            },
            std::time::Duration::from_millis(2_000),
        );
    };
    show_hint();
    let escape = window_event_listener(leptos::ev::keydown, move |event| {
        if event.key() == "Escape" && !is_full_screen() {
            let _closed = window().close();
        }
    });
    on_cleanup(move || escape.remove());

    let content = match request.target {
        Target::Program => view! { <Picture scene=None max=screen_pixels() fps=30 /> }.into_any(),
        Target::Scene(id) => {
            view! { <Picture scene=Some(id) max=screen_pixels() fps=30 /> }.into_any()
        }
        Target::Multiview => view! { <Multiview /> }.into_any(),
    };
    view! {
        <main
            class="projector"
            class:quiet=move || !hint.get()
            on:pointermove=move |_| show_hint()
            on:dblclick=move |_| toggle_full_screen()
        >
            {content}
            <div class="projector-hint" class:shown=move || hint.get()>
                <span class="what">{title}</span>
                " · Double-click: full screen · Esc: close"
            </div>
        </main>
    }
    .into_any()
}

/// A multiview: program, with preview beside it in studio mode, over the scenes marked
/// "Show in multiview". A scene on program has a red frame and one on preview a green one.
#[component]
fn Multiview() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let studio_mode = RwSignal::new(crate::header::kept_studio_mode());
    crate::header::on_kept_studio_mode(move |on| studio_mode.set(on));
    let scenes = Memo::new(move |_| {
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| multiview_scenes(&c.scenes))
                .unwrap_or_default()
        })
    });
    let name_of = move |id: &SceneId| {
        studio.collection.with(|c| {
            c.as_ref()
                .and_then(|c| c.scenes.iter().find(|s| &s.scene_id == id))
                .map(|s| s.name.clone())
                .unwrap_or_default()
        })
    };
    let preview = move || {
        studio.preview.get().map(|id| {
            let name = name_of(&id);
            view! {
                <figure class="tile preview">
                    <Picture scene=Some(id) max=(960, 540) fps=30 />
                    <figcaption>"Preview · "{name}</figcaption>
                </figure>
            }
        })
    };
    let program_name = move || {
        studio
            .program
            .get()
            .map(|id| name_of(&id))
            .unwrap_or_default()
    };
    view! {
        <div class="multiview" class:studio=move || studio_mode.get()>
            <div class="multiview-top">
                {move || studio_mode.get().then(preview).flatten()}
                <figure class="tile program">
                    <Picture scene=None max=(960, 540) fps=30 />
                    <figcaption>"Program · "{program_name}</figcaption>
                </figure>
            </div>
            <div class="multiview-scenes">
                <For
                    each=move || scenes.get()
                    key=|(id, _)| id.clone()
                    children=move |(id, name)| {
                        let on_program = {
                            let id = id.clone();
                            move || studio.program.get().as_ref() == Some(&id)
                        };
                        let on_preview = {
                            let id = id.clone();
                            move || studio_mode.get() && studio.preview.get().as_ref() == Some(&id)
                        };
                        view! {
                            <figure class="tile" class:live=on_program class:cued=on_preview>
                                <Picture scene=Some(id) max=(480, 270) fps=15 />
                                <figcaption>{name}</figcaption>
                            </figure>
                        }
                    }
                />
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::{MULTIVIEW_SCENES, Request, Target, decode, encode, multiview_scenes, subscribe};
    use jstream_control::protocol::{Action, PreviewCodec, SceneId, SceneInfo, SceneRole};

    fn request(target: Target, full: bool) -> Request {
        Request { target, full }
    }

    #[test]
    fn a_request_goes_through_its_query() {
        for request in [
            request(Target::Program, false),
            request(Target::Multiview, true),
            request(Target::Scene(SceneId::new("scn_1")), false),
            request(Target::Scene(SceneId::new("a b&c=d/é")), true),
        ] {
            assert_eq!(Request::parse(&request.query()), Some(request.clone()));
        }
        assert_eq!(
            request(Target::Program, true).query(),
            "?projector=program&full=1"
        );
        assert_eq!(
            request(Target::Scene(SceneId::new("a b")), false).query(),
            "?projector=scene:a%20b"
        );
    }

    #[test]
    fn other_pages_are_not_projectors() {
        assert_eq!(Request::parse(""), None);
        assert_eq!(Request::parse("?full=1"), None);
        assert_eq!(Request::parse("?projector=nothing"), None);
        assert_eq!(Request::parse("?projector=scene:"), None);
        assert_eq!(Request::parse("?projector=scene:%zz"), None);
        assert_eq!(
            Request::parse("x=1&projector=program"),
            Some(request(Target::Program, false))
        );
    }

    #[test]
    fn each_target_has_its_own_window() {
        let names: Vec<String> = [
            Target::Program,
            Target::Multiview,
            Target::Scene(SceneId::new("a")),
            Target::Scene(SceneId::new("b")),
        ]
        .into_iter()
        .map(|target| request(target, false).window_name())
        .collect();
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name), "{name} twice");
        }
        assert_eq!(
            request(Target::Program, true).window_name(),
            request(Target::Program, false).window_name()
        );
    }

    #[test]
    fn encoding_round_trips() {
        for text in ["", "scn_1", "a b", "100%", "日本"] {
            assert_eq!(decode(&encode(text)).as_deref(), Some(text));
        }
        assert_eq!(decode("%"), None);
        assert_eq!(decode("%ff"), None);
    }

    #[test]
    fn a_picture_asks_for_what_a_stream_may_be() {
        let Action::PreviewSubscribe {
            channel,
            max_width,
            max_height,
            fps,
            codecs,
            scene_id,
            ..
        } = subscribe(3, Some(SceneId::new("s")), (3840, 2160), 60, true)
        else {
            panic!("a subscribe");
        };
        assert_eq!(channel, 3);
        assert_eq!((max_width, max_height), (Some(1920), Some(1080)));
        assert_eq!(fps, Some(30));
        assert_eq!(codecs, vec![PreviewCodec::H264, PreviewCodec::Jpeg]);
        assert_eq!(scene_id, Some(SceneId::new("s")));
        let Action::PreviewSubscribe {
            codecs, scene_id, ..
        } = subscribe(1, None, (640, 360), 15, false)
        else {
            panic!("a subscribe");
        };
        assert_eq!(codecs, vec![PreviewCodec::Jpeg]);
        assert_eq!(scene_id, None);
    }

    fn scene(id: &str, role: SceneRole, shown: bool) -> SceneInfo {
        let mut scene: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": id,
            "name": id.to_uppercase(),
            "items": [],
        }))
        .expect("a scene");
        scene.role = role;
        scene.meta.show_in_multiview = shown;
        scene
    }

    #[test]
    fn a_multiview_shows_the_scenes_marked_for_it() {
        let scenes = vec![
            scene("a", SceneRole::Scene, true),
            scene("grp", SceneRole::Group, true),
            scene("hidden", SceneRole::Scene, false),
            scene("b", SceneRole::Scene, true),
        ];
        assert_eq!(
            multiview_scenes(&scenes),
            vec![
                (SceneId::new("a"), "A".to_owned()),
                (SceneId::new("b"), "B".to_owned())
            ]
        );
        let many: Vec<SceneInfo> = (0..12)
            .map(|i| scene(&format!("s{i}"), SceneRole::Scene, true))
            .collect();
        assert_eq!(multiview_scenes(&many).len(), MULTIVIEW_SCENES);
    }
}
