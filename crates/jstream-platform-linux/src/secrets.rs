//! Secrets on Linux (ADR-0071, R-SEC-001): stream keys and OAuth refresh tokens go to
//! the desktop's keyring through the Secret Service, and only where none is reachable to
//! a file only the user can read.
//!
//! `LinuxSecrets` implements [`jstream_platform::StreamKeyStore`] and
//! [`jstream_platform::OAuthTokenStore`], as `MacosKeychain` and `WindowsCredentials` do.
//! Each call opens the session bus:
//! - **A Secret Service answers.** The secret goes in the default collection, found by
//!   the attributes `application = "jstream"`, `kind` and `id`, and labelled with the
//!   Windows credential name (`JStream/stream-key/<output id>`). A locked collection or
//!   item is unlocked, which may show the desktop's prompt; a dismissed prompt fails the
//!   call. Secrets the file still holds are moved into the keyring first, and the file
//!   is deleted once it is empty.
//! - **None is reachable** (no session bus, or nothing provides
//!   `org.freedesktop.secrets` and the bus can't start it). The secret goes to the file
//!   `secrets` in `$XDG_CONFIG_HOME/jstream`: plaintext, `0600`, in a folder created
//!   `0700`, and refused if anyone else could read it or owns it. A keyring that refuses,
//!   is locked or fails is an error, never a reason to use the file.
//!
//! The item names, the file's format and the Secret Service's messages are platform
//! independent and tested everywhere; the store exists only on Linux.
//!
//! Docs: docs/adr/0071-linux-secret-store.md, docs/ARCHITECTURE.md#persistence

use std::collections::BTreeMap;

use jstream_platform::{PlatformError, Secret};

use crate::dbus::{Message, Value};

#[cfg(target_os = "linux")]
mod store;
#[cfg(target_os = "linux")]
pub use store::LinuxSecrets;

/// The `application` attribute of every item JStream stores.
const APPLICATION: &str = "jstream";

/// The fallback file's name, in the config folder.
pub(crate) const FILE_NAME: &str = "secrets";

/// The first lines of the fallback file, so whoever opens it knows what it is.
const FILE_HEADER: &str = "\
# JStream secrets (ADR-0071). NOT ENCRYPTED: JStream keeps stream keys and OAuth tokens
# here only while no Secret Service (GNOME Keyring, KWallet) is reachable, and moves
# them into the keyring, deleting this file, once one is. Never share this file.
";

/// The kind of secret, which keeps a stream key and a token with the same ID apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    /// A stream key, by output ID.
    StreamKey,
    /// An OAuth refresh token, by account ID.
    OAuthToken,
}

impl Kind {
    /// The `kind` attribute, and the kind's word in the file and in labels.
    fn word(self) -> &'static str {
        match self {
            Self::StreamKey => "stream-key",
            Self::OAuthToken => "oauth-token",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        [Self::StreamKey, Self::OAuthToken]
            .into_iter()
            .find(|k| k.word() == word)
    }
}

/// One secret's name, validated: its kind and the output or account ID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Name {
    kind: Kind,
    id: String,
}

impl Name {
    /// The name of the secret of `kind` for `id`. IDs are opaque, so any is taken except
    /// an empty one or one with a control character (a keyring manager can't show it, and
    /// the file keeps one secret per line). Those are `Unsupported`.
    pub(crate) fn new(kind: Kind, id: &str) -> Result<Self, PlatformError> {
        let refuse = |why: &str| PlatformError::Unsupported {
            what: format!("secret name for ID {id:?}: {why}"),
        };
        if id.is_empty() {
            return Err(refuse("the ID is empty"));
        }
        if id.chars().any(char::is_control) {
            return Err(refuse("the ID holds a control character"));
        }
        Ok(Self {
            kind,
            id: id.to_owned(),
        })
    }

    /// The item's label, the Windows credential name: `JStream/<kind>/<id>`.
    pub(crate) fn label(&self) -> String {
        format!("JStream/{}/{}", self.kind.word(), self.id)
    }

    /// The item's lookup attributes, an `a{ss}`.
    fn attributes(&self) -> Value {
        let entry = |k: &str, v: &str| {
            Value::Entry(
                Box::new(Value::Str(k.into())),
                Box::new(Value::Str(v.into())),
            )
        };
        Value::Array(
            "{ss}".into(),
            vec![
                entry("application", APPLICATION),
                entry("kind", self.kind.word()),
                entry("id", &self.id),
            ],
        )
    }
}

/// Checks that a secret can be stored: not empty (a cleared secret has no item). The
/// error never names the secret.
pub(crate) fn check_secret(secret: &Secret) -> Result<(), PlatformError> {
    if secret.expose().is_empty() {
        return Err(PlatformError::Unsupported {
            what: "an empty secret".into(),
        });
    }
    Ok(())
}

/// The warning the host shows and logs while the fallback file holds a secret
/// (ADR-0071 §4). It names the file and never a secret.
pub(crate) fn fallback_warning(path: &std::path::Path) -> String {
    format!(
        "no Secret Service (GNOME Keyring, KWallet) is reachable on the session bus, so \
         stream keys and OAuth tokens are stored NOT ENCRYPTED in {}; anyone who can read \
         that file, or a backup of it, can use them. Start a keyring in this session (for \
         example gnome-keyring-daemon) and JStream moves them into it",
        path.display()
    )
}

/// What the fallback file holds for one name.
#[derive(Debug)]
pub(crate) enum Entry {
    /// The secret.
    Set(Secret),
    /// The secret was cleared while no keyring was reachable: the keyring's copy, if it
    /// has one, is deleted when the entry moves there, so a cleared key or revoked token
    /// doesn't come back.
    Cleared,
}

/// The fallback file's entries, by name.
pub(crate) type Contents = BTreeMap<Name, Entry>;

/// Whether the file holds any secret (cleared entries hold none).
pub(crate) fn holds_secrets(contents: &Contents) -> bool {
    contents.values().any(|e| matches!(e, Entry::Set(_)))
}

/// The file's text for `contents`: the header, then one line per entry,
/// `<kind>\t<id>\t<secret>` for a secret and `<kind>\t<id>` for a cleared one, with `\`,
/// tab, CR and LF escaped as `\\`, `\t`, `\r` and `\n`. The text is a [`Secret`], so it
/// is zeroed when dropped.
pub(crate) fn serialize(contents: &Contents) -> Secret {
    let mut len = FILE_HEADER.len();
    for (name, entry) in contents {
        len += name.kind.word().len() + 2 * name.id.len() + 2;
        if let Entry::Set(s) = entry {
            len += 2 * s.expose().len() + 1;
        }
    }
    // Room for every character escaped, so the buffer never grows and leaves an
    // unzeroed copy behind.
    let mut text = String::with_capacity(len);
    text.push_str(FILE_HEADER);
    for (name, entry) in contents {
        text.push_str(name.kind.word());
        text.push('\t');
        escape(&name.id, &mut text);
        if let Entry::Set(s) = entry {
            text.push('\t');
            escape(s.expose(), &mut text);
        }
        text.push('\n');
    }
    Secret::new(text)
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

/// The text unescaped, or `None` for an unknown escape. The result never grows past the
/// text's length, so it leaves no unzeroed copy.
fn unescape(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let c = match chars.next() {
            Some('\\') => '\\',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('n') => '\n',
            _ => {
                drop(Secret::new(out));
                return None;
            }
        };
        out.push(c);
    }
    Some(out)
}

/// Parses the file's text. Lines that are empty or start with `#` are skipped. A
/// malformed line is an error that names its number, never its contents.
pub(crate) fn parse(text: &str) -> Result<Contents, PlatformError> {
    let mut contents = Contents::new();
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = || PlatformError::Os {
            call: "reading the secrets file",
            code: None,
            message: format!("line {} is malformed", n + 1),
        };
        let mut fields = line.split('\t');
        let kind = fields.next().and_then(Kind::from_word).ok_or_else(bad)?;
        let id = fields.next().and_then(unescape).ok_or_else(bad)?;
        let name = Name::new(kind, &id).map_err(|_| bad())?;
        let entry = match fields.next() {
            None => Entry::Cleared,
            Some(field) => {
                let secret = Secret::new(unescape(field).ok_or_else(bad)?);
                if secret.expose().is_empty() {
                    return Err(bad());
                }
                Entry::Set(secret)
            }
        };
        if fields.next().is_some() {
            return Err(bad());
        }
        contents.insert(name, entry);
    }
    Ok(contents)
}

/// The Secret Service's names and the messages the store sends, built and read here so
/// they are tested without a bus.
pub(crate) mod service {
    use super::{Message, Name, PlatformError, Secret, Value};

    /// The service's well-known bus name.
    pub(crate) const BUS_NAME: &str = "org.freedesktop.secrets";
    /// The service object.
    pub(crate) const PATH: &str = "/org/freedesktop/secrets";
    const SERVICE: &str = "org.freedesktop.Secret.Service";
    const COLLECTION: &str = "org.freedesktop.Secret.Collection";
    const ITEM: &str = "org.freedesktop.Secret.Item";
    /// The prompt interface, whose `Completed` signal ends a prompt.
    pub(crate) const PROMPT: &str = "org.freedesktop.Secret.Prompt";
    /// The path a method returns for "no object" (no prompt, no collection).
    pub(crate) const NONE: &str = "/";
    /// The content type of every secret JStream stores.
    const CONTENT_TYPE: &str = "text/plain; charset=utf8";

    fn call(path: &str, interface: &str, member: &str, body: Vec<Value>) -> Message {
        Message::call(BUS_NAME, path, interface, member, body)
    }

    fn paths(paths: &[String]) -> Value {
        Value::Array(
            "o".into(),
            paths.iter().map(|p| Value::Path(p.clone())).collect(),
        )
    }

    /// `OpenSession("plain", "")`: secrets cross the user's own session bus unencrypted
    /// (ADR-0071 §1).
    pub(crate) fn open_session() -> Message {
        call(
            PATH,
            SERVICE,
            "OpenSession",
            vec![
                Value::Str("plain".into()),
                Value::Variant(Box::new(Value::Str(String::new()))),
            ],
        )
    }

    /// `SearchItems` for the item named `name`.
    pub(crate) fn search_items(name: &Name) -> Message {
        call(PATH, SERVICE, "SearchItems", vec![name.attributes()])
    }

    /// `Unlock` of these objects.
    pub(crate) fn unlock(objects: &[String]) -> Message {
        call(PATH, SERVICE, "Unlock", vec![paths(objects)])
    }

    /// `ReadAlias("default")`.
    pub(crate) fn read_default_alias() -> Message {
        call(
            PATH,
            SERVICE,
            "ReadAlias",
            vec![Value::Str("default".into())],
        )
    }

    /// `CreateCollection` of the default collection, for a keyring that has none yet.
    pub(crate) fn create_default_collection() -> Message {
        call(
            PATH,
            SERVICE,
            "CreateCollection",
            vec![
                Value::dict(vec![(
                    "org.freedesktop.Secret.Collection.Label",
                    Value::Str("Default keyring".into()),
                )]),
                Value::Str("default".into()),
            ],
        )
    }

    /// `GetSecret` of `item` in `session`.
    pub(crate) fn get_secret(item: &str, session: &str) -> Message {
        call(item, ITEM, "GetSecret", vec![Value::Path(session.into())])
    }

    /// `CreateItem` in `collection` replacing any item with the same attributes. The
    /// message holds the secret: the caller wipes it with [`wipe`] once it is sent.
    pub(crate) fn create_item(
        collection: &str,
        name: &Name,
        session: &str,
        secret: &Secret,
    ) -> Message {
        let bytes = secret.expose().bytes().map(Value::Byte).collect();
        call(
            collection,
            COLLECTION,
            "CreateItem",
            vec![
                Value::dict(vec![
                    (
                        "org.freedesktop.Secret.Item.Label",
                        Value::Str(name.label()),
                    ),
                    ("org.freedesktop.Secret.Item.Attributes", name.attributes()),
                ]),
                Value::Struct(vec![
                    Value::Path(session.into()),
                    Value::Array("y".into(), Vec::new()),
                    Value::Array("y".into(), bytes),
                    Value::Str(CONTENT_TYPE.into()),
                ]),
                Value::Bool(true),
            ],
        )
    }

    /// `Delete` of `item`.
    pub(crate) fn delete(item: &str) -> Message {
        call(item, ITEM, "Delete", Vec::new())
    }

    /// `Prompt` of `prompt`, with no parent window.
    pub(crate) fn prompt(prompt: &str) -> Message {
        call(prompt, PROMPT, "Prompt", vec![Value::Str(String::new())])
    }

    /// Zeroes every byte array in `values`, such as the secret in a `CreateItem` call or
    /// a `GetSecret` reply.
    pub(crate) fn wipe(values: &mut [Value]) {
        for v in values.iter_mut() {
            match v {
                Value::Byte(b) => *b = 0,
                Value::Array(_, items) | Value::Struct(items) => wipe(items),
                Value::Variant(inner) => wipe(std::slice::from_mut(inner.as_mut())),
                Value::Entry(k, v) => {
                    wipe(std::slice::from_mut(k.as_mut()));
                    wipe(std::slice::from_mut(v.as_mut()));
                }
                _ => {}
            }
        }
        std::hint::black_box(&*values);
    }

    /// Whether an error reply means no Secret Service is reachable: nothing owns or
    /// provides the name, or the bus failed to start what does (ADR-0071 §2). Any other
    /// error is the keyring failing, never a reason to use the file.
    pub(crate) fn unreachable(error_name: &str) -> bool {
        error_name == "org.freedesktop.DBus.Error.ServiceUnknown"
            || error_name == "org.freedesktop.DBus.Error.NameHasNoOwner"
            || error_name.starts_with("org.freedesktop.DBus.Error.Spawn.")
    }

    fn malformed(what: &'static str) -> PlatformError {
        PlatformError::Os {
            call: what,
            code: None,
            message: "the Secret Service's reply has an unexpected shape".into(),
        }
    }

    /// The object paths in an `ao`.
    pub(crate) fn object_paths(
        v: &Value,
        what: &'static str,
    ) -> Result<Vec<String>, PlatformError> {
        let Value::Array(_, items) = v.unwrap_variant() else {
            return Err(malformed(what));
        };
        items
            .iter()
            .map(|i| match i {
                Value::Path(p) => Ok(p.clone()),
                _ => Err(malformed(what)),
            })
            .collect()
    }

    /// The object path in an `o` (or a variant holding one).
    pub(crate) fn object_path(
        v: Option<&Value>,
        what: &'static str,
    ) -> Result<String, PlatformError> {
        match v.map(Value::unwrap_variant) {
            Some(Value::Path(p)) => Ok(p.clone()),
            _ => Err(malformed(what)),
        }
    }

    /// The secret in a `GetSecret` reply, `((oayays))`. The reply's own copy is left
    /// for the caller to [`wipe`].
    pub(crate) fn secret_of(body: &[Value]) -> Result<Secret, PlatformError> {
        const WHAT: &str = "org.freedesktop.Secret.Item.GetSecret";
        let [Value::Struct(fields)] = body else {
            return Err(malformed(WHAT));
        };
        let Some(Value::Array(_, value)) = fields.get(2) else {
            return Err(malformed(WHAT));
        };
        // Exactly the secret's length, so the Vec never grows and leaves no copy.
        let mut bytes = Vec::with_capacity(value.len());
        for b in value {
            let Value::Byte(b) = b else {
                drop(Secret::from_utf8(bytes));
                return Err(malformed(WHAT));
            };
            bytes.push(*b);
        }
        Secret::from_utf8(bytes)
    }

    /// The outcome of a prompt's `Completed` signal, `(bv)`: `None` when the user
    /// dismissed it, else its result.
    pub(crate) fn completed(body: &[Value]) -> Result<Option<&Value>, PlatformError> {
        match body {
            [Value::Bool(true), _] => Ok(None),
            [Value::Bool(false), result] => Ok(Some(result.unwrap_variant())),
            _ => Err(malformed("org.freedesktop.Secret.Prompt.Completed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::service::{self, completed, object_paths, secret_of, unreachable};
    use super::{
        Contents, Entry, FILE_HEADER, Kind, Name, Value, check_secret, fallback_warning,
        holds_secrets, parse, serialize,
    };
    use jstream_platform::{PlatformError, Secret};

    fn name(kind: Kind, id: &str) -> Name {
        Name::new(kind, id).expect("a valid ID")
    }

    #[test]
    fn names_label_as_on_windows_and_keep_kinds_apart() {
        let key = name(Kind::StreamKey, "out_yt");
        assert_eq!(key.label(), "JStream/stream-key/out_yt");
        let token = name(Kind::OAuthToken, "user@example.com");
        assert_eq!(token.label(), "JStream/oauth-token/user@example.com");
        assert_ne!(
            name(Kind::StreamKey, "same"),
            name(Kind::OAuthToken, "same")
        );
        assert_eq!(
            key.attributes().signature(),
            "a{ss}",
            "lookup attributes are string pairs"
        );
        let Value::Array(_, entries) = key.attributes() else {
            panic!("an array");
        };
        let pairs: Vec<(String, String)> = entries
            .iter()
            .filter_map(|e| match e {
                Value::Entry(k, v) => Some((k.as_str()?.to_owned(), v.as_str()?.to_owned())),
                _ => None,
            })
            .collect();
        assert_eq!(
            pairs,
            [
                ("application".to_owned(), "jstream".to_owned()),
                ("kind".to_owned(), "stream-key".to_owned()),
                ("id".to_owned(), "out_yt".to_owned()),
            ]
        );
    }

    #[test]
    fn rejects_bad_ids_and_empty_secrets() {
        for id in ["", "a\0b", "line\nbreak", "tab\there", "\u{7f}"] {
            for kind in [Kind::StreamKey, Kind::OAuthToken] {
                assert!(
                    matches!(Name::new(kind, id), Err(PlatformError::Unsupported { .. })),
                    "{id:?} must be refused"
                );
            }
        }
        assert!(check_secret(&Secret::new("k")).is_ok());
        assert_eq!(
            check_secret(&Secret::new(""))
                .expect_err("empty")
                .to_string(),
            "not supported by this backend: an empty secret"
        );
    }

    #[test]
    fn the_file_round_trips_secrets_and_cleared_entries() {
        let mut contents = Contents::new();
        contents.insert(
            name(Kind::StreamKey, "out\\1 é"),
            Entry::Set(Secret::new("live\tkey\\with\nodd\rbits")),
        );
        contents.insert(
            name(Kind::OAuthToken, "me@example.com"),
            Entry::Set(Secret::new("1//refresh")),
        );
        contents.insert(name(Kind::StreamKey, "gone"), Entry::Cleared);
        let text = serialize(&contents);
        assert!(text.expose().starts_with(FILE_HEADER));
        assert_eq!(
            text.expose()
                .lines()
                .filter(|l| !l.starts_with('#'))
                .count(),
            3,
            "one line per entry: {:?}",
            text.expose()
        );
        let back = parse(text.expose()).expect("parses");
        assert_eq!(back.len(), 3);
        let secret = |n: &Name| match back.get(n) {
            Some(Entry::Set(s)) => Some(s.expose().to_owned()),
            _ => None,
        };
        assert_eq!(
            secret(&name(Kind::StreamKey, "out\\1 é")).as_deref(),
            Some("live\tkey\\with\nodd\rbits")
        );
        assert_eq!(
            secret(&name(Kind::OAuthToken, "me@example.com")).as_deref(),
            Some("1//refresh")
        );
        assert!(matches!(
            back.get(&name(Kind::StreamKey, "gone")),
            Some(Entry::Cleared)
        ));
        assert!(holds_secrets(&back));
        let mut only_cleared = Contents::new();
        only_cleared.insert(name(Kind::StreamKey, "gone"), Entry::Cleared);
        assert!(!holds_secrets(&only_cleared));
    }

    #[test]
    fn a_malformed_file_names_the_line_and_never_its_contents() {
        for text in [
            "stream-key\tout\tkey\textra\n",
            "nonsense\tout\tsecret-text\n",
            "stream-key\tout\tbad\\qescape\n",
            "stream-key\t\tsecret-text\n",
            "stream-key\tout\t\n",
            "stream-key\n",
        ] {
            let err = parse(&format!("# header\n\n{text}"))
                .expect_err(text)
                .to_string();
            assert_eq!(err, "reading the secrets file failed: line 3 is malformed");
            assert!(
                !err.contains("secret-text") && !err.contains("key"),
                "{err}"
            );
        }
        assert!(parse("").expect("empty").is_empty());
    }

    #[test]
    fn the_warning_names_the_file_and_how_to_fix_it() {
        let warning = fallback_warning(std::path::Path::new("/home/u/.config/jstream/secrets"));
        assert!(
            warning.contains("/home/u/.config/jstream/secrets"),
            "{warning}"
        );
        assert!(warning.contains("NOT ENCRYPTED"), "{warning}");
        assert!(warning.contains("Start a keyring"), "{warning}");
    }

    #[test]
    fn messages_have_the_specified_signatures() {
        let key = name(Kind::StreamKey, "out");
        let session = "/org/freedesktop/secrets/session/s1";
        assert_eq!(service::open_session().signature(), "sv");
        assert_eq!(service::search_items(&key).signature(), "a{ss}");
        assert_eq!(service::unlock(&["/a".into()]).signature(), "ao");
        assert_eq!(service::read_default_alias().signature(), "s");
        assert_eq!(service::create_default_collection().signature(), "a{sv}s");
        assert_eq!(service::get_secret("/item", session).signature(), "o");
        assert_eq!(service::delete("/item").signature(), "");
        assert_eq!(service::prompt("/prompt").signature(), "s");
        let mut create = service::create_item(
            "/org/freedesktop/secrets/collection/login",
            &key,
            session,
            &Secret::new("live_abc"),
        );
        assert_eq!(create.signature(), "a{sv}(oayays)b");
        assert_eq!(
            create.path.as_deref(),
            Some("/org/freedesktop/secrets/collection/login")
        );
        assert_eq!(
            create.destination.as_deref(),
            Some("org.freedesktop.secrets")
        );
        let encoded = create.encode(1).expect("encodes");
        assert!(encoded.windows(8).any(|w| w == b"live_abc"));
        service::wipe(&mut create.body);
        let encoded = create.encode(1).expect("encodes");
        assert!(!encoded.windows(8).any(|w| w == b"live_abc"), "wiped");
        assert!(
            encoded.windows(10).any(|w| w == b"text/plain"),
            "only bytes are wiped"
        );
    }

    #[test]
    fn replies_are_read_and_checked() {
        let reply = vec![Value::Struct(vec![
            Value::Path("/s".into()),
            Value::Array("y".into(), Vec::new()),
            Value::Array(
                "y".into(),
                b"live_abc".iter().copied().map(Value::Byte).collect(),
            ),
            Value::Str("text/plain".into()),
        ])];
        assert_eq!(secret_of(&reply).expect("a secret").expose(), "live_abc");
        assert!(secret_of(&[Value::Str("x".into())]).is_err());
        let not_utf8 = vec![Value::Struct(vec![
            Value::Path("/s".into()),
            Value::Array("y".into(), Vec::new()),
            Value::Array("y".into(), vec![Value::Byte(0xFF)]),
            Value::Str("text/plain".into()),
        ])];
        assert!(matches!(
            secret_of(&not_utf8),
            Err(PlatformError::Unsupported { .. })
        ));

        let found = Value::Array("o".into(), vec![Value::Path("/i/1".into())]);
        assert_eq!(object_paths(&found, "x").expect("paths"), ["/i/1"]);
        assert!(object_paths(&Value::Str("/i/1".into()), "x").is_err());

        let done = [Value::Bool(false), Value::Variant(Box::new(found.clone()))];
        assert_eq!(completed(&done).expect("a signal"), Some(&found));
        let dismissed = [
            Value::Bool(true),
            Value::Variant(Box::new(Value::Str(String::new()))),
        ];
        assert_eq!(completed(&dismissed).expect("a signal"), None);
        assert!(completed(&[]).is_err());
    }

    #[test]
    fn only_a_missing_service_means_unreachable() {
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
            "org.freedesktop.DBus.Error.Spawn.ChildExited",
            "org.freedesktop.DBus.Error.Spawn.ServiceNotFound",
        ] {
            assert!(unreachable(name), "{name}");
        }
        for name in [
            "org.freedesktop.Secret.Error.IsLocked",
            "org.freedesktop.DBus.Error.AccessDenied",
            "org.freedesktop.DBus.Error.UnknownMethod",
            "org.freedesktop.DBus.Error.NoReply",
        ] {
            assert!(
                !unreachable(name),
                "{name}: the keyring failed, so no fallback"
            );
        }
    }
}
