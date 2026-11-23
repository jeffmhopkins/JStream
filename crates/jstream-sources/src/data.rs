//! Live data (ADR-0171 decision 2): each JTunnel sender's newest state message per key,
//! and the watches that read one field of one key without a lock.
//!
//! The JTunnel receive thread puts each whole message in with [`LiveData::put`], as
//! JSON bytes under its sender's name and key, and says a resend kept a key current
//! with [`LiveData::touch`]. A key not kept current for [`STALE_MS`] goes on the next
//! [`LiveData::expire`], and [`LiveData::restart`] forgets a sender that started again.
//! The protocol reads it with [`LiveData::list`] and [`LiveData::get`]; nothing else
//! writes it. It is runtime state: never saved, never an edit.
//!
//! **Watches.** Planning asks for a [`DataCell`] with [`LiveData::watch`], naming the
//! sender (or any), the key and a dotted field. Each time a message for that key lands,
//! the thread that puts it evaluates the field and stores the reading in the cell; a
//! key that goes, or a field it doesn't have, reads [`DataReading::Absent`]. The render
//! thread only loads the cell: one atomic load, no lock and no allocation
//! ([R-RT-003](../../../docs/RULES.md#R-RT-003)). A cell lives while someone holds it;
//! the store forgets a watch nobody holds.

use crate::color::parse_color;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// A key not refreshed for this long reads as absent: senders resend everything about
/// once a second.
pub const STALE_MS: u64 = 3_000;
/// The longest message, as JSON bytes: JTunnel's 64 pieces of 1,200 bytes.
pub const MAX_MESSAGE_BYTES: usize = 76_800;
/// The deepest a message nests, counting each object and list.
pub const MAX_DEPTH: usize = 32;
/// The most keys one sender holds.
pub const MAX_KEYS: usize = 1024;
/// The most bytes of messages one sender holds.
pub const MAX_SENDER_BYTES: usize = 4 << 20;
/// The most senders with state: the JTunnel receiver's accepted senders.
pub const MAX_INPUTS: usize = 32;
/// The most watches at once; one more reads absent and is not kept.
pub const MAX_WATCHES: usize = 4096;
/// The longest field path, in bytes.
pub const MAX_FIELD: usize = 255;

/// Why a message was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not one JSON value, or longer than [`MAX_MESSAGE_BYTES`].
    NotJson,
    /// Nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// A sender's key past [`MAX_KEYS`], or a sender past [`MAX_INPUTS`].
    TooManyKeys,
    /// A sender's messages past [`MAX_SENDER_BYTES`] in all.
    TooLarge,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotJson => "not one JSON value",
            Self::TooDeep => "nested too deep",
            Self::TooManyKeys => "too many keys",
            Self::TooLarge => "too much state from one sender",
        })
    }
}

/// What a watched field reads now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DataReading {
    /// No such sender, key or field, or the field is an object, a list or `null`.
    Absent,
    /// A number; `true` reads 1 and `false` 0.
    Number(f64),
    /// A text, by its [`text_hash`], so a table of texts can be matched without one.
    Text(u64),
    /// A `#rrggbb` or `#rrggbbaa` text, for a watch that asked for a colour.
    Color([u8; 4]),
}

/// A text's hash as a [`DataReading::Text`] holds it: 48 bits of FNV-1a over its bytes.
#[must_use]
pub fn text_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash & PAYLOAD
}

// A cell holds one u64: a number as its own bits (JSON numbers are finite, so a NaN is
// never one), anything else as a quiet NaN with a tag and a 48-bit payload.
const PAYLOAD: u64 = (1 << 48) - 1;
const ABSENT: u64 = 0x7FF9 << 48;
const TEXT: u64 = 0x7FFA << 48;
const COLOR: u64 = 0x7FFB << 48;
const TAG: u64 = 0xFFFF << 48;

fn pack(reading: DataReading) -> u64 {
    match reading {
        DataReading::Absent => ABSENT,
        DataReading::Number(n) if n.is_finite() => n.to_bits(),
        DataReading::Number(_) => ABSENT,
        DataReading::Text(hash) => TEXT | (hash & PAYLOAD),
        DataReading::Color(rgba) => COLOR | u64::from(u32::from_be_bytes(rgba)),
    }
}

fn unpack(bits: u64) -> DataReading {
    match bits & TAG {
        ABSENT => DataReading::Absent,
        TEXT => DataReading::Text(bits & PAYLOAD),
        COLOR => DataReading::Color(((bits & 0xFFFF_FFFF) as u32).to_be_bytes()),
        _ => DataReading::Number(f64::from_bits(bits)),
    }
}

/// One watched field's reading, written when its key's message lands and read without a
/// lock.
#[derive(Debug)]
pub struct DataCell(AtomicU64);

impl Default for DataCell {
    fn default() -> Self {
        Self(AtomicU64::new(ABSENT))
    }
}

impl DataCell {
    /// What the field reads now: one atomic load.
    pub fn read(&self) -> DataReading {
        unpack(self.0.load(Ordering::Acquire))
    }

    fn set(&self, reading: DataReading) {
        self.0.store(pack(reading), Ordering::Release);
    }
}

/// What a watch reads: a sender (or any), a key and a field in its message.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DataWatch {
    /// The sender's name, or `None` for whichever sender's message for the key landed
    /// last.
    pub input: Option<String>,
    /// The key.
    pub key: String,
    /// A dotted path of object names and list indexes from 0 (`volume`,
    /// `slots.2.state`); empty for the message itself.
    pub field: String,
    /// Read a `#rrggbb` text as a [`DataReading::Color`] rather than a
    /// [`DataReading::Text`].
    pub color: bool,
}

/// The value at `field` in `value`: each dotted part an object's name, or a list's index
/// from 0. `None` when a part is missing.
#[must_use]
pub fn field<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    if field.is_empty() {
        return Some(value);
    }
    field.split('.').try_fold(value, |at, part| match at {
        Value::Object(map) => map.get(part),
        Value::Array(list) => part.parse::<usize>().ok().and_then(|i| list.get(i)),
        _ => None,
    })
}

/// What `value` reads as for a watch, colour or not.
#[must_use]
pub fn reading(value: Option<&Value>, color: bool) -> DataReading {
    match value {
        Some(Value::Bool(b)) => DataReading::Number(if *b { 1.0 } else { 0.0 }),
        Some(Value::Number(n)) => n.as_f64().map_or(DataReading::Absent, DataReading::Number),
        Some(Value::String(s)) if color => {
            parse_color(s).map_or(DataReading::Absent, DataReading::Color)
        }
        Some(Value::String(s)) => DataReading::Text(text_hash(s)),
        _ => DataReading::Absent,
    }
}

struct Entry {
    value: Arc<Value>,
    bytes: usize,
    seen_ms: u64,
}

#[derive(Default)]
struct Input {
    keys: BTreeMap<String, Entry>,
    bytes: usize,
}

struct Watched {
    watch: DataWatch,
    cell: Arc<DataCell>,
    /// For a watch on any sender: the sender whose message set it last.
    from: Option<String>,
}

#[derive(Default)]
struct Store {
    inputs: BTreeMap<String, Input>,
    watches: Vec<Watched>,
}

impl Store {
    /// Sets each watch on `key` that `input`'s message (or its absence) now decides.
    fn update(&mut self, input: &str, key: &str) {
        let Self { inputs, watches } = self;
        for w in watches.iter_mut().filter(|w| w.watch.key == key) {
            match &w.watch.input {
                Some(name) if name != input => continue,
                Some(_) => {
                    let value = inputs.get(input).and_then(|i| i.keys.get(key));
                    w.cell.set(read(value, &w.watch));
                }
                None => {
                    // Any sender's: the one whose message just landed, or, if this
                    // sender's went, another's that has the key.
                    let here = inputs.get(input).and_then(|i| i.keys.get(key));
                    if here.is_some() {
                        w.from = Some(input.to_owned());
                        w.cell.set(read(here, &w.watch));
                    } else if w.from.as_deref() == Some(input) {
                        let other = inputs
                            .iter()
                            .find_map(|(name, i)| i.keys.get(key).map(|e| (name, e)));
                        w.from = other.map(|(name, _)| name.clone());
                        w.cell.set(read(other.map(|(_, e)| e), &w.watch));
                    }
                }
            }
        }
    }

    fn first_reading(&self, watch: &DataWatch) -> (DataReading, Option<String>) {
        let found = match &watch.input {
            Some(name) => self
                .inputs
                .get(name)
                .and_then(|i| i.keys.get(&watch.key))
                .map(|e| (name, e)),
            None => self
                .inputs
                .iter()
                .find_map(|(name, i)| i.keys.get(&watch.key).map(|e| (name, e))),
        };
        (
            read(found.map(|(_, e)| e), watch),
            found.map(|(name, _)| name.clone()),
        )
    }
}

fn read(entry: Option<&Entry>, watch: &DataWatch) -> DataReading {
    reading(
        entry.and_then(|e| field(&e.value, &watch.field)),
        watch.color,
    )
}

/// How deep `value` nests: 0 for a scalar, 1 for a flat object or list.
fn depth(value: &Value) -> usize {
    match value {
        Value::Array(list) => 1 + list.iter().map(depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// The live data from every sender with state, and the watches on it. Cloning it
/// shares it.
#[derive(Clone, Default)]
pub struct LiveData {
    store: Arc<Mutex<Store>>,
    /// Counts every change to any sender's messages, so a reader that draws from many
    /// keys (a `grid` source) can tell nothing changed with one atomic load.
    changes: Arc<AtomicU64>,
    /// The channels' values (ADR-0183), carried beside the data so every reader that
    /// planning hands live data to can find a channel's cell too.
    channels: crate::ChannelLevels,
}

impl std::fmt::Debug for LiveData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let store = self.lock();
        f.debug_struct("LiveData")
            .field("inputs", &store.inputs.keys().collect::<Vec<_>>())
            .field("watches", &store.watches.len())
            .finish()
    }
}

impl LiveData {
    /// Nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the sender `input`'s whole message for `key`, JSON bytes, received at
    /// `now_ms`, in place of the one before, and updates the watches on the key.
    ///
    /// # Errors
    /// What is [`Refused`]; the key keeps the message it had.
    pub fn put(&self, input: &str, key: &str, json: &[u8], now_ms: u64) -> Result<(), Refused> {
        if json.len() > MAX_MESSAGE_BYTES {
            return Err(Refused::NotJson);
        }
        // serde_json's own recursion limit (128) bounds the parse; ours is lower.
        let value: Value = serde_json::from_slice(json).map_err(|_| Refused::NotJson)?;
        if depth(&value) > MAX_DEPTH {
            return Err(Refused::TooDeep);
        }
        let mut store = self.lock();
        if !store.inputs.contains_key(input) && store.inputs.len() >= MAX_INPUTS {
            return Err(Refused::TooManyKeys);
        }
        let sender = store.inputs.entry(input.to_owned()).or_default();
        let before = sender.keys.get(key).map(|e| e.bytes);
        if before.is_none() && sender.keys.len() >= MAX_KEYS {
            return Err(Refused::TooManyKeys);
        }
        let bytes = sender.bytes - before.unwrap_or(0) + json.len();
        if bytes > MAX_SENDER_BYTES {
            return Err(Refused::TooLarge);
        }
        sender.bytes = bytes;
        sender.keys.insert(
            key.to_owned(),
            Entry {
                value: Arc::new(value),
                bytes: json.len(),
                seen_ms: now_ms,
            },
        );
        store.update(input, key);
        self.changed();
        Ok(())
    }

    /// Notes that the sender `input` resent its message for `key` at `now_ms`, so the
    /// key stays current. Returns whether the key is here: `false` once it expired or
    /// was forgotten, when the caller should put the resend in again.
    pub fn touch(&self, input: &str, key: &str, now_ms: u64) -> bool {
        match self
            .lock()
            .inputs
            .get_mut(input)
            .and_then(|i| i.keys.get_mut(key))
        {
            Some(entry) => {
                entry.seen_ms = entry.seen_ms.max(now_ms);
                true
            }
            None => false,
        }
    }

    /// Forgets everything the sender `input` sent: it started again, or stopped being
    /// received.
    pub fn restart(&self, input: &str) {
        let mut store = self.lock();
        let Some(gone) = store.inputs.remove(input) else {
            return;
        };
        for key in gone.keys.keys() {
            store.update(input, key);
        }
        self.changed();
    }

    /// Forgets each key not put or touched in the [`STALE_MS`] before `now_ms`.
    pub fn expire(&self, now_ms: u64) {
        let mut store = self.lock();
        let mut gone = Vec::new();
        for (name, input) in &mut store.inputs {
            let stale: Vec<String> = input
                .keys
                .iter()
                .filter(|(_, e)| now_ms.saturating_sub(e.seen_ms) >= STALE_MS)
                .map(|(k, _)| k.clone())
                .collect();
            for key in stale {
                if let Some(e) = input.keys.remove(&key) {
                    input.bytes -= e.bytes;
                }
                gone.push((name.clone(), key));
            }
        }
        store.inputs.retain(|_, input| !input.keys.is_empty());
        let any = !gone.is_empty();
        for (name, key) in gone {
            store.update(&name, &key);
        }
        if any {
            self.changed();
        }
    }

    fn changed(&self) {
        self.changes.fetch_add(1, Ordering::Release);
    }

    /// How many times any sender's messages changed: a message put, a key expired, a
    /// sender forgotten. Equal readings mean nothing changed in between.
    #[must_use]
    pub fn changes(&self) -> u64 {
        self.changes.load(Ordering::Acquire)
    }

    /// The message for `key` from the sender `input`, or with `None` from whichever
    /// sender has one (the first by name), as a watch on any sender reads it once no
    /// message has landed since.
    #[must_use]
    pub fn get_from(&self, input: Option<&str>, key: &str) -> Option<Arc<Value>> {
        let store = self.lock();
        match input {
            Some(name) => store.inputs.get(name).and_then(|i| i.keys.get(key)),
            None => store.inputs.values().find_map(|i| i.keys.get(key)),
        }
        .map(|e| Arc::clone(&e.value))
    }

    /// Every sender with state, by name, and its keys, in order.
    #[must_use]
    pub fn list(&self) -> Vec<(String, Vec<String>)> {
        self.lock()
            .inputs
            .iter()
            .map(|(name, input)| (name.clone(), input.keys.keys().cloned().collect()))
            .collect()
    }

    /// Every key's message from the sender `input`, or with `None` from whichever sender
    /// has each key (the first by name), in key order.
    #[must_use]
    pub fn entries(&self, input: Option<&str>) -> Vec<(String, Arc<Value>)> {
        let store = self.lock();
        match input {
            Some(name) => store
                .inputs
                .get(name)
                .map(|i| {
                    i.keys
                        .iter()
                        .map(|(k, e)| (k.clone(), Arc::clone(&e.value)))
                        .collect()
                })
                .unwrap_or_default(),
            None => {
                let mut all: BTreeMap<String, Arc<Value>> = BTreeMap::new();
                for i in store.inputs.values() {
                    for (k, e) in &i.keys {
                        all.entry(k.clone()).or_insert_with(|| Arc::clone(&e.value));
                    }
                }
                all.into_iter().collect()
            }
        }
    }

    /// The sender `input`'s message for `key`, if it has one.
    #[must_use]
    pub fn get(&self, input: &str, key: &str) -> Option<Arc<Value>> {
        self.lock()
            .inputs
            .get(input)
            .and_then(|i| i.keys.get(key))
            .map(|e| Arc::clone(&e.value))
    }

    /// A cell that reads `watch` from now on, set to what it reads now. The same watch
    /// asked for twice shares one cell. Past [`MAX_WATCHES`], or for a field longer than
    /// [`MAX_FIELD`], the cell reads absent for ever.
    pub fn watch(&self, watch: &DataWatch) -> Arc<DataCell> {
        let mut store = self.lock();
        // Forget the watches nobody holds any more.
        store.watches.retain(|w| Arc::strong_count(&w.cell) > 1);
        if let Some(w) = store.watches.iter().find(|w| w.watch == *watch) {
            return Arc::clone(&w.cell);
        }
        let cell = Arc::new(DataCell::default());
        if store.watches.len() >= MAX_WATCHES || watch.field.len() > MAX_FIELD {
            return cell;
        }
        let (now, from) = store.first_reading(watch);
        cell.set(now);
        store.watches.push(Watched {
            watch: watch.clone(),
            cell: Arc::clone(&cell),
            from,
        });
        cell
    }

    /// The channels' cells (ADR-0183), which the engine's channel ticker stores.
    #[must_use]
    pub fn channels(&self) -> &crate::ChannelLevels {
        &self.channels
    }

    /// How many watches someone still holds.
    #[must_use]
    pub fn watches(&self) -> usize {
        self.lock()
            .watches
            .iter()
            .filter(|w| Arc::strong_count(&w.cell) > 1)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn watch(input: Option<&str>, key: &str, field: &str) -> DataWatch {
        DataWatch {
            input: input.map(str::to_owned),
            key: key.to_owned(),
            field: field.to_owned(),
            color: false,
        }
    }

    #[test]
    fn a_cell_holds_each_kind_of_reading() {
        for r in [
            DataReading::Absent,
            DataReading::Number(0.0),
            DataReading::Number(-0.0),
            DataReading::Number(-1.5e300),
            DataReading::Number(f64::MAX),
            DataReading::Text(text_hash("playing")),
            DataReading::Color([1, 2, 3, 255]),
            DataReading::Color([0, 0, 0, 0]),
        ] {
            assert_eq!(unpack(pack(r)), r);
        }
        assert_eq!(
            unpack(pack(DataReading::Number(f64::NAN))),
            DataReading::Absent
        );
        assert_eq!(text_hash("a"), text_hash("a"));
        assert_ne!(text_hash("playing"), text_hash("stopped"));
        assert!(text_hash("anything") <= PAYLOAD);
    }

    #[test]
    fn a_field_is_a_dotted_path_of_names_and_indexes() {
        let v = json!({"volume": 0.5, "slots": [{"state": "playing"}], "tracks": [[1, 2]], "3": 7});
        assert_eq!(field(&v, "volume"), Some(&json!(0.5)));
        assert_eq!(field(&v, "slots.0.state"), Some(&json!("playing")));
        assert_eq!(field(&v, "tracks.0.1"), Some(&json!(2)));
        assert_eq!(field(&v, "3"), Some(&json!(7)));
        assert_eq!(field(&v, ""), Some(&v));
        assert_eq!(field(&v, "slots.1.state"), None);
        assert_eq!(field(&v, "slots.x"), None);
        assert_eq!(field(&v, "volume.more"), None);
        assert_eq!(reading(Some(&json!(true)), false), DataReading::Number(1.0));
        assert_eq!(
            reading(Some(&json!(false)), false),
            DataReading::Number(0.0)
        );
        assert_eq!(reading(Some(&json!(null)), false), DataReading::Absent);
        assert_eq!(reading(Some(&json!([1])), false), DataReading::Absent);
        assert_eq!(
            reading(Some(&json!("#ff8000")), true),
            DataReading::Color([255, 128, 0, 255])
        );
        assert_eq!(reading(Some(&json!("orange")), true), DataReading::Absent);
        assert_eq!(
            reading(Some(&json!("#ff8000")), false),
            DataReading::Text(text_hash("#ff8000"))
        );
    }

    #[test]
    fn watches_follow_their_key_from_one_sender_or_any() {
        let data = LiveData::new();
        let mine = data.watch(&watch(Some("A"), "track/0", "volume"));
        let any = data.watch(&watch(None, "track/0", "volume"));
        assert_eq!(mine.read(), DataReading::Absent);
        data.put("B", "track/0", br#"{"volume": 0.25}"#, 0).unwrap();
        assert_eq!(mine.read(), DataReading::Absent, "another sender's");
        assert_eq!(any.read(), DataReading::Number(0.25));
        data.put("A", "track/0", br#"{"volume": 0.75}"#, 0).unwrap();
        assert_eq!(mine.read(), DataReading::Number(0.75));
        assert_eq!(any.read(), DataReading::Number(0.75), "the newest message");
        // A watch asked for later starts at what is there now, and shares a cell.
        let late = data.watch(&watch(Some("A"), "track/0", "volume"));
        assert!(Arc::ptr_eq(&late, &mine));
        // A sends a message without the field: absent, and any-sender follows it.
        data.put("A", "track/0", br#"{"mute": true}"#, 0).unwrap();
        assert_eq!(mine.read(), DataReading::Absent);
        assert_eq!(any.read(), DataReading::Absent);
        // A starts again: any-sender falls back on B's.
        data.restart("A");
        assert_eq!(any.read(), DataReading::Number(0.25));
        assert_eq!(
            data.list(),
            vec![("B".to_owned(), vec!["track/0".to_owned()])]
        );
    }

    #[test]
    fn a_key_not_kept_current_goes_after_three_seconds() {
        let data = LiveData::new();
        let cell = data.watch(&watch(Some("A"), "clock", "playing"));
        data.put("A", "clock", br#"{"playing": true}"#, 1_000)
            .unwrap();
        data.put("A", "transport", br#"{"playing": true}"#, 1_000)
            .unwrap();
        assert_eq!(cell.read(), DataReading::Number(1.0));
        // A resend keeps clock current; transport isn't resent.
        assert!(data.touch("A", "clock", 3_500));
        data.expire(4_000);
        assert_eq!(data.list()[0].1, vec!["clock".to_owned()]);
        assert_eq!(cell.read(), DataReading::Number(1.0));
        data.expire(6_500);
        assert_eq!(cell.read(), DataReading::Absent);
        assert!(data.list().is_empty());
        assert!(data.get("A", "clock").is_none());
        assert!(
            !data.touch("A", "clock", 7_000),
            "gone: the caller puts it again"
        );
    }

    #[test]
    fn what_is_too_much_or_not_json_is_refused_and_changes_nothing() {
        let data = LiveData::new();
        data.put("A", "k", b"1", 0).unwrap();
        assert_eq!(data.put("A", "k", b"{", 0), Err(Refused::NotJson));
        assert_eq!(data.put("A", "k", b"1 2", 0), Err(Refused::NotJson));
        let deep = format!("{}{}", "[".repeat(33), "]".repeat(33));
        assert_eq!(
            data.put("A", "k", deep.as_bytes(), 0),
            Err(Refused::TooDeep)
        );
        let ok = format!("{}{}", "[".repeat(32), "]".repeat(32));
        assert_eq!(data.put("A", "j", ok.as_bytes(), 0), Ok(()));
        let long = vec![b' '; MAX_MESSAGE_BYTES + 1];
        assert_eq!(data.put("A", "k", &long, 0), Err(Refused::NotJson));
        assert_eq!(data.get("A", "k").as_deref(), Some(&json!(1)));
        for n in 2..MAX_KEYS {
            data.put("A", &format!("n{n}"), b"0", 0).unwrap();
        }
        assert_eq!(
            data.put("A", "one-more", b"0", 0),
            Err(Refused::TooManyKeys)
        );
        assert_eq!(
            data.put("A", "k", b"2", 0),
            Ok(()),
            "an existing key still changes"
        );
        // 4 MiB in all from one sender.
        let data = LiveData::new();
        let big = format!("\"{}\"", "x".repeat(MAX_MESSAGE_BYTES - 2));
        let fits = MAX_SENDER_BYTES / MAX_MESSAGE_BYTES;
        for n in 0..fits {
            data.put("A", &format!("b{n}"), big.as_bytes(), 0).unwrap();
        }
        assert_eq!(
            data.put("A", "more", big.as_bytes(), 0),
            Err(Refused::TooLarge)
        );
        assert_eq!(
            data.put("B", "more", big.as_bytes(), 0),
            Ok(()),
            "per sender"
        );
    }

    #[test]
    fn a_watch_nobody_holds_is_forgotten() {
        let data = LiveData::new();
        let kept = data.watch(&watch(None, "a", ""));
        drop(data.watch(&watch(None, "b", "")));
        assert_eq!(data.watches(), 1);
        let _again = data.watch(&watch(None, "c", ""));
        assert_eq!(data.lock().watches.len(), 2, "b's went on the next watch");
        drop(kept);
    }
}
