//! Studio mode's program monitor: the program mix, live, beside the canvas, which shows
//! the preview scene. It is a `preview.subscribe` of the program mix (no `scene_id`) on a
//! channel tapped from the canvas's binary channel ([`taps`](super::taps)), sized for
//! its box as the canvas's own stream is ([`preview::stage`]), so the desktop app's own
//! engine gives the sharpest picture and any other engine only what the box shows. A new
//! base rate, or a box that changed by a quarter or more once the newest resize has
//! settled, closes the stream and opens the next on a new tapped channel
//! ([`preview::Stream::at_fps`], [`preview::Stream::at_stage`]). A new scene moves the
//! open stream there on its channel (`preview.show`), keeping the last picture until the
//! new one's first; a decoder that falls behind asks for a keyframe
//! (`preview.keyframe`).
//!
//! Docs: crates/jstream-ui/README.md#canvas

use super::preview::{self, Stream};
use super::screen::Screen;
use super::taps::{Handler, PreviewTaps};
use crate::app::SessionContext;
use crate::store::Studio;
use jstream_control::Control;
use jstream_control::protocol::{Action, Call};
use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// How the monitor's picture stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Starting,
    Live,
    Ended(String),
}

/// The stream to open in place of `current` for the base rate `fps` and the box
/// `stage`, or `None` while `current` still fits both. The next stream keeps the same
/// picture; its channel is set by the caller, from the taps.
pub fn reopened(current: &Stream, fps: u32, stage: (f32, f32, f32)) -> Option<Stream> {
    let at_fps = current.at_fps(fps);
    let base = at_fps.as_ref().unwrap_or(current);
    match base.at_stage(stage) {
        Some(next) => Some(next),
        None => at_fps,
    }
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

/// The program monitor: the program mix, drawn into a canvas that fills its box.
#[component]
pub fn ProgramMonitor() -> impl IntoView {
    view! { <Monitor scene=Signal::stored(None) still=false label="program" /> }
}

/// A monitor of `scene` (the program mix while it is `None`), drawn into a canvas that
/// fills its box, following the scene as it changes. A `still` monitor keeps one
/// picture: it closes its stream once the first frame is drawn, and opens one again
/// only when the scene changes, so it costs the engine one frame per change (the Show
/// panel's Next monitor set to Still). `label` names it in `preview.report`.
#[component]
pub fn Monitor(
    /// The scene to show, or `None` for the program mix.
    scene: Signal<Option<jstream_control::protocol::SceneId>>,
    /// Whether to keep one picture rather than a moving one.
    still: bool,
    /// The monitor's name in `preview.report`.
    label: &'static str,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let studio = expect_context::<Studio>();
    let shared_taps = use_context::<PreviewTaps>();
    let taps = shared_taps.and_then(|taps| taps.get());
    let control: Option<Weak<dyn Control>> = session.get().map(|s| Rc::downgrade(&s.control));
    let canvas = NodeRef::<Canvas>::new();
    let frame = NodeRef::<Div>::new();
    let shown = RwSignal::new(Shown::Starting);
    // The open stream: its channel is a tap's.
    let open = StoredValue::new_local(None::<Stream>);
    let screen = Rc::new(RefCell::new(None::<Screen>));
    let live_fps = Memo::new(move |_| {
        studio
            .collection
            .with(|c| preview::live_fps(c.as_ref().map_or(preview::FPS, |c| c.base_fps)))
    });
    // What the monitor did with its pictures, reported to the engine (`preview.report`,
    // ADR-0132), as the canvas's are.
    let tally = crate::views::ViewTally::default();
    {
        let tally = tally.clone();
        Effect::new(move |before: Option<Shown>| {
            let now = shown.get();
            if before.as_ref() != Some(&now) {
                match now {
                    Shown::Starting => tally.placeholder(),
                    Shown::Ended(_) => tally.blank(),
                    Shown::Live => {}
                }
            }
            now
        });
    }
    let reporting = StoredValue::new_local(None::<IntervalHandle>);
    // Whether the session is the desktop app's own engine, whose pictures cross no
    // network: the stream is then always the full picture, as the canvas's is.
    let connected = use_context::<crate::host::ConnectedEngine>();
    // The newest resize's timeout: only it restages, so a long drag opens one stream.
    let resizes = StoredValue::new(0_u64);
    let measured = move || {
        let (width, height) = frame.get_untracked().map_or((640.0, 360.0), |frame| {
            (frame.client_width() as f32, frame.client_height() as f32)
        });
        preview::stage(
            (width, height, crate::ui_scale::pixel_ratio() as f32),
            crate::host::own_engine(connected),
        )
    };

    // Opens `next` on a new tapped channel, closing the stream before it.
    let reopen: Rc<dyn Fn(Stream)> = {
        let screen = Rc::clone(&screen);
        let taps = taps.clone();
        let control = control.clone();
        let tally = tally.clone();
        Rc::new(move |next: Stream| {
            let (Some(taps), Some(control)) = (taps.clone(), control.clone()) else {
                shown.set(Shown::Ended(
                    "the program picture isn't available here".into(),
                ));
                return;
            };
            let before = open.try_update_value(Option::take).flatten();
            if let Some(before) = &before {
                taps.remove(before.channel);
            }
            let next = Stream {
                channel: taps.take(),
                ..next
            };
            open.try_set_value(Some(next.clone()));
            let handler = frames(&next, &screen, &control, open, shown);
            taps.insert(next.channel, handler);
            tally.opened();
            let channel = next.channel;
            let subscribe = next.subscribe();
            spawn_local(async move {
                if let Some(before) = before {
                    let close = Action::PreviewUnsubscribe {
                        channel: before.channel,
                    };
                    let _closed = act(&control, close).await;
                    // Checked and sent with nothing between: `call` sends at once.
                    if open.try_get_value().flatten().map(|s| s.channel) != Some(channel) {
                        return;
                    }
                }
                if let Some(Err(error)) = act(&control, subscribe).await {
                    shown.try_set(Shown::Ended(error));
                }
            });
        })
    };

    canvas.on_load({
        let screen = Rc::clone(&screen);
        let reopen = Rc::clone(&reopen);
        let tally = tally.clone();
        let control = control.clone();
        move |element| {
            let drawn = Screen::new(
                element,
                move || {
                    if shown.try_get_untracked() == Some(Shown::Starting) {
                        shown.try_set(Shown::Live);
                    }
                },
                tally.clone(),
            );
            if drawn.is_none() {
                shown.set(Shown::Ended("this browser can't draw on a canvas".into()));
                return;
            }
            *screen.borrow_mut() = drawn;
            if let Some(control) = control.clone() {
                reporting.set_value(crate::views::report_every_second(
                    control,
                    crate::views::label("monitor", Some(label)),
                    tally.clone(),
                ));
            }
            reopen(Stream {
                scene: scene.get_untracked(),
                fps: if still { 1 } else { live_fps.get_untracked() },
                ..Stream::first(
                    measured(),
                    Screen::supports_h264(),
                    live_fps.get_untracked(),
                )
            });
        }
    });

    // Closes the open stream, keeping the picture drawn.
    let close = move || {
        let Some(stream) = open.try_update_value(Option::take).flatten() else {
            return;
        };
        if let Some(taps) = shared_taps.and_then(|taps| taps.get()) {
            taps.remove(stream.channel);
        }
        if let Some(open) = session.get() {
            let control = Rc::downgrade(&open.control);
            let channel = stream.channel;
            spawn_local(async move {
                let _closed = act(&control, Action::PreviewUnsubscribe { channel }).await;
            });
        }
    };
    // A still monitor stops once it has its picture.
    if still {
        Effect::new(move |_| {
            if shown.get() == Shown::Live {
                close();
            }
        });
    }
    // A new scene: a stream of it, once the canvas is up.
    {
        let reopen = Rc::clone(&reopen);
        let control = control.clone();
        Effect::new(
            move |before: Option<Option<jstream_control::protocol::SceneId>>| {
                let now = scene.get();
                if let Some(before) = before
                    && before != now
                    && screen.borrow().is_some()
                {
                    // A moving monitor's open stream moves to the scene on its channel
                    // (`preview.show`): the last picture stays until the new one's
                    // first, with no placeholder and no new stream. Should the engine
                    // refuse, or a still monitor have no stream open, one opens.
                    if !still
                        && let (Some(current), Some(control)) =
                            (open.try_get_value().flatten(), control.clone())
                    {
                        let next = current.showing(now.clone());
                        open.try_set_value(Some(next.clone()));
                        let reopen = Rc::clone(&reopen);
                        spawn_local(async move {
                            match act(&control, next.show()).await {
                                Some(Ok(())) | None => {}
                                Some(Err(_)) => {
                                    if open.try_get_value().flatten().as_ref() == Some(&next) {
                                        shown.try_set(Shown::Starting);
                                        reopen(next);
                                    }
                                }
                            }
                        });
                        return now;
                    }
                    shown.set(Shown::Starting);
                    reopen(Stream {
                        scene: now.clone(),
                        fps: if still { 1 } else { live_fps.get_untracked() },
                        ..Stream::first(
                            measured(),
                            Screen::supports_h264(),
                            live_fps.get_untracked(),
                        )
                    });
                }
                now
            },
        );
    }

    // A new base rate, or a box resized (the window, the dock, studio mode turned on):
    // once it holds for `preview::SETTLE`, a stream for it, when it differs enough.
    let restage = {
        let reopen = Rc::clone(&reopen);
        move || {
            let Some(current) = open.try_get_value().flatten() else {
                return;
            };
            let Some(fps) = live_fps.try_get_untracked() else {
                return;
            };
            let fps = if still { 1 } else { fps };
            if let Some(next) = reopened(&current, fps, measured()) {
                reopen(next);
            }
        }
    };
    {
        let restage = restage.clone();
        Effect::new(move |_| {
            live_fps.track();
            restage();
        });
    }
    let resized = {
        let restage = restage.clone();
        window_event_listener(leptos::ev::resize, move |_| {
            let Some(generation) = resizes.try_update_value(|n| {
                *n = n.wrapping_add(1);
                *n
            }) else {
                return;
            };
            let restage = restage.clone();
            set_timeout(
                move || {
                    if resizes.try_get_value() == Some(generation) {
                        restage();
                    }
                },
                preview::SETTLE,
            );
        })
    };

    // Handles only: the cleanup runs where any other may.
    on_cleanup(move || {
        resized.remove();
        if let Some(handle) = reporting.try_update_value(Option::take).flatten() {
            handle.clear();
        }
        close();
    });

    view! {
        <div class="monitor-picture" node_ref=frame>
            <canvas node_ref=canvas></canvas>
            {move || match shown.get() {
                Shown::Live => None,
                Shown::Starting => Some("Starting…".to_owned()),
                Shown::Ended(reason) => Some(format!("No picture: {reason}")),
            }.map(|note| view! { <p class="monitor-note">{note}</p> })}
        </div>
    }
}

/// What draws the frames of `stream`'s channel: each into the screen, and once, when
/// the browser's H.264 decoder fails, the same stream again as JPEG on its channel.
fn frames(
    stream: &Stream,
    screen: &Rc<RefCell<Option<Screen>>>,
    control: &Weak<dyn Control>,
    open: StoredValue<Option<Stream>, LocalStorage>,
    shown: RwSignal<Shown>,
) -> Handler {
    let screen = Rc::clone(screen);
    let control = control.clone();
    let channel = stream.channel;
    Box::new(move |frame| {
        let header = frame.header;
        if header.end {
            let reason = String::from_utf8_lossy(&frame.payload).into_owned();
            shown.try_set(Shown::Ended(reason));
            return;
        }
        let mut drawn = screen.borrow_mut();
        let Some(drawn) = drawn.as_mut() else {
            return;
        };
        let result = drawn.show(&header, &frame.payload);
        if drawn.wants_keyframe() {
            // Behind: the next keyframe now, rather than the stream's next regular one.
            let control = control.clone();
            spawn_local(async move {
                let _asked = act(&control, Action::PreviewKeyframe { channel }).await;
            });
        }
        if let Err(reason) = result
            && !drawn.h264_failed()
        {
            shown.try_set(Shown::Ended(reason));
            return;
        }
        if shown.try_get_untracked() == Some(Shown::Starting) {
            shown.try_set(Shown::Live);
        }
        let Some(now) = open.try_get_value().flatten() else {
            return;
        };
        if now.channel == channel && now.h264 && drawn.h264_failed() {
            // Once, for JPEG: the channel is closed first so it can be reused.
            let jpeg = Stream { h264: false, ..now };
            open.try_set_value(Some(jpeg.clone()));
            let control = control.clone();
            spawn_local(async move {
                let close = Action::PreviewUnsubscribe { channel };
                let _closed = act(&control, close).await;
                if open.try_get_value().flatten().map(|s| s.channel) != Some(channel) {
                    return;
                }
                if let Some(Err(error)) = act(&control, jpeg.subscribe()).await {
                    shown.try_set(Shown::Ended(error));
                }
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::reopened;
    use crate::canvas::preview::Stream;

    #[test]
    fn the_monitor_reopens_only_for_a_new_rate_or_a_much_resized_box() {
        let first = Stream::first((800.0, 450.0, 1.0), true, 30);
        assert_eq!(reopened(&first, 30, (820.0, 460.0, 1.0)), None);

        let faster = reopened(&first, 60, (800.0, 450.0, 1.0)).expect("a new rate");
        assert_eq!((faster.fps, faster.stage), (60, first.stage));

        let bigger = reopened(&first, 30, (1600.0, 900.0, 1.0)).expect("a bigger box");
        assert_eq!((bigger.fps, bigger.stage), (30, (1600.0, 900.0, 1.0)));

        let both = reopened(&first, 60, (1600.0, 900.0, 1.0)).expect("both");
        assert_eq!((both.fps, both.stage), (60, (1600.0, 900.0, 1.0)));
        assert_eq!(both.scene, None, "always the program mix");
    }
}
