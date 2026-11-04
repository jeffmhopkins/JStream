//! A `browser` source's page API (ADR-0102, docs/adr/0102-page-api-for-browser-sources.md):
//! what its settings grant ([`Grant`]) and what its page is sent ([`Feed`]). The engine
//! hands the source a [`PageView`] when what the page may see changes, and the time on
//! each look; the feed turns them into updates holding only what changed and what the
//! grant covers.

use std::collections::BTreeMap;

use jstream_browser::{PageApi, PageData};
use jstream_media::MediaTime;
use jstream_protocol::{Settings, check_value_name};
use serde_json::{Value, json};

use crate::SourceError;

/// The longest `page_api_values` setting, in bytes.
pub(crate) const MAX_VALUES_GRANT_BYTES: usize = 1024;

/// How often a page is sent a clock anchor while the engine looks at it, in seconds.
const ANCHOR_EVERY: f64 = 0.25;

/// A clock that moves back, or on by more than this since the last anchor, has jumped:
/// the page is sent an anchor at once.
const JUMP: f64 = 1.0;

/// How much of each new frame age the lead's mean takes.
const AGE_WEIGHT: f64 = 0.1;

/// What the engine shows a granted page now (ADR-0102 decisions 4 to 7).
#[derive(Clone, Copy, Debug)]
pub struct PageView<'a> {
    /// The media clock now, which the animation clock reads (ADR-0099).
    pub now: MediaTime,
    /// Whether any item showing the source is visible on program, nested scenes
    /// included.
    pub program: bool,
    /// The same on preview, in studio mode.
    pub preview: bool,
    /// The program scene's ID and name.
    pub program_scene: Option<(&'a str, &'a str)>,
    /// The preview scene's ID and name, in studio mode.
    pub preview_scene: Option<(&'a str, &'a str)>,
    /// Every shared value, by name.
    pub values: &'a BTreeMap<String, Value>,
}

/// What a source's settings grant its page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    pub(crate) scenes: bool,
    /// The shared values it may read: whole names, and prefixes (`status.` for
    /// `status.*`) that cover every name below them.
    pub(crate) values: Vec<Covered>,
}

/// One entry of `page_api_values`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Covered {
    Name(String),
    /// Every name below this one: `status` for `status.*`.
    Below(String),
}

impl Grant {
    /// The grant `settings` make: `None` when `page_api` is off or absent (a kind built on
    /// `browser` without it never grants anything).
    ///
    /// # Errors
    /// [`SourceError::Setting`] for a `page_api_values` that isn't names or prefixes.
    pub(crate) fn of(settings: &Settings) -> Result<Option<Self>, SourceError> {
        if !settings
            .get("page_api")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        let scenes = settings
            .get("page_api_scenes")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let values = parse_values(
            settings
                .get("page_api_values")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )?;
        Ok(Some(Self { scenes, values }))
    }

    /// What the page's `window.jstream` has.
    pub(crate) fn api(&self) -> PageApi {
        PageApi {
            scenes: self.scenes,
            values: !self.values.is_empty(),
        }
    }

    /// Whether the page may read the value `name`.
    pub(crate) fn covers(&self, name: &str) -> bool {
        self.values.iter().any(|covered| match covered {
            Covered::Name(own) => own == name,
            Covered::Below(prefix) => name
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('.')),
        })
    }
}

/// `page_api_values`: comma-separated names (`status.text`) or prefixes ending in `.*`
/// (`counter.*`), with spaces around them ignored.
fn parse_values(text: &str) -> Result<Vec<Covered>, SourceError> {
    let invalid = |reason: String| SourceError::Setting {
        param: "page_api_values".into(),
        reason,
    };
    if text.len() > MAX_VALUES_GRANT_BYTES {
        return Err(invalid(format!(
            "must be at most {MAX_VALUES_GRANT_BYTES} bytes"
        )));
    }
    let mut out = Vec::new();
    for entry in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (name, covered) = match entry.strip_suffix(".*") {
            Some(prefix) => (prefix, Covered::Below(prefix.to_owned())),
            None => (entry, Covered::Name(entry.to_owned())),
        };
        check_value_name(name, "page_api_values").map_err(|_| {
            invalid(format!(
                "`{entry}` isn't a value's name (dotted segments of a-z, 0-9 and _) or a \
                 prefix ending in .*"
            ))
        })?;
        if !out.contains(&covered) {
            out.push(covered);
        }
    }
    Ok(out)
}

/// What a granted page was sent last, so each update holds only what changed.
#[derive(Debug)]
pub(crate) struct Feed {
    grant: Grant,
    /// The most frames a second the page paints: one frame is the lead's floor.
    fps: u32,
    source: Option<String>,
    scenes: Option<String>,
    /// The covered values as last sent.
    values: BTreeMap<String, Value>,
    program: bool,
    preview: bool,
    shown_at: Option<f64>,
    preview_shown_at: Option<f64>,
    /// The clock time of the last anchor sent.
    anchored: Option<f64>,
    /// The mean age of the page's frames when the engine looks at them, in seconds.
    age: Option<f64>,
    /// What is still to be sent, merged: updates go at most once per page frame
    /// (ADR-0102 decision 3).
    pending: PageData,
    /// The clock time the last update was sent.
    sent: Option<f64>,
}

impl Feed {
    pub(crate) fn new(grant: Grant, fps: u32) -> Self {
        Self {
            grant,
            fps: fps.max(1),
            source: None,
            scenes: None,
            values: BTreeMap::new(),
            program: false,
            preview: false,
            shown_at: None,
            preview_shown_at: None,
            anchored: None,
            age: None,
            pending: PageData::default(),
            sent: None,
        }
    }

    /// Adds `data` to what is still to be sent: a later clock, visibility or scenes
    /// replaces the earlier, and a later value replaces the same name's.
    pub(crate) fn post(&mut self, data: PageData) {
        let pending = &mut self.pending;
        if data.clock.is_some() {
            pending.clock = data.clock;
        }
        if data.source.is_some() {
            pending.source = data.source;
        }
        if data.scenes.is_some() {
            pending.scenes = data.scenes;
        }
        for (name, value) in data.values {
            match pending.values.iter_mut().find(|(n, _)| *n == name) {
                Some(entry) => entry.1 = value,
                None => pending.values.push((name, value)),
            }
        }
    }

    /// What is still to be sent, if a page frame has passed since the last update at
    /// `now` (or the clock went back), else `None`, keeping it. A clock anchor in it is
    /// moved to `now`.
    pub(crate) fn flush(&mut self, now: MediaTime) -> Option<PageData> {
        let now = now.as_secs_f64();
        if self.pending.is_empty() || !now.is_finite() {
            return None;
        }
        let ready = self
            .sent
            .is_none_or(|sent| now < sent || now - sent >= self.frame());
        if !ready {
            return None;
        }
        self.sent = Some(now);
        let mut data = std::mem::take(&mut self.pending);
        // A held anchor says what the clock reads as it is sent, not when it was made.
        if let Some((_, lead)) = data.clock {
            data.clock = Some((now, lead));
            self.anchored = Some(now);
        }
        Some(data)
    }

    /// When what is still to be sent may go, on the media clock, or `None` with nothing
    /// pending.
    pub(crate) fn due(&self) -> Option<MediaTime> {
        if self.pending.is_empty() {
            return None;
        }
        let at = self.sent.map_or(0.0, |sent| sent + self.frame());
        // Rounded up to a microsecond, so the update is never early.
        let micros = (at * 1e6).ceil();
        Some(MediaTime::new(
            if micros.is_finite() {
                micros as i64
            } else {
                i64::MAX
            },
            jstream_media::Timebase::MICROS,
        ))
    }

    /// One page frame, in seconds.
    fn frame(&self) -> f64 {
        1.0 / f64::from(self.fps)
    }

    /// The update `view` makes: what changed since the last, and a clock anchor when
    /// one is due.
    pub(crate) fn view(&mut self, view: &PageView<'_>) -> PageData {
        let now = view.now.as_secs_f64();
        if view.program && !self.program {
            self.shown_at = Some(now);
        }
        if view.preview && !self.preview {
            self.preview_shown_at = Some(now);
        }
        self.program = view.program;
        self.preview = view.preview;
        let source = json!({
            "shown": self.program,
            "previewShown": self.preview,
            "shownAt": self.shown_at,
            "previewShownAt": self.preview_shown_at,
        })
        .to_string();
        let mut data = PageData {
            clock: self.anchor(now),
            source: changed(&mut self.source, source),
            ..PageData::default()
        };
        if self.grant.scenes {
            let scene = |scene: Option<(&str, &str)>| {
                scene.map_or(Value::Null, |(id, name)| json!({ "id": id, "name": name }))
            };
            let scenes = json!({
                "program": scene(view.program_scene),
                "preview": scene(view.preview_scene),
            })
            .to_string();
            data.scenes = changed(&mut self.scenes, scenes);
        }
        if !self.grant.values.is_empty() {
            let grant = &self.grant;
            let values = &mut self.values;
            values.retain(|name, _| {
                let kept = view.values.contains_key(name) && grant.covers(name);
                if !kept {
                    data.values.push((name.clone(), None));
                }
                kept
            });
            // Compared as values, so only a changed one is written out as text.
            for (name, value) in view.values.iter().filter(|(name, _)| grant.covers(name)) {
                if values.get(name) != Some(value) {
                    values.insert(name.clone(), value.clone());
                    data.values.push((name.clone(), Some(value.to_string())));
                }
            }
        }
        data
    }

    /// A clock anchor at `now` if one is due: the first, every [`ANCHOR_EVERY`] after
    /// that, and at once when the clock jumps. `age` is how old the page's latest frame
    /// is at `now`, if known, which the lead's mean takes in.
    pub(crate) fn clock(&mut self, now: MediaTime, age: Option<f64>) -> Option<(f64, f64)> {
        if let Some(age) = age.filter(|age| age.is_finite() && *age >= 0.0 && *age < JUMP) {
            self.age = Some(match self.age {
                Some(mean) => mean + AGE_WEIGHT * (age - mean),
                None => age,
            });
        }
        self.anchor(now.as_secs_f64())
    }

    fn anchor(&mut self, now: f64) -> Option<(f64, f64)> {
        let due = match self.anchored {
            None => true,
            Some(last) => now < last || now - last >= ANCHOR_EVERY || now - last > JUMP,
        };
        if !due || !now.is_finite() {
            return None;
        }
        self.anchored = Some(now);
        Some((now, self.lead()))
    }

    /// The page's delay from paint to program: one frame interval, plus the mean age of
    /// its frames when the engine samples them (ADR-0102 decision 4).
    fn lead(&self) -> f64 {
        1.0 / f64::from(self.fps) + self.age.unwrap_or(0.0)
    }
}

/// `now` if it differs from what `last` holds, which then holds it.
fn changed(last: &mut Option<String>, now: String) -> Option<String> {
    if last.as_ref() == Some(&now) {
        return None;
    }
    *last = Some(now.clone());
    Some(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_media::Timebase;

    fn at(ms: i64) -> MediaTime {
        MediaTime::new(ms, Timebase::MILLIS)
    }

    fn settings(pairs: &[(&str, Value)]) -> Settings {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn the_grant_follows_the_settings() {
        assert_eq!(Grant::of(&settings(&[])).unwrap(), None);
        assert_eq!(
            Grant::of(&settings(&[
                ("page_api", json!(false)),
                ("page_api_scenes", json!(true))
            ]))
            .unwrap(),
            None
        );
        let grant = Grant::of(&settings(&[
            ("page_api", json!(true)),
            (
                "page_api_values",
                json!(" status.text, counter.* ,,counter.*"),
            ),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(
            grant.values,
            [
                Covered::Name("status.text".into()),
                Covered::Below("counter".into())
            ]
        );
        assert_eq!(
            grant.api(),
            PageApi {
                scenes: false,
                values: true
            }
        );
        assert!(grant.covers("status.text"));
        assert!(grant.covers("counter.a.b"));
        assert!(!grant.covers("counter"));
        assert!(!grant.covers("counterx.a"));
        assert!(!grant.covers("status"));
        for bad in ["Status", "a..b", "*", "a.*.b", ".*"] {
            assert!(
                Grant::of(&settings(&[
                    ("page_api", json!(true)),
                    ("page_api_values", json!(bad))
                ]))
                .is_err(),
                "{bad}"
            );
        }
        let long = "a,".repeat(MAX_VALUES_GRANT_BYTES);
        assert!(
            Grant::of(&settings(&[
                ("page_api", json!(true)),
                ("page_api_values", json!(long))
            ]))
            .is_err()
        );
    }

    #[test]
    fn a_feed_sends_what_changed_and_only_what_the_grant_covers() {
        let grant = Grant {
            scenes: true,
            values: vec![Covered::Below("status".into())],
        };
        let mut feed = Feed::new(grant, 50);
        let mut values = BTreeMap::new();
        values.insert("status.text".to_owned(), json!("On air"));
        values.insert("secret".to_owned(), json!(1));
        fn view(values: &BTreeMap<String, Value>, program: bool, ms: i64) -> PageView<'_> {
            PageView {
                now: at(ms),
                program,
                preview: false,
                program_scene: Some(("scn_1", "Main")),
                preview_scene: None,
                values,
            }
        }
        let first = feed.view(&view(&values, false, 1000));
        assert_eq!(first.clock, Some((1.0, 0.02)));
        assert_eq!(
            first.source.as_deref(),
            Some(r#"{"previewShown":false,"previewShownAt":null,"shown":false,"shownAt":null}"#)
        );
        assert_eq!(
            first.scenes.as_deref(),
            Some(r#"{"preview":null,"program":{"id":"scn_1","name":"Main"}}"#)
        );
        assert_eq!(
            first.values,
            [("status.text".to_owned(), Some(r#""On air""#.to_owned()))]
        );
        // Nothing changed, and no anchor is due yet.
        assert!(feed.view(&view(&values, false, 1100)).is_empty());
        // Shown: its time is the clock's then.
        let shown = feed.view(&view(&values, true, 2000));
        assert_eq!(
            shown.source.as_deref(),
            Some(r#"{"previewShown":false,"previewShownAt":null,"shown":true,"shownAt":2.0}"#)
        );
        assert_eq!(shown.scenes, None);
        values.remove("status.text");
        let removed = feed.view(&view(&values, true, 2100));
        assert_eq!(removed.values, [("status.text".to_owned(), None)]);
    }

    #[test]
    fn a_feed_sends_at_most_one_update_per_page_frame() {
        let grant = Grant {
            scenes: false,
            values: vec![Covered::Below("counter".into())],
        };
        // 50 a second: one page frame is 20 ms.
        let mut feed = Feed::new(grant, 50);
        let value = |n: i64| PageData {
            values: vec![("counter.a".to_owned(), Some(n.to_string()))],
            ..PageData::default()
        };
        assert_eq!(feed.due(), None, "nothing pending");
        feed.post(value(1));
        assert_eq!(
            feed.flush(at(1000)),
            Some(value(1)),
            "the first goes at once"
        );
        // Three more within the frame merge into one, the latest winning.
        feed.post(value(2));
        feed.post(PageData {
            source: Some("{}".to_owned()),
            ..PageData::default()
        });
        feed.post(value(3));
        assert_eq!(feed.flush(at(1010)), None);
        assert_eq!(
            feed.due(),
            Some(MediaTime::new(1_020_000, Timebase::MICROS))
        );
        let merged = feed.flush(at(1020)).expect("a frame has passed");
        assert_eq!(merged.source.as_deref(), Some("{}"));
        assert_eq!(merged.values, value(3).values);
        assert_eq!(feed.due(), None);
        // A held anchor goes out as the clock reads when it is sent.
        feed.post(PageData {
            clock: Some((1.03, 0.02)),
            ..PageData::default()
        });
        assert_eq!(feed.flush(at(1030)), None);
        assert_eq!(
            feed.flush(at(1045)).and_then(|data| data.clock),
            Some((1.045, 0.02))
        );
        // A clock that went back sends at once.
        feed.post(value(4));
        assert_eq!(feed.flush(at(500)), Some(value(4)));
    }

    #[test]
    fn a_page_without_scenes_or_values_gets_neither() {
        let mut feed = Feed::new(
            Grant {
                scenes: false,
                values: Vec::new(),
            },
            30,
        );
        let mut values = BTreeMap::new();
        values.insert("status.text".to_owned(), json!("x"));
        let data = feed.view(&PageView {
            now: at(0),
            program: true,
            preview: false,
            program_scene: Some(("scn_1", "Main")),
            preview_scene: None,
            values: &values,
        });
        assert_eq!(data.scenes, None);
        assert!(data.values.is_empty());
        assert!(data.source.is_some());
    }

    #[test]
    fn anchors_come_every_quarter_second_and_when_the_clock_jumps() {
        let mut feed = Feed::new(
            Grant {
                scenes: false,
                values: Vec::new(),
            },
            25,
        );
        assert_eq!(feed.clock(at(1000), None), Some((1.0, 0.04)));
        assert_eq!(feed.clock(at(1200), None), None);
        assert_eq!(feed.clock(at(1250), Some(0.01)).map(|c| c.0), Some(1.25));
        // Backwards: a jump.
        assert_eq!(feed.clock(at(900), None).map(|c| c.0), Some(0.9));
        // The lead takes in the frames' mean age.
        let (_, lead) = feed.clock(at(5000), Some(0.03)).unwrap();
        assert!((lead - (0.04 + 0.012)).abs() < 1e-9, "{lead}");
    }
}
