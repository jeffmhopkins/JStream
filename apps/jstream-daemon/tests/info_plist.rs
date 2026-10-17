//! The daemon's `Info.plist` carries the usage strings macOS shows in its camera,
//! microphone and system audio recording prompts (ADR-0063 §11), and on macOS the built
//! binary embeds it, so the prompts work without an app bundle
//! (apps/jstream-daemon/README.md#purpose).

/// The usage-string keys macOS requires before it prompts for each permission.
const USAGE_KEYS: [&str; 3] = [
    "NSMicrophoneUsageDescription",
    "NSCameraUsageDescription",
    "NSAudioCaptureUsageDescription",
];

/// The plist as checked in.
const PLIST: &str = include_str!("../Info.plist");

/// The pinned facts, which name the daemon's bundle identifier.
const FACTS: &str = include_str!("../../../docs/facts.toml");

/// The string value right after `<key>{key}</key>`, if there is one.
fn string_after(key: &str) -> Option<&'static str> {
    let tag = format!("<key>{key}</key>");
    let at = PLIST.find(&tag)?;
    PLIST[at + tag.len()..]
        .trim_start()
        .strip_prefix("<string>")
        .and_then(|r| r.split_once("</string>"))
        .map(|(v, _)| v.trim())
}

/// A fact's value from `docs/facts.toml`.
fn fact(id: &str) -> Option<&'static str> {
    let at = FACTS.find(&format!("id = \"{id}\""))?;
    let rest = &FACTS[at..];
    let value = rest.find("value = \"")? + "value = \"".len();
    rest[value..].split('"').next()
}

#[test]
fn the_bundle_identifier_is_the_pinned_one_under_the_app_id() {
    let pinned = fact("macos-daemon-bundle-id").expect("the fact");
    assert_eq!(string_after("CFBundleIdentifier"), Some(pinned));
    let app = fact("macos-app-id").expect("the fact");
    assert!(
        pinned.starts_with(&format!("{app}.")),
        "{pinned} is not under {app}"
    );
}

#[test]
fn the_plist_has_every_usage_string() {
    assert!(PLIST.starts_with("<?xml"), "not an XML plist");
    for key in USAGE_KEYS {
        let tag = format!("<key>{key}</key>");
        let at = PLIST
            .find(&tag)
            .unwrap_or_else(|| panic!("{key} is missing"));
        // Each key is followed by a non-empty string value.
        let rest = PLIST[at + tag.len()..].trim_start();
        let value = rest
            .strip_prefix("<string>")
            .and_then(|r| r.split_once("</string>"))
            .map(|(v, _)| v.trim())
            .unwrap_or_default();
        assert!(!value.is_empty(), "{key} has no string value");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_binary_embeds_the_plist() {
    let image = std::fs::read(env!("CARGO_BIN_EXE_jstream-daemon")).expect("the binary");
    let body = PLIST.as_bytes();
    assert!(
        image.windows(body.len()).any(|w| w == body),
        "the built daemon doesn't carry Info.plist (build.rs's -sectcreate)"
    );
}
