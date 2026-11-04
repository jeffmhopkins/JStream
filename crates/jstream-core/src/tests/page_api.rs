//! The page API (ADR-0102): a `browser` source with `page_api` on is told whether it is
//! shown on program and on preview, nested scenes included, which scenes those are, and
//! the shared values its grant covers, each only when it changes.

use super::audio::{engine, eventually};
use super::{add, batch, block_on, connected, create, edits, switch_call};
use jstream_browser::PageData;
use jstream_browser::fake::{FakeBrowser, FakePageState};
use jstream_control::Control;
use jstream_media::{ChannelLayout, Timebase};
use jstream_platform::fake::{FakeAudioIo, FakeClock};
use jstream_protocol::{Action, Call, Edit, SceneId, Settings, SourceId, SourceKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A `browser` source granted the page API, the scenes, and the values below `status`.
fn granted_page(id: &str) -> Edit {
    let settings: BTreeMap<String, Value> = [
        ("url", json!("https://example.com/")),
        ("page_api", json!(true)),
        ("page_api_scenes", json!(true)),
        ("page_api_values", json!("status.*")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin {
            kind: "browser".into(),
        },
        settings: Settings(settings),
        duplicated_from: None,
    }
}

fn scene_source(id: &str, scene: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Scene {
            scene_id: SceneId::new(scene),
        },
        settings: Settings::default(),
        duplicated_from: None,
    }
}

/// What a page has been sent so far, merged: the latest visibility and scenes, and each
/// value as it stands (`None` once removed).
#[derive(Debug, Default)]
struct Seen {
    source: Option<Value>,
    scenes: Option<Value>,
    values: BTreeMap<String, Option<Value>>,
    clock: bool,
}

impl Seen {
    fn take(&mut self, page: &FakePageState) {
        for PageData {
            clock,
            source,
            scenes,
            values,
        } in page.take_data()
        {
            self.clock |= clock.is_some();
            let parse = |text: String| serde_json::from_str::<Value>(&text).expect("JSON");
            if let Some(source) = source {
                self.source = Some(parse(source));
            }
            if let Some(scenes) = scenes {
                self.scenes = Some(parse(scenes));
            }
            for (name, value) in values {
                self.values.insert(name, value.map(parse));
            }
        }
    }

    /// Takes what the page is sent until `done` holds, moving the clock on a millisecond
    /// at a time: an update held back to keep to one per page frame goes out once a
    /// frame has passed.
    fn until(
        &mut self,
        clock: &FakeClock,
        page: &FakePageState,
        mut done: impl FnMut(&Self) -> bool,
    ) -> bool {
        eventually(|| {
            clock.advance(1_000);
            self.take(page);
            done(self)
        })
    }

    fn shown(&self) -> (bool, bool) {
        let source = self.source.as_ref().expect("visibility was sent");
        (
            source["shown"].as_bool().expect("shown"),
            source["previewShown"].as_bool().expect("previewShown"),
        )
    }
}

fn set_value(control: &dyn Control, name: &str, value: Option<Value>) {
    let action = Action::ValuesSet {
        name: name.into(),
        value,
    };
    block_on(control.call(Call::Action { action })).expect("values.set");
}

fn set_preview(control: &dyn Control, scene: &str) {
    let action = Action::StudioSetPreview {
        scene_id: SceneId::new(scene),
    };
    block_on(control.call(Call::Action { action })).expect("studio.set_preview");
}

#[test]
fn a_granted_page_follows_its_visibility_the_scenes_and_its_values() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let browser = FakeBrowser::new();
    let engine = engine(&audio, &clock).with_browser(browser.clone());
    let control = connected(&engine);
    // `a` places the page directly, `b` through the nested scene `inner`, `c` not at
    // all. `a`, the first scene, goes on program and on preview.
    batch(
        &control,
        edits(vec![
            create("a"),
            create("b"),
            create("c"),
            create("inner"),
            granted_page("page"),
            scene_source("nested", "inner"),
            add("a", "page-in-a", "page"),
            add("inner", "page-in-inner", "page"),
            add("b", "inner-in-b", "nested"),
        ]),
    )
    .expect("collection");
    assert!(eventually(|| browser.pages().len() == 1));
    let page: Arc<FakePageState> = browser.pages().remove(0);
    assert!(
        page.spec()
            .page_api
            .as_ref()
            .is_some_and(|api| api.scenes && api.values)
    );

    let mut seen = Seen::default();
    let scene = |id: &str| json!({ "id": id, "name": id.to_uppercase() });
    assert!(seen.until(&clock, &page, |seen| seen.source.is_some()
        && seen.scenes.is_some()));
    assert_eq!(seen.shown(), (true, true));
    assert_eq!(
        seen.scenes,
        Some(json!({ "program": scene("a"), "preview": scene("a") }))
    );
    assert!(seen.clock, "the first update anchors the clock");

    // Only the values the grant covers reach the page, and a removal does too.
    set_value(&control, "status.text", Some(json!("On air")));
    set_value(&control, "counter.a", Some(json!(2)));
    let on_air = BTreeMap::from([("status.text".to_owned(), Some(json!("On air")))]);
    assert!(seen.until(&clock, &page, |seen| seen.values == on_air));
    set_value(&control, "status.text", None);
    let removed = BTreeMap::from([("status.text".to_owned(), None)]);
    assert!(seen.until(&clock, &page, |seen| seen.values == removed));

    // Preview on `c` hides it there; program on `b` still shows it, through `inner`.
    set_preview(&control, "c");
    assert!(seen.until(&clock, &page, |seen| seen.shown() == (true, false)));
    block_on(control.call(switch_call("b"))).expect("scene.switch");
    let b_and_c = Some(json!({ "program": scene("b"), "preview": scene("c") }));
    assert!(seen.until(&clock, &page, |seen| seen.scenes == b_and_c));
    assert_eq!(seen.shown(), (true, false));
    block_on(control.call(switch_call("c"))).expect("scene.switch");
    assert!(seen.until(&clock, &page, |seen| seen.shown() == (false, false)));
    let source = seen.source.as_ref().expect("visibility");
    assert!(source["shownAt"].is_number(), "{source}");

    // Nothing changed, so nothing more is sent but a clock anchor.
    set_value(&control, "counter.a", Some(json!(3)));
    assert!(
        page.take_data()
            .iter()
            .all(|data| data.source.is_none() && data.scenes.is_none() && data.values.is_empty())
    );
}

/// A page without the grant is never sent anything.
#[test]
fn a_page_without_the_grant_is_sent_nothing() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let browser = FakeBrowser::new();
    let engine = engine(&audio, &clock).with_browser(browser.clone());
    let control = connected(&engine);
    batch(
        &control,
        edits(vec![
            create("a"),
            Edit::SourceCreate {
                source_id: SourceId::new("page"),
                name: "page".into(),
                kind: SourceKind::Builtin {
                    kind: "browser".into(),
                },
                settings: Settings(
                    [("url".to_owned(), json!("https://example.com/"))]
                        .into_iter()
                        .collect(),
                ),
                duplicated_from: None,
            },
            add("a", "page-in-a", "page"),
        ]),
    )
    .expect("collection");
    assert!(eventually(|| browser.pages().len() == 1));
    set_value(&control, "status.text", Some(json!("On air")));
    let page = browser.pages().remove(0);
    assert!(page.spec().page_api.is_none());
    assert!(page.take_data().is_empty());
}
