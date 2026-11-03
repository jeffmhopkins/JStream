//! Screenshots, as OBS takes them (ADR-0096): the item menu's Screenshot (source) and
//! the scene menus' Screenshot (scene) save one lossless picture as a PNG file.
//!
//! A screenshot opens a preview stream asking for `png` only, at the canvas's size
//! within the largest a preview may be, on a channel from the canvas's taps
//! ([`Taps`](crate::canvas::taps::Taps)). Its first picture is saved through a
//! download link, and the stream is closed. Captures show in it, as in any preview. If
//! the engine refuses the stream (too many streams, a scene or source gone), a toast
//! gives its reason. Native builds (tests, clippy) get a stand-in that only says so.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::menu::Entry;
use crate::store::Studio;
use jstream_control::protocol::{
    Action, BinaryFrame, Canvas, PreviewCodec, PreviewSettings, SceneId, SourceId,
};
use leptos::prelude::WithUntracked;

/// What a screenshot shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shot {
    /// A scene, as the canvas shows it.
    Scene(SceneId),
    /// A source alone, as a projector of it shows it.
    Source(SourceId),
}

/// The `preview.subscribe` for one PNG picture of `shot` on `channel`, at `canvas`'s
/// size, scaled down into the largest picture a preview may be.
pub fn subscribe(channel: u32, shot: &Shot, canvas: Canvas) -> Action {
    let (scene_id, source_id) = match shot {
        Shot::Scene(scene) => (Some(scene.clone()), None),
        Shot::Source(source) => (None, Some(source.clone())),
    };
    Action::PreviewSubscribe {
        channel,
        max_width: Some(canvas.width.clamp(
            *PreviewSettings::WIDTH.start(),
            *PreviewSettings::WIDTH.end(),
        )),
        max_height: Some(canvas.height.clamp(
            *PreviewSettings::HEIGHT.start(),
            *PreviewSettings::HEIGHT.end(),
        )),
        fps: Some(1),
        codecs: vec![PreviewCodec::Png],
        scene_id,
        source: None,
        source_id,
    }
}

/// What a frame of a screenshot's stream means: the PNG to save, or why there is none.
pub fn outcome(frame: &BinaryFrame) -> Result<Vec<u8>, String> {
    let header = frame.header;
    if header.end {
        let reason = String::from_utf8_lossy(&frame.payload).into_owned();
        return Err(if reason.is_empty() {
            "the engine ended the picture".to_owned()
        } else {
            reason
        });
    }
    if header.codec != Some(PreviewCodec::Png) {
        return Err("the engine sent no PNG".to_owned());
    }
    Ok(frame.payload.to_vec())
}

/// The name a screenshot of `name` taken at `stamp` (year, month, day, hour, minute,
/// second) is saved under: `<name> YYYY-MM-DD HH-MM-SS.png`, as OBS names its own, with
/// each character a file name can't hold replaced by `_`.
pub fn file_name(name: &str, stamp: [u32; 6]) -> String {
    let safe: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "Screenshot".to_owned()
    } else {
        safe
    };
    let [year, month, day, hour, minute, second] = stamp;
    format!("{safe} {year:04}-{month:02}-{day:02} {hour:02}-{minute:02}-{second:02}.png")
}

/// The menu entry `label` (OBS's “Screenshot (scene)” or “Screenshot (source)”) that
/// takes a screenshot of `shot`; disabled without a session or a shot.
pub fn entry(
    label: &str,
    studio: Studio,
    session: Option<SessionContext>,
    shot: Option<Shot>,
) -> Entry {
    let off = session.is_none() || shot.is_none();
    Entry::item(label, move || {
        if let (Some(session), Some(shot)) = (session, shot.clone()) {
            take(studio, session, shot);
        }
    })
    .disabled_if(off)
}

/// The name of what `shot` shows, for the file: the scene's or the source's.
fn name_of(studio: Studio, shot: &Shot) -> String {
    studio
        .collection
        .with_untracked(|c| {
            let c = c.as_ref()?;
            match shot {
                Shot::Scene(id) => c.scene(id).map(|s| s.name.clone()),
                Shot::Source(id) => c.source(id).map(|s| s.name.clone()),
            }
        })
        .unwrap_or_default()
}

/// The toast when the engine refuses the stream, with the engine's reason: too many
/// streams, a scene or source deleted meanwhile. (An engine without the `png` codec
/// can't get here: the handshake refuses a client newer than the engine.)
pub fn refused(error: &str) -> String {
    format!("Couldn't take the screenshot: {error}.")
}

#[cfg(target_arch = "wasm32")]
pub use web::take;

#[cfg(not(target_arch = "wasm32"))]
pub use native::take;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{Shot, name_of};
    use crate::app::SessionContext;
    use crate::store::Studio;

    /// Native builds can't save a file: the toast says so.
    pub fn take(studio: Studio, _session: SessionContext, shot: Shot) {
        let name = name_of(studio, &shot);
        studio.notify(format!("A screenshot of {name} is saved from the browser."));
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::{Shot, file_name, name_of, outcome, refused, subscribe};
    use crate::app::SessionContext;
    use crate::canvas::taps::{Handler, PreviewTaps};
    use crate::store::Studio;
    use jstream_control::protocol::{Action, Call};
    use leptos::prelude::{WithUntracked, use_context};
    use leptos::task::spawn_local;
    use leptos::web_sys::js_sys::{Array, Date, Function, Object, Reflect, Uint8Array};
    use leptos::web_sys::wasm_bindgen::{JsCast, JsValue};
    use std::cell::Cell;
    use std::rc::Rc;

    fn global(name: &str) -> Result<JsValue, String> {
        Reflect::get(&leptos::web_sys::js_sys::global(), &JsValue::from_str(name))
            .ok()
            .filter(|value| !value.is_undefined() && !value.is_null())
            .ok_or_else(|| format!("this browser has no {name}"))
    }

    fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, String> {
        let function: Function = Reflect::get(target, &JsValue::from_str(method))
            .map_err(|_| format!("no {method}"))?
            .dyn_into()
            .map_err(|_| format!("{method} is not a function"))?;
        function
            .apply(target, &args.iter().collect::<Array>())
            .map_err(|_| format!("{method} failed"))
    }

    /// Saves `png` as `name` through a download link.
    fn save(png: &[u8], name: &str) -> Result<(), String> {
        let options = JsValue::from(Object::new());
        Reflect::set(&options, &"type".into(), &"image/png".into())
            .map_err(|_| "no Blob options")?;
        let blob_class: Function = global("Blob")?
            .dyn_into()
            .map_err(|_| "Blob is not a constructor")?;
        let parts = Array::of1(&Uint8Array::from(png));
        let blob = Reflect::construct(&blob_class, &Array::of2(&parts, &options))
            .map_err(|_| "the browser refused the picture")?;
        let url = call(&global("URL")?, "createObjectURL", &[blob])?;
        let document = leptos::prelude::document();
        let link = document
            .create_element("a")
            .map_err(|_| "no link to save with")?;
        let href = url.as_string().unwrap_or_default();
        let _href = link.set_attribute("href", &href);
        let _download = link.set_attribute("download", name);
        let link: leptos::web_sys::HtmlElement =
            link.dyn_into().map_err(|_| "no link to save with")?;
        link.click();
        // Some browsers cancel a download whose URL is revoked in the same task, so the
        // picture is let go a few seconds later instead.
        leptos::prelude::set_timeout(
            move || {
                if let Ok(url_api) = global("URL") {
                    let _revoked = call(&url_api, "revokeObjectURL", &[url]);
                }
            },
            std::time::Duration::from_secs(5),
        );
        Ok(())
    }

    /// The local time now, for the file's name.
    fn stamp() -> [u32; 6] {
        let now = Date::new_0();
        [
            now.get_full_year(),
            now.get_month() + 1,
            now.get_date(),
            now.get_hours(),
            now.get_minutes(),
            now.get_seconds(),
        ]
    }

    /// Takes a screenshot of `shot`, saved as a file named after it.
    pub fn take(studio: Studio, session: SessionContext, shot: Shot) {
        let name = name_of(studio, &shot);
        let (Some(open), Some(taps)) = (
            session.get(),
            use_context::<PreviewTaps>().and_then(|taps| taps.get()),
        ) else {
            studio.notify("Screenshots need the canvas, which isn't open here.");
            return;
        };
        let Some(canvas) = studio
            .collection
            .with_untracked(|c| c.as_ref().map(|c| c.canvas))
        else {
            return;
        };
        let control = Rc::downgrade(&open.control);
        let channel = taps.take();
        let done = Rc::new(Cell::new(false));
        let handler: Handler = {
            let (taps, control, done) = (taps.clone(), control.clone(), Rc::clone(&done));
            Box::new(move |frame| {
                if done.replace(true) {
                    return;
                }
                let saved = outcome(frame).and_then(|png| save(&png, &file_name(&name, stamp())));
                match saved {
                    Ok(()) => studio.notify(format!("Saved a screenshot of {name}.")),
                    Err(reason) => {
                        studio.notify(format!("Couldn't take the screenshot: {reason}."))
                    }
                }
                taps.remove(channel);
                let control = control.clone();
                spawn_local(async move {
                    if let Some(control) = control.upgrade() {
                        let close = Action::PreviewUnsubscribe { channel };
                        let _closed = control.call(Call::Action { action: close }).await;
                    }
                });
            })
        };
        taps.insert(channel, handler);
        let action = subscribe(channel, &shot, canvas);
        spawn_local(async move {
            let Some(control) = control.upgrade() else {
                return;
            };
            if let Err(error) = control.call(Call::Action { action }).await
                && !done.replace(true)
            {
                taps.remove(channel);
                studio.notify(refused(&error.to_string()));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{BinaryHeader, Rev, StreamKind};
    use std::sync::Arc;

    fn frame(codec: Option<PreviewCodec>, end: bool, payload: &[u8]) -> BinaryFrame {
        BinaryFrame {
            header: BinaryHeader {
                kind: StreamKind::Preview,
                channel: 9,
                codec,
                keyframe: true,
                end,
                width: 1920,
                height: 1080,
                pts_us: 0,
                rev: Rev(1),
            },
            payload: Arc::from(payload),
        }
    }

    #[test]
    fn a_screenshot_asks_for_one_png_at_the_canvas_size() {
        let canvas = Canvas {
            width: 1280,
            height: 720,
        };
        let Action::PreviewSubscribe {
            channel,
            max_width,
            max_height,
            fps,
            codecs,
            scene_id,
            source_id,
            ..
        } = subscribe(9, &Shot::Scene(SceneId::new("s")), canvas)
        else {
            panic!("not a subscribe");
        };
        assert_eq!((channel, max_width, max_height), (9, Some(1280), Some(720)));
        assert_eq!((fps, codecs), (Some(1), vec![PreviewCodec::Png]));
        assert_eq!((scene_id, source_id), (Some(SceneId::new("s")), None));
        // A 4K canvas is scaled into the largest preview; a source is shown alone.
        let big = Canvas {
            width: 3840,
            height: 2160,
        };
        let Action::PreviewSubscribe {
            max_width,
            max_height,
            scene_id,
            source_id,
            ..
        } = subscribe(9, &Shot::Source(SourceId::new("cam")), big)
        else {
            panic!("not a subscribe");
        };
        assert_eq!((max_width, max_height), (Some(1920), Some(1080)));
        assert_eq!((scene_id, source_id), (None, Some(SourceId::new("cam"))));
    }

    #[test]
    fn only_a_png_picture_is_saved() {
        let png = [0x89, b'P', b'N', b'G'];
        assert_eq!(
            outcome(&frame(Some(PreviewCodec::Png), false, &png)),
            Ok(png.to_vec())
        );
        assert!(outcome(&frame(Some(PreviewCodec::Jpeg), false, &png)).is_err());
        assert_eq!(
            outcome(&frame(None, true, b"scene deleted")),
            Err("scene deleted".to_owned())
        );
    }

    #[test]
    fn the_file_is_named_after_what_it_shows_and_when() {
        assert_eq!(
            file_name("Main", [2026, 11, 3, 4, 5, 6]),
            "Main 2026-11-03 04-05-06.png"
        );
        assert_eq!(
            file_name(" a/b:c ", [2026, 1, 2, 13, 14, 15]),
            "a_b_c 2026-01-02 13-14-15.png"
        );
        assert_eq!(
            file_name("", [2026, 1, 2, 3, 4, 5]),
            "Screenshot 2026-01-02 03-04-05.png"
        );
    }
}
