//! The UI's copy of the engine's state, shared by every panel: the collection as
//! `state.get` last reported it, the program scene, what is selected, and the kind
//! descriptors read so far. The app keeps it current from the session's events; panels
//! read it reactively and change the engine only through calls, never by editing it.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::session::Session;
use jstream_control::ControlError;
use jstream_control::protocol::{
    Batch, BusInfo, Call, CallResult, Canvas, DeleteTarget, Edit, Event, ItemId, ItemInfo,
    KindDescriptor, KindSummary, OutputInfo, Query, QueryResult, Rev, SceneId, SceneInfo,
    SceneRole, SourceId, SourceInfo, SourceKind,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashMap;

/// The collection state at one revision, as `state.get` reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Collection {
    /// The revision it was read at.
    pub rev: Rev,
    /// The canvas every scene is laid out on.
    pub canvas: Canvas,
    /// Every source, ordered by ID.
    pub sources: Vec<SourceInfo>,
    /// Every scene with its items, in display order.
    pub scenes: Vec<SceneInfo>,
    /// Every output, ordered by ID.
    pub outputs: Vec<OutputInfo>,
    /// Every audio bus, ordered by ID, the master among them: what a `bus` parameter
    /// picks from.
    pub buses: Vec<BusInfo>,
}

impl Collection {
    /// The collection a `state.get` result carries, or `None` for any other result.
    pub fn from_result(result: QueryResult) -> Option<Collection> {
        match result {
            QueryResult::StateGet {
                rev,
                canvas,
                sources,
                scenes,
                outputs,
                buses,
                ..
            } => Some(Collection {
                rev,
                canvas,
                sources,
                scenes,
                outputs,
                buses,
            }),
            _ => None,
        }
    }

    /// The scene with this ID.
    pub fn scene(&self, scene_id: &SceneId) -> Option<&SceneInfo> {
        self.scenes.iter().find(|s| &s.scene_id == scene_id)
    }

    /// The source with this ID.
    pub fn source(&self, source_id: &SourceId) -> Option<&SourceInfo> {
        self.sources.iter().find(|s| &s.source_id == source_id)
    }

    /// The item with this ID, with the scene it is in.
    pub fn item(&self, item_id: &ItemId) -> Option<(&SceneInfo, &ItemInfo)> {
        self.scenes.iter().find_map(|scene| {
            scene
                .items
                .iter()
                .find(|i| &i.item_id == item_id)
                .map(|item| (scene, item))
        })
    }

    /// The scenes the scene list shows, in display order: groups are left out
    /// (ADR-0024), since they are shown under the items that place them.
    pub fn listed_scenes(&self) -> impl Iterator<Item = &SceneInfo> {
        self.scenes.iter().filter(|s| s.role == SceneRole::Scene)
    }

    /// A source's display name, falling back to its ID.
    pub fn source_name(&self, source_id: &SourceId) -> String {
        self.source(source_id)
            .map_or_else(|| source_id.to_string(), |s| s.name.clone())
    }

    /// What a source shows, for a label: its kind's ID, or `scene: <name>` for a nested
    /// scene.
    pub fn kind_label(&self, kind: &SourceKind) -> String {
        match kind {
            SourceKind::Builtin { kind } => kind.clone(),
            SourceKind::Scene { scene_id } => {
                let name = self
                    .scene(scene_id)
                    .map_or_else(|| scene_id.to_string(), |s| s.name.clone());
                format!("scene: {name}")
            }
            _ => "unknown".into(),
        }
    }

    /// Whether any scene, source, item or output uses this ID. The engine rejects a
    /// creating edit whose ID exists, so new IDs are checked against all of them.
    pub fn id_taken(&self, id: &str) -> bool {
        self.scenes
            .iter()
            .any(|s| s.scene_id.as_str() == id || s.items.iter().any(|i| i.item_id.as_str() == id))
            || self.sources.iter().any(|s| s.source_id.as_str() == id)
            || self.outputs.iter().any(|o| o.output_id.as_str() == id)
    }
}

/// What is selected: a scene being edited, and items in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// The scene the sources list and the canvas show.
    pub scene: Option<SceneId>,
    /// The primary item: the one the properties form edits. Always an item of `scene`.
    pub item: Option<ItemId>,
    /// The other selected items, which the canvas moves, scales and rotates with the
    /// primary one. Items of `scene`, without `item`, and empty when `item` is `None`.
    pub also: Vec<ItemId>,
}

impl Selection {
    /// Whether `item_id` is selected, as the primary item or another.
    pub fn contains(&self, item_id: &ItemId) -> bool {
        self.item.as_ref() == Some(item_id) || self.also.contains(item_id)
    }

    /// Every selected item, the primary one first.
    pub fn items(&self) -> impl Iterator<Item = &ItemId> {
        self.item.iter().chain(&self.also)
    }

    /// The selection with `items` selected in the same scene: the first is the primary
    /// one. Duplicates are dropped.
    pub fn with_items(&self, items: impl IntoIterator<Item = ItemId>) -> Selection {
        let mut all: Vec<ItemId> = Vec::new();
        for id in items {
            if !all.contains(&id) {
                all.push(id);
            }
        }
        let mut all = all.into_iter();
        Selection {
            scene: self.scene.clone(),
            item: all.next(),
            also: all.collect(),
        }
    }

    /// The selection with `item_id` added, as the new primary item, or removed if it
    /// was selected; removing the primary item makes the next one primary.
    pub fn toggled(&self, item_id: &ItemId) -> Selection {
        if self.contains(item_id) {
            self.with_items(self.items().filter(|id| *id != item_id).cloned())
        } else {
            self.with_items(std::iter::once(item_id.clone()).chain(self.items().cloned()))
        }
    }

    /// The selection after the state or the program scene changed: the scene is kept
    /// while it exists, else falls back to the program scene, else to the first listed
    /// scene; the item is kept while it is still in that scene.
    pub fn reconcile(&self, collection: &Collection, program: Option<&SceneId>) -> Selection {
        let exists = |id: &SceneId| collection.scene(id).is_some();
        let scene = self
            .scene
            .clone()
            .filter(exists)
            .or_else(|| program.cloned().filter(exists))
            .or_else(|| {
                collection
                    .listed_scenes()
                    .next()
                    .map(|s| s.scene_id.clone())
            });
        let in_scene = |item_id: &&ItemId| {
            let found = collection.item(item_id).map(|(s, _)| &s.scene_id);
            found.is_some() && found == scene.as_ref()
        };
        let kept: Vec<ItemId> = self.items().filter(in_scene).cloned().collect();
        Selection {
            scene,
            ..Selection::default()
        }
        .with_items(kept)
    }
}

/// A short account of what a `delete.plan` does beyond deleting its target, for the
/// confirmation: `None` when it deletes the target alone.
pub fn plan_summary(edits: &[Edit]) -> Option<String> {
    let (mut items, mut sources, mut references) = (0_usize, 0_usize, 0_usize);
    // The last edit is the delete of the target itself.
    for edit in edits.iter().take(edits.len().saturating_sub(1)) {
        match edit {
            Edit::ItemRemove { .. } => items += 1,
            Edit::SourceDelete { .. } | Edit::SceneDelete { .. } => sources += 1,
            Edit::SourceSetSettings { patch, .. } => references += patch.0.len(),
            _ => {}
        }
    }
    let count = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    let parts: Vec<String> = [
        count(items, "item", "items").map(|p| format!("removes {p}")),
        count(sources, "nested-scene source", "nested-scene sources")
            .map(|p| format!("deletes {p}")),
        count(
            references,
            "setting that refers to it",
            "settings that refer to it",
        )
        .map(|p| format!("clears {p}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| format!("This also {}.", parts.join(", ")))
}

/// A new ID for a scene, source or item: `<prefix>-<client>-<n>`, with the smallest `n`
/// from `next` on that `taken` refuses. The client ID keeps two UIs on one engine from
/// choosing the same ID.
pub fn fresh_id(
    prefix: &str,
    client: &str,
    next: &mut u64,
    taken: impl Fn(&str) -> bool,
) -> String {
    loop {
        *next += 1;
        let id = format!("{prefix}-{client}-{next}");
        if !taken(&id) {
            return id;
        }
    }
}

/// The shared state, provided as Leptos context by the app. Copyable: every field is a
/// handle.
#[derive(Clone, Copy)]
pub struct Studio {
    /// The collection, once the first `state.get` answered.
    pub collection: RwSignal<Option<Collection>>,
    /// The scene on program.
    pub program: RwSignal<Option<SceneId>>,
    /// What is selected.
    pub selection: RwSignal<Selection>,
    /// The source kinds `kind.list` reported, once it answered.
    pub kinds: RwSignal<Option<Vec<KindSummary>>>,
    /// The kind descriptors read so far, by kind ID.
    pub descriptors: RwSignal<HashMap<String, KindDescriptor>>,
    /// The last call that failed, for the problem bar.
    pub problem: RwSignal<Option<String>>,
    /// Bumped when an output's stream key was set or cleared, which no event reports, so
    /// the header re-reads the outputs Go Live may start.
    pub keys_changed: RwSignal<u64>,
    /// Whether a `state.get` is in flight, and whether another is due after it.
    refresh: StoredValue<(bool, bool)>,
    /// The counter [`fresh_id`] continues from.
    next_id: StoredValue<u64>,
}

impl Studio {
    /// Empty state, before a session opens.
    pub fn new() -> Studio {
        Studio {
            collection: RwSignal::new(None),
            program: RwSignal::new(None),
            selection: RwSignal::new(Selection::default()),
            kinds: RwSignal::new(None),
            descriptors: RwSignal::new(HashMap::new()),
            problem: RwSignal::new(None),
            keys_changed: RwSignal::new(0),
            refresh: StoredValue::new((false, false)),
            next_id: StoredValue::new(0),
        }
    }

    /// Forgets everything: a new session starts from its own `state.get`.
    pub fn reset(&self) {
        self.collection.set(None);
        self.program.set(None);
        self.selection.set(Selection::default());
        self.kinds.set(None);
        self.descriptors.set(HashMap::new());
        self.problem.set(None);
        self.refresh.set_value((false, false));
    }

    /// Reads everything a new session needs: the state, the program scene and the kinds.
    pub fn load(&self, session: &Session) {
        let studio = *self;
        let first = session.clone();
        spawn_local(async move {
            if let Ok(QueryResult::StudioGet { program_scene, .. }) =
                query(&first, Query::StudioGet).await
            {
                studio.program.set(program_scene);
            }
            // The state is read after the program scene, so the first selection is the
            // scene on program.
            studio.refresh(&first);
        });
        let kinds = session.clone();
        spawn_local(async move {
            studio.take_kinds(list_kinds(&kinds).await);
        });
    }

    /// Keeps the kinds `kind.list` answered. A failed `kind.list` shows its error in the
    /// problem bar and leaves "New source" empty, as an engine that lists no kinds does;
    /// existing sources and scenes can still be placed.
    fn take_kinds(&self, listed: Result<Vec<KindSummary>, ControlError>) {
        let kinds = listed.unwrap_or_else(|error| {
            self.report(&error);
            Vec::new()
        });
        self.kinds.set(Some(kinds));
    }

    /// Follows one engine event.
    pub fn on_event(&self, session: &Session, event: &Event) {
        match event {
            Event::StateChanged { rev, .. } => {
                let current = self
                    .collection
                    .with_untracked(|c| c.as_ref().map(|c| c.rev));
                if current.is_none_or(|current| current < *rev) {
                    self.refresh(session);
                }
            }
            Event::EventsDropped { .. } => self.refresh(session),
            Event::StudioChanged { program_scene, .. } => {
                self.program.set(program_scene.clone());
                self.reconcile();
            }
            _ => {}
        }
    }

    /// Re-reads the state with `state.get`. While one read is in flight, further
    /// requests coalesce into one more read after it, so a burst of `state.changed`
    /// costs two reads, not one per event.
    pub fn refresh(&self, session: &Session) {
        let (busy, _) = self.refresh.get_value();
        if busy {
            self.refresh.set_value((true, true));
            return;
        }
        self.refresh.set_value((true, false));
        let studio = *self;
        let session = session.clone();
        spawn_local(async move {
            loop {
                match query(&session, Query::StateGet).await {
                    Ok(result) => {
                        if let Some(collection) = Collection::from_result(result) {
                            studio.collection.set(Some(collection));
                            studio.reconcile();
                        }
                    }
                    Err(error) => studio.report(&error),
                }
                let (_, again) = studio.refresh.get_value();
                studio.refresh.set_value((again, false));
                if !again {
                    break;
                }
            }
        });
    }

    /// Fixes the selection up after the state or the program scene changed.
    fn reconcile(&self) {
        let next = self.collection.with_untracked(|collection| {
            let program = self.program.get_untracked();
            collection.as_ref().map(|c| {
                self.selection
                    .with_untracked(|s| s.reconcile(c, program.as_ref()))
            })
        });
        if let Some(next) = next
            && self.selection.with_untracked(|s| *s != next)
        {
            self.selection.set(next);
        }
    }

    /// Selects a scene, and no item.
    pub fn select_scene(&self, scene_id: SceneId) {
        self.selection.set(Selection {
            scene: Some(scene_id),
            ..Selection::default()
        });
    }

    /// Selects one item of the selected scene alone, or none.
    pub fn select_item(&self, item_id: Option<ItemId>) {
        self.select_items(item_id);
    }

    /// Selects these items of the selected scene, the first as the primary one.
    pub fn select_items(&self, items: impl IntoIterator<Item = ItemId>) {
        let next = self.selection.with_untracked(|s| s.with_items(items));
        if self.selection.with_untracked(|s| *s != next) {
            self.selection.set(next);
        }
    }

    /// Adds an item to the selection, or removes it if it was selected.
    pub fn toggle_item(&self, item_id: &ItemId) {
        let next = self.selection.with_untracked(|s| s.toggled(item_id));
        self.selection.set(next);
    }

    /// A new ID with this prefix, unused in the current collection.
    pub fn fresh_id(&self, session: &Session, prefix: &str) -> String {
        let client = session.welcome.client_id.to_string();
        let mut next = self.next_id.get_value();
        let id = self.collection.with_untracked(|c| {
            fresh_id(prefix, &client, &mut next, |id| {
                c.as_ref().is_some_and(|c| c.id_taken(id))
            })
        });
        self.next_id.set_value(next);
        id
    }

    /// Shows a failed call in the problem bar.
    pub fn report(&self, error: &ControlError) {
        self.problem.set(Some(error.to_string()));
    }

    /// Sends `edits` as one batch, reporting a failure in the problem bar. The state
    /// follows from the `state.changed` the batch causes.
    pub fn apply(&self, session: &Session, edits: Vec<Edit>) {
        let studio = *self;
        let session = session.clone();
        spawn_local(async move {
            if let Err(error) = apply(&session, edits, None).await {
                studio.report(&error);
            }
        });
    }

    /// Runs an action, reporting a failure in the problem bar.
    pub fn act(&self, session: &Session, action: jstream_control::protocol::Action) {
        let studio = *self;
        let session = session.clone();
        spawn_local(async move {
            if let Err(error) = session.control.call(Call::Action { action }).await {
                studio.report(&error);
            }
        });
    }

    /// The descriptor of `kind_id`, reading it with `kind.describe` the first time.
    pub async fn descriptor(
        &self,
        session: &Session,
        kind_id: &str,
    ) -> Result<KindDescriptor, ControlError> {
        if let Some(known) = self.descriptors.with_untracked(|d| d.get(kind_id).cloned()) {
            return Ok(known);
        }
        let query_kind = Query::KindDescribe {
            kind_id: kind_id.to_owned(),
        };
        match query(session, query_kind).await? {
            QueryResult::KindDescribe { kind, .. } => {
                self.descriptors
                    .update(|d| _ = d.insert(kind_id.to_owned(), kind.clone()));
                Ok(kind)
            }
            other => Err(unexpected(other.name())),
        }
    }
}

impl Default for Studio {
    fn default() -> Self {
        Studio::new()
    }
}

/// Runs a query and returns its result.
pub async fn query(session: &Session, query: Query) -> Result<QueryResult, ControlError> {
    match session.control.call(Call::Query { query }).await?.result {
        CallResult::Query { result } => Ok(result),
        _ => Err(unexpected("a query")),
    }
}

/// Sends `edits` as one batch, applied only at `expected_rev` when one is given.
pub async fn apply(
    session: &Session,
    edits: Vec<Edit>,
    expected_rev: Option<Rev>,
) -> Result<Rev, ControlError> {
    let mut batch = Batch {
        edits,
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    };
    if let Some(rev) = expected_rev {
        batch = batch.expecting(rev);
    }
    match session.control.call(Call::Batch(batch)).await?.result {
        CallResult::Batch { rev, .. } => Ok(rev),
        _ => Err(unexpected("a batch")),
    }
}

/// Reads the source kinds the engine offers, with `kind.list`.
pub async fn list_kinds(session: &Session) -> Result<Vec<KindSummary>, ControlError> {
    match query(session, Query::KindList).await? {
        QueryResult::KindList { kinds } => Ok(kinds),
        other => Err(unexpected(other.name())),
    }
}

/// Reads the `delete.plan` for `target`: the revision it was made at and its edits.
pub async fn plan_delete(
    session: &Session,
    target: DeleteTarget,
) -> Result<(Rev, Vec<Edit>), ControlError> {
    match query(session, Query::DeletePlan { target }).await? {
        QueryResult::DeletePlan { rev, edits, .. } => Ok((rev, edits)),
        other => Err(unexpected(other.name())),
    }
}

/// An answer of the wrong kind, which only a broken engine sends.
fn unexpected(what: &str) -> ControlError {
    ControlError::Disconnected {
        reason: format!("the engine answered {what} with the wrong kind of result"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Collection, Selection, Studio, fresh_id, list_kinds, plan_summary};
    use crate::session::Session;
    use jstream_control::protocol::{
        Batch, Call, CallResult, Edit, Hello, ItemId, Query, QueryResult, Response, SceneId,
        SceneRole, Settings, SourceId, SourceKind, Welcome,
    };
    use jstream_control::{BoxFuture, Control, ControlError, EventStream, MockEngine};
    use leptos::prelude::*;
    use serde_json::json;
    use std::pin::pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    /// A mock engine's collection after `edits`: two scenes and a group, a colour
    /// source placed twice, and a nested-scene source.
    fn collection() -> Collection {
        let engine = MockEngine::new();
        let session = now(Session::open(Rc::new(engine.connect())))
            .unwrap_or_else(|e| panic!("hello failed: {e}"));
        let edits = vec![
            Edit::SceneCreate {
                scene_id: "a".into(),
                name: "Main".into(),
                role: None,
            },
            Edit::SceneCreate {
                scene_id: "g".into(),
                name: "Group".into(),
                role: None,
            },
            Edit::SceneSetRole {
                scene_id: "g".into(),
                role: SceneRole::Group,
            },
            Edit::SceneCreate {
                scene_id: "b".into(),
                name: "Break".into(),
                role: None,
            },
            Edit::SourceCreate {
                source_id: "red".into(),
                name: "Red".into(),
                kind: SourceKind::Builtin {
                    kind: "color".into(),
                },
                settings: Settings::new(),
                duplicated_from: None,
            },
            Edit::SourceCreate {
                source_id: "nest".into(),
                name: "Nested".into(),
                kind: SourceKind::Scene {
                    scene_id: "b".into(),
                },
                settings: Settings::new(),
                duplicated_from: None,
            },
            Edit::ItemAdd {
                scene_id: "a".into(),
                item_id: "i1".into(),
                source_id: "red".into(),
                transform: None,
                geometry: None,
                opacity: None,
                blend: None,
            },
            Edit::ItemAdd {
                scene_id: "a".into(),
                item_id: "i2".into(),
                source_id: "nest".into(),
                transform: None,
                geometry: None,
                opacity: None,
                blend: None,
            },
            Edit::ItemAdd {
                scene_id: "b".into(),
                item_id: "i3".into(),
                source_id: "red".into(),
                transform: None,
                geometry: None,
                opacity: None,
                blend: None,
            },
        ];
        let batch = session.control.call(Call::Batch(Batch {
            edits,
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        }));
        now(batch).unwrap_or_else(|e| panic!("batch failed: {e}"));
        let state = now(session.control.call(Call::Query {
            query: Query::StateGet,
        }))
        .unwrap_or_else(|e| panic!("state.get failed: {e}"));
        let CallResult::Query { result } = state.result else {
            panic!("not a query result");
        };
        Collection::from_result(result).unwrap_or_else(|| panic!("not a state.get result"))
    }

    #[test]
    fn the_collection_reads_state_get() {
        let c = collection();
        let listed: Vec<&str> = c.listed_scenes().map(|s| s.scene_id.as_str()).collect();
        assert_eq!(listed, ["a", "b"], "groups are not listed");
        let (scene, item) = c
            .item(&ItemId::new("i3"))
            .unwrap_or_else(|| panic!("no i3"));
        assert_eq!(scene.scene_id.as_str(), "b");
        assert_eq!(c.source_name(&item.source_id), "Red");
        assert_eq!(c.source_name(&SourceId::new("gone")), "gone");
        let nest = c
            .source(&SourceId::new("nest"))
            .map(|s| c.kind_label(&s.kind));
        assert_eq!(nest.as_deref(), Some("scene: Break"));
        for id in ["a", "g", "red", "i2"] {
            assert!(c.id_taken(id), "{id}");
        }
        assert!(!c.id_taken("z"));
        let other = QueryResult::StudioGet {
            program_scene: None,
            preview_scene: None,
        };
        assert_eq!(Collection::from_result(other), None);
    }

    #[test]
    fn the_selection_follows_the_state() {
        let c = collection();
        let b = SceneId::new("b");
        let empty = Selection::default();
        assert_eq!(
            empty.reconcile(&c, Some(&b)).scene,
            Some(b.clone()),
            "nothing selected: the program scene"
        );
        assert_eq!(
            empty.reconcile(&c, None).scene,
            Some(SceneId::new("a")),
            "no program scene: the first listed scene"
        );
        let kept = Selection {
            scene: Some(SceneId::new("a")),
            item: Some(ItemId::new("i2")),
            also: vec![ItemId::new("i1")],
        };
        assert_eq!(kept.reconcile(&c, Some(&b)), kept);
        let gone = Selection {
            scene: Some(SceneId::new("deleted")),
            item: Some(ItemId::new("i2")),
            also: Vec::new(),
        };
        assert_eq!(
            gone.reconcile(&c, Some(&b)),
            Selection {
                scene: Some(b.clone()),
                ..Selection::default()
            },
            "an item outside the selected scene is dropped"
        );
        let moved = Selection {
            scene: Some(b.clone()),
            item: Some(ItemId::new("i2")),
            also: vec![ItemId::new("i3")],
        };
        assert_eq!(
            moved.reconcile(&c, None),
            Selection {
                scene: Some(b),
                item: Some(ItemId::new("i3")),
                also: Vec::new(),
            },
            "the next selected item in the scene becomes the primary one"
        );
    }

    #[test]
    fn items_are_added_and_removed() {
        let one = Selection::default().with_items([ItemId::new("i1")]);
        let two = one.toggled(&ItemId::new("i2"));
        assert_eq!(two.item, Some(ItemId::new("i2")));
        assert_eq!(two.also, vec![ItemId::new("i1")]);
        assert!(two.contains(&ItemId::new("i1")) && two.contains(&ItemId::new("i2")));
        let back = two.toggled(&ItemId::new("i2"));
        assert_eq!(back, one, "removing the primary item promotes the next");
        assert_eq!(back.toggled(&ItemId::new("i1")), Selection::default());
        let dup = Selection::default().with_items(["a", "b", "a"].map(ItemId::new));
        assert_eq!(dup.items().count(), 2);
    }

    #[test]
    fn delete_plans_are_summarised() {
        let target = Edit::SourceDelete {
            source_id: "s".into(),
        };
        assert_eq!(plan_summary(std::slice::from_ref(&target)), None);
        let mut patch = Settings::new();
        patch.insert("mask", json!(null));
        let edits = vec![
            Edit::ItemRemove {
                item_id: "i1".into(),
            },
            Edit::ItemRemove {
                item_id: "i2".into(),
            },
            Edit::SourceSetSettings {
                source_id: "t".into(),
                patch,
            },
            target,
        ];
        assert_eq!(
            plan_summary(&edits).as_deref(),
            Some("This also removes 2 items, clears 1 setting that refers to it.")
        );
    }

    /// An engine whose connection closed: every call fails.
    struct Closed;

    impl Control for Closed {
        fn hello(&self, _: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
            panic!("the session is already open")
        }

        fn call(&self, _: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
            Box::pin(std::future::ready(Err(ControlError::Disconnected {
                reason: "the engine closed the connection".into(),
            })))
        }

        fn events(&self) -> EventStream {
            Box::pin(futures_util::stream::empty())
        }
    }

    /// An engine that lists no kinds offers none, quietly; a failed `kind.list` offers
    /// none either, but says why in the problem bar (#401).
    #[test]
    fn a_failed_kind_list_is_reported() {
        let engine = MockEngine::new();
        let session = now(Session::open(Rc::new(engine.connect())))
            .unwrap_or_else(|e| panic!("hello failed: {e}"));
        let studio = Studio::new();
        studio.take_kinds(now(list_kinds(&session)));
        assert_eq!(studio.kinds.get_untracked(), Some(Vec::new()));
        assert_eq!(
            studio.problem.get_untracked(),
            None,
            "no kinds is no problem"
        );

        let studio = Studio::new();
        let closed = Session {
            control: Rc::new(Closed),
            welcome: session.welcome,
        };
        studio.take_kinds(now(list_kinds(&closed)));
        assert_eq!(studio.kinds.get_untracked(), Some(Vec::new()));
        let problem = studio.problem.get_untracked().unwrap_or_default();
        assert!(
            problem.contains("the engine closed the connection"),
            "{problem:?}"
        );
    }

    #[test]
    fn fresh_ids_skip_taken_ones() {
        let mut next = 0;
        let taken = |id: &str| id == "scene-c1-1";
        assert_eq!(fresh_id("scene", "c1", &mut next, taken), "scene-c1-2");
        assert_eq!(fresh_id("scene", "c1", &mut next, taken), "scene-c1-3");
    }
}
