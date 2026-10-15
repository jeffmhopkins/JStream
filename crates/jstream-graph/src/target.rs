//! Validation of an output's target: the RTMP(S) server URL an output publishes to,
//! and the file a recording output writes.
//!
//! The collection is saved in plain text, so a URL must never carry a credential
//! (R-SEC-001, docs/RULES.md#R-SEC-001). The stream key goes to the OS credential store
//! through `output.set_key` instead. So a URL with `user:password@`, a `?query` or a
//! `#fragment` is rejected, as is anything `jstream-outputs`' `RtmpUrl::parse` would
//! refuse. A key pasted into the path (`rtmp://host/app/<key>`) looks like a nested
//! application name and can't be told apart; the path is stored as given.
//!
//! The rejection reasons are fixed text. They never repeat the URL, which might hold a
//! key.
//!
//! A recording path is checked for its shape only: absolute on the engine's machine and
//! ending in a file name. Whether its directory exists, and whether a file is already
//! there, is checked by `output.start`, when the recording is opened.

/// The longest URL accepted, in bytes.
pub(crate) const MAX_URL_LEN: usize = 2048;
/// The longest recording path accepted, in bytes.
pub(crate) const MAX_PATH_LEN: usize = 4096;

/// Why `path` is not an acceptable recording path, or `None` when it is.
///
/// The check is the same on every platform, as `check_asset_path` is for an absolute
/// asset: `/…`, `\…`, `C:/…` or `C:\…`, with `/` or `\` separators, no `..` segment,
/// and a file name last. The engine checks again, when it opens the file, that the
/// path is absolute on its own machine.
pub(crate) fn record_path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("must not be empty");
    }
    if path.len() > MAX_PATH_LEN {
        return Some("must be at most 4096 bytes");
    }
    if path.chars().any(char::is_control) {
        return Some("must not contain control characters");
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    if !(path.starts_with(['/', '\\']) || drive) {
        return Some("must be absolute (`/…`, or `C:\\…` on Windows)");
    }
    let mut segments = path.split(['/', '\\']);
    if segments.clone().any(|segment| segment == "..") {
        return Some("must not contain a `..` segment");
    }
    match segments.next_back() {
        Some("" | ".") | None => Some("must end in a file name"),
        Some(_) => None,
    }
}

/// Why `url` is not an acceptable RTMP(S) server URL, or `None` when it is.
pub(crate) fn rtmp_url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() {
        return Some("must not be empty");
    }
    if url.len() > MAX_URL_LEN {
        return Some("must be at most 2048 bytes");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("must not contain whitespace or control characters");
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Some("must start with `rtmp://` or `rtmps://`");
    };
    if !(scheme.eq_ignore_ascii_case("rtmp") || scheme.eq_ignore_ascii_case("rtmps")) {
        return Some("the scheme must be `rtmp` or `rtmps`");
    }
    if rest.contains(['?', '#']) {
        return Some("must not have a query or fragment: set the stream key with `output.set_key`");
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.contains('@') {
        return Some(
            "must not carry credentials (`user:password@`): set the stream key with \
             `output.set_key`",
        );
    }
    if let Some(problem) = host_port_problem(authority) {
        return Some(problem);
    }
    if path.trim_matches('/').is_empty() {
        return Some("is missing the application name (the path after the host)");
    }
    None
}

/// Checks `host[:port]` or `[v6]:port`, as `RtmpUrl::parse` splits it.
fn host_port_problem(authority: &str) -> Option<&'static str> {
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let Some((host, after)) = v6.split_once(']') else {
            return Some("has an unclosed `[` in its IPv6 host");
        };
        match after {
            "" => (host, None),
            _ => match after.strip_prefix(':') {
                Some(port) => (host, Some(port)),
                None => return Some("has unexpected text after its IPv6 host"),
            },
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Some("is missing the host");
    }
    match port.map(str::parse::<u16>) {
        None | Some(Ok(1..)) => None,
        Some(_) => Some("has a port that is not a number in 1..=65535"),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PATH_LEN, MAX_URL_LEN, record_path_problem, rtmp_url_problem};

    #[test]
    fn recording_paths_are_accepted() {
        for path in [
            "/show.mp4",
            "/home/me/Videos/show.mp4",
            "/home/me/Videos/show",
            "/home/me/My Videos/show 2.mp4",
            "C:\\Users\\me\\Videos\\show.mp4",
            "C:/Users/me/Videos/show.mp4",
            "\\\\server\\share\\show.mp4",
            "/home/me/.hidden.mp4",
        ] {
            assert_eq!(record_path_problem(path), None, "{path}");
        }
    }

    #[test]
    fn bad_recording_paths_are_rejected() {
        let long = format!("/{}", "a".repeat(MAX_PATH_LEN));
        for path in [
            "",
            long.as_str(),
            "show.mp4",
            "Videos/show.mp4",
            "C:show.mp4",
            "/Videos/show\n.mp4",
            "/Videos/show\0.mp4",
            "/",
            "/Videos/",
            "C:\\Videos\\",
            "/Videos/.",
            "/Videos/..",
            "/home/me/../other/show.mp4",
        ] {
            assert!(record_path_problem(path).is_some(), "{path:?} was accepted");
        }
    }

    #[test]
    fn server_urls_are_accepted() {
        for url in [
            "rtmp://a.rtmp.youtube.com/live2",
            "rtmps://a.rtmps.youtube.com:443/live2",
            "RTMP://live.twitch.tv/app/",
            "rtmps://live-api-s.facebook.com:443/rtmp/",
            "rtmp://127.0.0.1:1935/live",
            "rtmp://[::1]:1935/live",
            "rtmp://[::1]/live",
            "rtmp://host/app/instance",
        ] {
            assert_eq!(rtmp_url_problem(url), None, "{url}");
        }
    }

    #[test]
    fn bad_urls_are_rejected_without_being_repeated() {
        let long = format!("rtmp://host/{}", "a".repeat(MAX_URL_LEN));
        for url in [
            "",
            long.as_str(),
            "rtmp://host/live 2",
            "rtmp://host/live\n",
            "a.rtmp.youtube.com/live2",
            "http://host/live",
            "srt://host/live",
            "rtmp://user:secret-key@host/live",
            "rtmp://secret-key@host/live",
            "rtmp://host/live?key=secret-key",
            "rtmp://host/live?secret-key",
            "rtmp://host/live#secret-key",
            "rtmp:///live",
            "rtmp://:1935/live",
            "rtmp://host:0/live",
            "rtmp://host:99999/live",
            "rtmp://host:port/live",
            "rtmp://[::1/live",
            "rtmp://[::1]x/live",
            "rtmp://host",
            "rtmp://host/",
            "rtmp://host//",
        ] {
            let problem = rtmp_url_problem(url);
            assert!(problem.is_some(), "{url:?} was accepted");
            let problem = problem.unwrap_or_default();
            assert!(!problem.contains("secret"), "{url:?}: {problem}");
        }
    }
}
