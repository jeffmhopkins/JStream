//! The `youtube_chat` source: a browser source preset showing a live video's chat
//! through YouTube's public popout chat page, which needs no sign-in and no Google
//! OAuth client (ADR-0051, proposed). It shows chat and can't send, feature or moderate.

use crate::browser::open;
use crate::describe::{kind, param, size};
use crate::source::setting;
use crate::{Context, Source, SourceError, SourceKind};
use jstream_browser::{MAX_CSS_BYTES, MAX_FPS, PageLocation};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::json;

/// The popout chat page, before the video ID.
pub(crate) const POPOUT_CHAT: &str = "https://www.youtube.com/live_chat?is_popout=1&v=";

/// The longest `video` setting, in characters: room for a full URL.
const MAX_VIDEO_CHARS: u32 = 2048;

/// The kind's ID.
pub(crate) const KIND: &str = "youtube_chat";

/// The CSS a new chat source starts with: a transparent page with no scroll bars, and
/// the chat's header, ticker and message box hidden, so only the messages show.
pub const YOUTUBE_CHAT_CSS: &str = "html, body, yt-live-chat-app, yt-live-chat-renderer, \
#item-list, #item-scroller, #items { background: transparent !important; } \
body { margin: 0; overflow: hidden; } \
yt-live-chat-header-renderer, #ticker, #input-panel, yt-live-chat-message-input-renderer, \
#panel-pages { display: none !important; }";

/// The `youtube_chat` kind.
pub(crate) struct YoutubeChatKind(KindDescriptor);

impl YoutubeChatKind {
    pub(crate) fn new() -> Self {
        let text = |max_len, multiline| ParamType::String { multiline, max_len };
        Self(KindDescriptor {
            params: vec![
                ParamDescriptor {
                    description: Some(
                        "The live video: its ID, or a youtube.com/watch, youtube.com/live, \
                         youtube.com/live_chat or youtu.be link. Empty shows nothing."
                            .into(),
                    ),
                    ..param(
                        "video",
                        "Live video",
                        text(MAX_VIDEO_CHARS, false),
                        json!(""),
                    )
                },
                param("width", "Width", size(), json!(400)),
                param("height", "Height", size(), json!(600)),
                param(
                    "fps",
                    "Frame rate",
                    ParamType::Int {
                        min: Some(1),
                        max: Some(i64::from(MAX_FPS)),
                        step: Some(1),
                        unit: Some("fps".into()),
                    },
                    json!(30),
                ),
                ParamDescriptor {
                    description: Some(
                        "CSS added to the chat page. The default makes it transparent and \
                         hides everything but the messages."
                            .into(),
                    ),
                    live: true,
                    ..param(
                        "css",
                        "Custom CSS",
                        text(u32::try_from(MAX_CSS_BYTES).unwrap_or(u32::MAX), true),
                        json!(YOUTUBE_CHAT_CSS),
                    )
                },
            ],
            ..kind(KIND, "YouTube chat", true, true)
        })
    }
}

impl SourceKind for YoutubeChatKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let bad = |reason: &str| SourceError::Setting {
            param: "video".into(),
            reason: reason.into(),
        };
        let video = setting(settings, "video")?
            .as_str()
            .ok_or_else(|| bad("must be a string"))?;
        let location = match video_id(video) {
            Some("") => PageLocation::Blank,
            Some(id) => {
                PageLocation::web(&format!("{POPOUT_CHAT}{id}")).map_err(|e| bad(&e.to_string()))?
            }
            None => {
                return Err(bad(
                    "must be a video ID or a youtube.com/watch, youtube.com/live, youtube.com/live_chat or youtu.be link",
                ));
            }
        };
        open(&self.0, settings, ctx, location)
    }
}

/// The video ID `video` names: an ID itself, or one taken from a `youtube.com/watch?v=`,
/// `youtube.com/live/`, `youtube.com/live_chat?v=` or `youtu.be/` link. `Some("")` for
/// an empty setting; `None` when it names no video.
pub(crate) fn video_id(video: &str) -> Option<&str> {
    let video = video.trim();
    if video.is_empty() {
        return Some("");
    }
    let id = if video.contains('/') {
        // The scheme and host match in any case, as URLs do; the path and ID don't.
        let rest = strip_prefix_ci(video, "https://")
            .or_else(|| strip_prefix_ci(video, "http://"))
            .unwrap_or(video);
        let (host, path) = rest.split_once('/')?;
        let host = strip_prefix_ci(host, "www.").unwrap_or(host);
        let host = strip_prefix_ci(host, "m.").unwrap_or(host);
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let path = path.split('#').next().unwrap_or(path);
        let query = query.split('#').next().unwrap_or(query);
        let from_query = || query.split('&').find_map(|pair| pair.strip_prefix("v="));
        let path = path.trim_end_matches('/');
        if host.eq_ignore_ascii_case("youtu.be") {
            path
        } else if host.eq_ignore_ascii_case("youtube.com") {
            match path {
                "watch" | "live_chat" => from_query()?,
                path => path.strip_prefix("live/")?,
            }
        } else {
            return None;
        }
    } else {
        video
    };
    // YouTube's video IDs are 11 characters of [A-Za-z0-9_-]; any length is allowed in
    // case that changes, but nothing else, so the ID can't reshape the URL.
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then_some(id)
}

/// `s` without `prefix`, matched in any ASCII case.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::video_id;

    #[test]
    fn video_ids_come_from_ids_and_links() {
        for (video, id) in [
            ("dQw4w9WgXcQ", Some("dQw4w9WgXcQ")),
            ("  dQw4w9WgXcQ ", Some("dQw4w9WgXcQ")),
            ("", Some("")),
            (
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                Some("dQw4w9WgXcQ"),
            ),
            (
                "https://youtube.com/watch?feature=share&v=dQw4w9WgXcQ#t",
                Some("dQw4w9WgXcQ"),
            ),
            ("youtube.com/live/dQw4w9WgXcQ?si=abc", Some("dQw4w9WgXcQ")),
            (
                "https://m.youtube.com/live/dQw4w9WgXcQ/",
                Some("dQw4w9WgXcQ"),
            ),
            ("https://youtu.be/dQw4w9WgXcQ?t=5", Some("dQw4w9WgXcQ")),
            (
                "https://www.youtube.com/live_chat?is_popout=1&v=dQw4w9WgXcQ",
                Some("dQw4w9WgXcQ"),
            ),
            ("https://example.com/watch?v=dQw4w9WgXcQ", None),
            ("https://www.youtube.com/@channel/live", None),
            ("https://www.youtube.com/watch?list=x", None),
            ("abc&x=1", None),
            ("abc def", None),
            ("https://youtu.be/", None),
            // The scheme and host in any case; the ID keeps its own.
            (
                "HTTPS://WWW.YouTube.com/watch?v=dQw4w9WgXcQ",
                Some("dQw4w9WgXcQ"),
            ),
            ("Youtu.Be/dQw4w9WgXcQ", Some("dQw4w9WgXcQ")),
            ("https://youtube.com/WATCH?v=dQw4w9WgXcQ", None),
        ] {
            assert_eq!(video_id(video), id, "{video}");
        }
    }
}
