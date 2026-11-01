//! The Properties dialog's live picture of a display, window, webcam or capture card
//! (DL-02): what the settings being edited capture, before OK saves them, so the user
//! sees the device they pick. It is a `preview.subscribe` stream of the draft source
//! (`source`, protocol 0.49) on a channel tapped from the canvas's binary channel
//! ([`taps`](crate::canvas::taps)), and it follows the draft: a change of what is
//! captured closes the stream and opens the next on a new channel. The engine opens
//! the capture for the stream alone, sharing a webcam already open; the picture never
//! reaches program.
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use crate::app::SessionContext;
use crate::canvas::screen::Screen;
use crate::canvas::taps::{Handler, PreviewTaps};
use jstream_control::Control;
use jstream_control::protocol::{Action, Call, PreviewCodec, PreviewDraft, Settings};
use leptos::html::Canvas;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// The picture asked for: small, as the dialog shows it, at the canvas's 16:9.
const MAX: (u32, u32) = (640, 360);
/// Pictures per second: enough to judge a camera's framing and motion.
const FPS: u32 = 15;

/// Whether the dialog shows a picture for sources of `kind_id`: the capture kinds.
pub fn shows(kind_id: &str) -> bool {
    matches!(kind_id, "display" | "window" | "webcam")
}

/// What of the draft the picture shows: the settings a capture reads. The name and
/// anything else in the dialog don't open a new stream.
pub fn captured(kind_id: &str, settings: &Settings) -> Settings {
    let keys: &[&str] = match kind_id {
        "webcam" => &["device"],
        _ => &["target", "show_cursor"],
    };
    Settings(
        settings
            .0
            .iter()
            .filter(|(key, _)| keys.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// The note over a picture that has nothing to show yet, when the settings explain it:
/// a window source with no window picked captures nothing.
pub fn hint(kind_id: &str, settings: &Settings) -> Option<&'static str> {
    let empty = |key: &str| {
        settings
            .get(key)
            .and_then(Value::as_str)
            .is_none_or(|v| v.trim().is_empty())
    };
    (kind_id == "window" && empty("target")).then_some("Pick a window to see it here.")
}

/// The `preview.subscribe` for the draft `settings` of a `kind_id` source on `channel`.
pub fn subscribe(channel: u32, kind_id: &str, settings: Settings, h264: bool) -> Action {
    let codecs = if h264 {
        vec![PreviewCodec::H264, PreviewCodec::Jpeg]
    } else {
        vec![PreviewCodec::Jpeg]
    };
    Action::PreviewSubscribe {
        channel,
        max_width: Some(MAX.0),
        max_height: Some(MAX.1),
        fps: Some(FPS),
        codecs,
        scene_id: None,
        source: Some(PreviewDraft {
            kind: kind_id.to_owned(),
            settings,
        }),
        source_id: None,
    }
}

/// How the picture stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Starting,
    Live,
    Ended(String),
}

/// Whether a subscribe for `channel` may still go out, with `open` the dialog's open
/// channel now (`None` once it closed, or its component is gone). A close awaited
/// before the subscribe lets the settings change again or the dialog close meanwhile:
/// their unsubscribe of `channel` reaches the engine first and does nothing, so a
/// subscribe sent after it would open a stream nobody closes.
fn still_open(open: Option<u32>, channel: u32) -> bool {
    open == Some(channel)
}

/// How the picture stands once a frame of its stream was drawn: a stream is live from
/// its first picture. The screen's own first-picture call comes once, for the first
/// stream, so a stream opened after a change of device needs this.
fn after_picture(shown: &Shown) -> Option<Shown> {
    (*shown == Shown::Starting).then_some(Shown::Live)
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

/// The picture of what `draft` captures, for a `kind_id` source.
#[component]
pub fn DevicePreview(
    /// The source's kind: `display`, `window` or `webcam`.
    kind_id: String,
    /// The settings the dialog is editing.
    draft: RwSignal<Settings>,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let shared_taps = use_context::<PreviewTaps>();
    let taps = shared_taps.and_then(|taps| taps.get());
    let control: Option<Weak<dyn Control>> = session.get().map(|s| Rc::downgrade(&s.control));
    let canvas = NodeRef::<Canvas>::new();
    let shown = RwSignal::new(Shown::Starting);
    let wanted = {
        let kind_id = kind_id.clone();
        Memo::new(move |_| draft.with(|d| captured(&kind_id, d)))
    };
    let hint = {
        let kind_id = kind_id.clone();
        move || wanted.with(|w| hint(&kind_id, w))
    };
    // The open stream's channel, and whether H.264 is still asked for.
    let open = StoredValue::new_local(None::<u32>);
    let h264 = Rc::new(Cell::new(Screen::supports_h264()));
    let screen = Rc::new(RefCell::new(None::<Screen>));

    canvas.on_load({
        let screen = Rc::clone(&screen);
        move |element| {
            let drawn = Screen::new(element, move || {
                if shown.try_get_untracked() == Some(Shown::Starting) {
                    shown.try_set(Shown::Live);
                }
            });
            if drawn.is_none() {
                shown.set(Shown::Ended("this browser can't draw on a canvas".into()));
            }
            *screen.borrow_mut() = drawn;
        }
    });

    // Opens a stream of what the draft captures now, closing the one before.
    let reopen = {
        let h264 = Rc::clone(&h264);
        let screen = Rc::clone(&screen);
        let taps = taps.clone();
        let control = control.clone();
        let kind_id = kind_id.clone();
        Rc::new(move |settings: Settings| {
            let (Some(taps), Some(control)) = (taps.clone(), control.clone()) else {
                shown.set(Shown::Ended("the preview isn't available here".into()));
                return;
            };
            let before = open.try_update_value(|open| open.take()).flatten();
            if let Some(before) = before {
                taps.remove(before);
            }
            let channel = taps.take();
            open.try_set_value(Some(channel));
            shown.set(Shown::Starting);
            let handler: Handler = {
                let screen = Rc::clone(&screen);
                let h264 = Rc::clone(&h264);
                let control = control.clone();
                let kind_id = kind_id.clone();
                let settings = settings.clone();
                Box::new(move |frame| {
                    let header = frame.header;
                    if header.end {
                        let reason = String::from_utf8_lossy(&frame.payload).into_owned();
                        shown.try_set(Shown::Ended(reason));
                        return;
                    }
                    let mut screen = screen.borrow_mut();
                    let Some(screen) = screen.as_mut() else {
                        return;
                    };
                    if let Err(reason) = screen.show(&header, &frame.payload)
                        && !screen.h264_failed()
                    {
                        shown.try_set(Shown::Ended(reason));
                        return;
                    }
                    if let Some(next) = shown.try_get_untracked().as_ref().and_then(after_picture) {
                        shown.try_set(next);
                    }
                    if h264.get() && screen.h264_failed() {
                        // Once, for JPEG: the channel is closed first so it can be
                        // reused.
                        h264.set(false);
                        let control = control.clone();
                        let kind_id = kind_id.clone();
                        let settings = settings.clone();
                        spawn_local(async move {
                            let close = Action::PreviewUnsubscribe { channel };
                            let _closed = act(&control, close).await;
                            if !still_open(open.try_get_value().flatten(), channel) {
                                return;
                            }
                            let again = subscribe(channel, &kind_id, settings, false);
                            if let Some(Err(error)) = act(&control, again).await {
                                shown.try_set(Shown::Ended(error));
                            }
                        });
                    }
                })
            };
            taps.insert(channel, handler);
            let open_now = subscribe(channel, &kind_id, settings, h264.get());
            spawn_local(async move {
                if let Some(before) = before {
                    let _closed =
                        act(&control, Action::PreviewUnsubscribe { channel: before }).await;
                    // Checked and sent with nothing between: `call` sends at once.
                    if !still_open(open.try_get_value().flatten(), channel) {
                        return;
                    }
                }
                if let Some(Err(error)) = act(&control, open_now).await {
                    shown.try_set(Shown::Ended(error));
                }
            });
        })
    };
    {
        let reopen = Rc::clone(&reopen);
        Effect::new(move |_| {
            let settings = wanted.get();
            reopen(settings);
        });
    }
    // Handles only: the cleanup runs where any other may.
    on_cleanup(move || {
        let Some(channel) = open.try_update_value(|open| open.take()).flatten() else {
            return;
        };
        if let Some(taps) = shared_taps.and_then(|taps| taps.get()) {
            taps.remove(channel);
        }
        if let Some(open) = session.get() {
            let control = Rc::downgrade(&open.control);
            spawn_local(async move {
                let _closed = act(&control, Action::PreviewUnsubscribe { channel }).await;
            });
        }
    });

    view! {
        <div class="props-preview" aria-label="Preview">
            <canvas node_ref=canvas></canvas>
            {move || {
                let note = match shown.get() {
                    Shown::Live => None,
                    Shown::Starting => Some(hint().unwrap_or("Starting…").to_owned()),
                    Shown::Ended(reason) => Some(reason),
                };
                note.map(|note| view! { <p class="props-preview-note">{note}</p> })
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::{FPS, MAX, Shown, after_picture, captured, hint, shows, still_open, subscribe};
    use jstream_control::protocol::{Action, PreviewCodec, PreviewDraft, Settings};
    use serde_json::json;

    fn settings(value: serde_json::Value) -> Settings {
        serde_json::from_value(value).expect("settings")
    }

    #[test]
    fn only_capture_kinds_show_a_picture() {
        assert!(shows("display") && shows("window") && shows("webcam"));
        assert!(!shows("color") && !shows("image") && !shows("browser"));
    }

    #[test]
    fn the_picture_follows_only_what_the_capture_reads() {
        let draft = settings(json!({"device": "cam-2", "extra": 1}));
        assert_eq!(
            captured("webcam", &draft),
            settings(json!({"device": "cam-2"}))
        );
        let draft = settings(json!({"target": "w-1", "show_cursor": false, "x": 2}));
        assert_eq!(
            captured("window", &draft),
            settings(json!({"target": "w-1", "show_cursor": false}))
        );
    }

    #[test]
    fn a_window_with_none_picked_says_so() {
        assert!(hint("window", &settings(json!({"target": " "}))).is_some());
        assert!(hint("window", &Settings::new()).is_some());
        assert_eq!(hint("window", &settings(json!({"target": "w-1"}))), None);
        assert_eq!(
            hint("display", &Settings::new()),
            None,
            "the primary display"
        );
    }

    #[test]
    fn a_subscribe_waits_for_a_close_only_while_its_channel_is_still_open() {
        assert!(still_open(Some(7), 7));
        assert!(
            !still_open(Some(8), 7),
            "the settings changed again meanwhile"
        );
        assert!(!still_open(None, 7), "the dialog closed meanwhile");
    }

    #[test]
    fn each_stream_goes_live_on_its_first_picture() {
        assert_eq!(after_picture(&Shown::Starting), Some(Shown::Live));
        assert_eq!(after_picture(&Shown::Live), None);
        assert_eq!(
            after_picture(&Shown::Ended("gone".into())),
            None,
            "an end stays"
        );
    }

    #[test]
    fn the_request_names_the_draft_and_no_scene() {
        let draft = settings(json!({"device": "cam-2"}));
        let Action::PreviewSubscribe {
            channel,
            max_width,
            max_height,
            fps,
            codecs,
            scene_id,
            source,
            source_id: None,
        } = subscribe(7, "webcam", draft.clone(), true)
        else {
            panic!("a subscribe");
        };
        assert_eq!(channel, 7);
        assert_eq!(
            (max_width, max_height, fps),
            (Some(MAX.0), Some(MAX.1), Some(FPS))
        );
        assert_eq!(codecs, vec![PreviewCodec::H264, PreviewCodec::Jpeg]);
        assert_eq!(scene_id, None);
        assert_eq!(
            source,
            Some(PreviewDraft {
                kind: "webcam".into(),
                settings: draft.clone(),
            })
        );
        let Action::PreviewSubscribe { codecs, .. } = subscribe(8, "webcam", draft, false) else {
            panic!("a subscribe");
        };
        assert_eq!(
            codecs,
            vec![PreviewCodec::Jpeg],
            "JPEG only once H.264 failed"
        );
    }
}
