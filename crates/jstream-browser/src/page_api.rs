//! The page API's browser side (ADR-0102, docs/adr/0102-page-api-for-browser-sources.md):
//! what a page is granted ([`PageApi`]), the one update the engine sends it
//! ([`PageData`]), which of its documents get `window.jstream` ([`ApiHome`]), and the
//! JavaScript that defines it ([`shim`]). Data only flows one way: nothing here, nor in
//! the shim, carries anything from a page back to the engine.

#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
use crate::PageLocation;
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
use crate::page::file_url;
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
use crate::request::same_web_origin;

/// The most bytes one [`PageData`] carries in its fields: under the pipe's message limit,
/// with room for the framing. A bigger update is split into several
/// ([`PageData::split`]); one shared value (at most 16 KiB) always fits.
pub const MAX_PAGE_DATA_BYTES: usize = 192 * 1024;

/// What a page's `window.jstream` has, beyond the clock and the page's own visibility,
/// which every granted page gets (ADR-0102 decision 1). Fixed while the page is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageApi {
    /// `jstream.scenes`: the program and preview scenes.
    pub scenes: bool,
    /// `jstream.values`: the shared values the engine sends it.
    pub values: bool,
    /// `jstream.levels()`: one bus's levels and bands, sent each page frame.
    pub levels: bool,
    /// `jstream.midi()`: one MIDI input's tempo, held notes, controllers and latest
    /// notes (ADR-0147).
    pub midi: bool,
}

/// One update to a page's `window.jstream`: each field the engine sends is the latest of
/// its kind, and a field left out is unchanged. The engine filters it to the page's
/// grant before sending.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageData {
    /// A clock anchor: the engine animation clock's time in seconds, and the lead the
    /// page adds for its delay from paint to program, in seconds.
    pub clock: Option<(f64, f64)>,
    /// The page's own visibility, as a JSON object (`shown`, `previewShown`, `shownAt`,
    /// `previewShownAt`).
    pub source: Option<String>,
    /// The program and preview scenes, as a JSON object (`program`, `preview`).
    pub scenes: Option<String>,
    /// The granted bus's latest reading, as a JSON object (`peak`, `rms`, `bands`), or
    /// `null` while there is none.
    pub levels: Option<String>,
    /// The granted MIDI input's latest reading, as a JSON object (`input`, `tempo`,
    /// `notes`, `controllers`, `recent`).
    pub midi: Option<String>,
    /// Shared values by name: each value's JSON text, or `None` for one removed.
    pub values: Vec<(String, Option<String>)>,
}

impl PageData {
    /// Whether the update changes nothing.
    pub fn is_empty(&self) -> bool {
        self.clock.is_none()
            && self.source.is_none()
            && self.scenes.is_none()
            && self.levels.is_none()
            && self.midi.is_none()
            && self.values.is_empty()
    }

    /// The bytes its fields take, as [`MAX_PAGE_DATA_BYTES`] counts them.
    pub fn bytes(&self) -> usize {
        16 + self.source.as_ref().map_or(0, String::len)
            + self.scenes.as_ref().map_or(0, String::len)
            + self.levels.as_ref().map_or(0, String::len)
            + self.midi.as_ref().map_or(0, String::len)
            + self
                .values
                .iter()
                .map(|(name, value)| 9 + name.len() + value.as_ref().map_or(0, String::len))
                .sum::<usize>()
    }

    /// The update as one or more, each within [`MAX_PAGE_DATA_BYTES`]: the clock,
    /// visibility, scenes, levels and MIDI in the first, and the values in order, each whole in one of
    /// them. Sent in order, they leave the page as the one update would.
    pub fn split(self) -> Vec<PageData> {
        if self.bytes() <= MAX_PAGE_DATA_BYTES {
            return vec![self];
        }
        let mut out = vec![PageData {
            values: Vec::new(),
            ..self.clone()
        }];
        for value in self.values {
            let last = out.last_mut().map(|last| {
                last.bytes() + 9 + value.0.len() + value.1.as_ref().map_or(0, String::len)
            });
            match (out.last_mut(), last) {
                (Some(current), Some(bytes)) if bytes <= MAX_PAGE_DATA_BYTES => {
                    current.values.push(value)
                }
                _ => out.push(PageData {
                    values: vec![value],
                    ..PageData::default()
                }),
            }
        }
        out
    }
}

/// Which documents of a page get `window.jstream` (ADR-0102 decision 2): only its main
/// frame, and only while that shows the source's own document.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
pub(crate) enum ApiHome {
    /// A document of the same origin as this URL (the source's own).
    Web(String),
    /// A `file:` document at or below this folder URL, which ends in `/`.
    File(String),
}

#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
impl ApiHome {
    /// Where a page at `location` gets the API: never for a blank page, nor for a file
    /// whose folder can't be resolved. A page given as HTML gets it on `html_url`'s
    /// origin, the one its policy gave it
    /// ([`RequestPolicy::html_url`](crate::RequestPolicy::html_url)).
    pub(crate) fn of(location: &PageLocation, html_url: Option<&str>) -> Option<Self> {
        match location {
            PageLocation::Blank => None,
            PageLocation::Web(url) => Some(Self::Web(url.clone())),
            PageLocation::File(file) => {
                let folder = file.parent()?.canonicalize().ok()?;
                let mut url = file_url(&folder);
                if !url.ends_with('/') {
                    url.push('/');
                }
                Some(Self::File(url))
            }
            // Its own origin, which the helper serves it from (#1035).
            PageLocation::Html(_) => Some(Self::Web(html_url?.to_owned())),
        }
    }

    /// Whether a main frame showing `url` gets the API.
    pub(crate) fn covers(&self, url: &str) -> bool {
        let scheme = url
            .split_once(':')
            .map(|(scheme, _)| scheme.to_ascii_lowercase());
        match self {
            Self::Web(own) => same_web_origin(own, url),
            // Chromium resolves `..` before a URL reaches the frame, and the request
            // policy refuses a file outside the folder in any case. Windows paths
            // compare without case, as the file system does.
            Self::File(folder) => {
                scheme.as_deref() == Some("file")
                    && url.get(..folder.len()).is_some_and(|head| {
                        if cfg!(windows) {
                            head.eq_ignore_ascii_case(folder)
                        } else {
                            head == folder
                        }
                    })
            }
        }
    }

    /// The rule as two strings, to cross to the renderer process: a kind and a value.
    pub(crate) fn to_parts(&self) -> (&'static str, &str) {
        match self {
            Self::Web(url) => ("web", url),
            Self::File(folder) => ("file", folder),
        }
    }

    /// The rule [`to_parts`](Self::to_parts) wrote, or `None` for anything else.
    pub(crate) fn from_parts(kind: &str, value: &str) -> Option<Self> {
        match kind {
            "web" => Some(Self::Web(value.to_owned())),
            "file" if value.ends_with('/') => Some(Self::File(value.to_owned())),
            _ => None,
        }
    }
}

/// The JavaScript that defines `window.jstream` for a page with `api`. Evaluated, it
/// returns the function that takes each update as JSON text ([`update_json`]).
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
pub(crate) fn shim(api: PageApi) -> String {
    let grant = format!(
        "{{scenes:{},values:{},levels:{},midi:{}}}",
        api.scenes, api.values, api.levels, api.midi
    );
    include_str!("page_api.js").replace("__GRANT__", &grant)
}

/// A page's latest update of each kind, merged from those it was sent: so a document
/// that gets the API later (one the browser hadn't made yet, a reload, a navigation
/// back) starts from where the page is.
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
#[derive(Debug, Default)]
pub(crate) struct PageCache {
    /// The clock anchor, and when it arrived.
    clock: Option<(f64, f64, std::time::Instant)>,
    source: Option<String>,
    scenes: Option<String>,
    levels: Option<String>,
    midi: Option<String>,
    values: std::collections::BTreeMap<String, String>,
}

#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
impl PageCache {
    /// Takes in `data`.
    pub(crate) fn merge(&mut self, data: &PageData) {
        if let Some((t, lead)) = data.clock {
            self.clock = Some((t, lead, std::time::Instant::now()));
        }
        if let Some(source) = &data.source {
            self.source = Some(source.clone());
        }
        if let Some(scenes) = &data.scenes {
            self.scenes = Some(scenes.clone());
        }
        if let Some(levels) = &data.levels {
            self.levels = Some(levels.clone());
        }
        if let Some(midi) = &data.midi {
            self.midi = Some(midi.clone());
        }
        for (name, value) in &data.values {
            match value {
                Some(value) => self.values.insert(name.clone(), value.clone()),
                None => self.values.remove(name),
            };
        }
    }

    /// Everything it holds as one update, the clock anchor moved on by the time since it
    /// arrived.
    pub(crate) fn full(&self) -> PageData {
        PageData {
            clock: self
                .clock
                .map(|(t, lead, at)| (t + at.elapsed().as_secs_f64(), lead)),
            source: self.source.clone(),
            scenes: self.scenes.clone(),
            levels: self.levels.clone(),
            midi: self.midi.clone(),
            values: self
                .values
                .iter()
                .map(|(name, value)| (name.clone(), Some(value.clone())))
                .collect(),
        }
    }
}

/// `data` as the JSON text the shim's update function takes. A non-finite clock is left
/// out.
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
pub(crate) fn update_json(data: &PageData) -> String {
    let mut fields = Vec::new();
    if let Some((t, lead)) = data.clock
        && t.is_finite()
        && lead.is_finite()
    {
        fields.push(format!("\"clock\":{{\"t\":{t},\"lead\":{lead}}}"));
    }
    if let Some(source) = &data.source {
        fields.push(format!("\"source\":{source}"));
    }
    if let Some(scenes) = &data.scenes {
        fields.push(format!("\"scenes\":{scenes}"));
    }
    if let Some(levels) = &data.levels {
        fields.push(format!("\"levels\":{levels}"));
    }
    if let Some(midi) = &data.midi {
        fields.push(format!("\"midi\":{midi}"));
    }
    if !data.values.is_empty() {
        let values: Vec<String> = data
            .values
            .iter()
            .filter(|(name, _)| is_value_name(name))
            .map(|(name, value)| format!("\"{name}\":{}", value.as_deref().unwrap_or("null")))
            .collect();
        fields.push(format!("\"values\":{{{}}}", values.join(",")));
    }
    format!("{{{}}}", fields.join(","))
}

/// Whether `name` is safe to write into JSON unescaped: a shared value's name is dotted
/// segments of `a`–`z`, `0`–`9` and `_`, which the engine checked.
#[cfg(any(test, all(any(windows, target_os = "linux"), feature = "helper")))]
fn is_value_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_web_page_gets_the_api_on_its_own_origin_only() {
        let home = ApiHome::of(
            &PageLocation::Web("https://example.com/overlay/".into()),
            None,
        )
        .unwrap();
        assert!(home.covers("https://example.com/other/page.html?x#y"));
        assert!(home.covers("https://EXAMPLE.com:443/"));
        assert!(!home.covers("http://example.com/"));
        assert!(!home.covers("https://example.com:8443/"));
        assert!(!home.covers("https://evil.example/"));
        assert!(!home.covers("data:text/html,hi"));
        assert!(!home.covers("about:blank"));
    }

    #[test]
    fn a_file_page_gets_it_in_its_folder_and_an_html_page_on_its_own_origin() {
        let dir = std::env::temp_dir().join(format!("jstream-page-api-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let home = ApiHome::of(&PageLocation::File(dir.join("index.html")), None).unwrap();
        let ApiHome::File(folder) = &home else {
            panic!("a file rule");
        };
        assert!(folder.starts_with("file:///") && folder.ends_with('/'));
        assert!(home.covers(&format!("{folder}index.html")));
        assert!(home.covers(&format!("{folder}sub/b.html#top")));
        assert!(!home.covers("file:///etc/passwd"));
        assert!(!home.covers("https://example.com/"));
        let _ = std::fs::remove_dir_all(&dir);

        let url = "https://p7.html.jstream.invalid/";
        let html = ApiHome::of(&PageLocation::Html("<p>hi</p>".into()), Some(url)).unwrap();
        assert!(html.covers(url));
        assert!(!html.covers("https://p8.html.jstream.invalid/"));
        assert_eq!(
            ApiHome::of(&PageLocation::Html("<p>hi</p>".into()), None),
            None
        );
        assert!(!html.covers("data:text/html;base64,PHA+"));
        assert!(!html.covers("https://example.com/"));
        assert_eq!(ApiHome::of(&PageLocation::Blank, None), None);
    }

    #[test]
    fn the_rule_crosses_as_two_strings() {
        for home in [
            ApiHome::Web("https://a.b/".into()),
            ApiHome::File("file:///x/".into()),
        ] {
            let (kind, value) = home.to_parts();
            assert_eq!(ApiHome::from_parts(kind, value), Some(home.clone()));
        }
        assert_eq!(ApiHome::from_parts("file", "file:///x"), None);
        assert_eq!(ApiHome::from_parts("blank", ""), None);
    }

    #[test]
    fn an_update_is_json() {
        let data = PageData {
            clock: Some((10.5, 0.25)),
            source: Some(r#"{"shown":true}"#.into()),
            scenes: None,
            levels: Some(r#"{"peak":[1,0.5],"rms":[0.25,0.25],"bands":[0]}"#.into()),
            midi: Some(r#"{"input":"Keys","notes":[[1,60,100]]}"#.into()),
            values: vec![
                ("status.text".into(), Some(r#""On air""#.into())),
                ("counter.a".into(), None),
            ],
        };
        assert_eq!(
            update_json(&data),
            r#"{"clock":{"t":10.5,"lead":0.25},"source":{"shown":true},"levels":{"peak":[1,0.5],"rms":[0.25,0.25],"bands":[0]},"midi":{"input":"Keys","notes":[[1,60,100]]},"values":{"status.text":"On air","counter.a":null}}"#
        );
        assert_eq!(update_json(&PageData::default()), "{}");
        // A name that isn't a value's is never written.
        let bad = PageData {
            values: vec![("a\",\"b".into(), Some("1".into()))],
            ..PageData::default()
        };
        assert_eq!(update_json(&bad), r#"{"values":{}}"#);
    }

    #[test]
    fn the_cache_keeps_the_latest_of_each_and_moves_the_clock_on() {
        let mut cache = PageCache::default();
        cache.merge(&PageData {
            clock: Some((5.0, 0.1)),
            source: Some("1".into()),
            values: vec![
                ("a".into(), Some("1".into())),
                ("b".into(), Some("2".into())),
            ],
            ..PageData::default()
        });
        cache.merge(&PageData {
            scenes: Some("2".into()),
            levels: Some("null".into()),
            values: vec![("a".into(), None), ("b".into(), Some("3".into()))],
            ..PageData::default()
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        let full = cache.full();
        let (t, lead) = full.clock.unwrap();
        assert!((5.02..6.0).contains(&t), "{t}");
        assert_eq!(lead, 0.1);
        assert_eq!(full.source.as_deref(), Some("1"));
        assert_eq!(full.scenes.as_deref(), Some("2"));
        assert_eq!(full.levels.as_deref(), Some("null"));
        assert_eq!(full.values, [("b".to_owned(), Some("3".to_owned()))]);
    }

    #[test]
    fn a_big_update_splits_with_each_value_whole() {
        let value = format!("\"{}\"", "x".repeat(16 * 1024 - 2));
        let data = PageData {
            clock: Some((1.0, 0.0)),
            values: (0..40)
                .map(|n| (format!("v{n}"), Some(value.clone())))
                .collect(),
            ..PageData::default()
        };
        let parts = data.clone().split();
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|part| part.bytes() <= MAX_PAGE_DATA_BYTES));
        assert_eq!(parts[0].clock, data.clock);
        let values: Vec<_> = parts.into_iter().flat_map(|part| part.values).collect();
        assert_eq!(values, data.values);
    }

    #[test]
    fn the_shim_names_the_grant() {
        let shim = shim(PageApi {
            scenes: true,
            values: false,
            levels: true,
            midi: true,
        });
        assert!(shim.ends_with("})({scenes:true,values:false,levels:true,midi:true})\n"));
        assert!(!shim.contains("__GRANT__"));
    }
}
