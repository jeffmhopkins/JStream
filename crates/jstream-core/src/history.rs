//! Per-client undo and redo (ADR-0013, ADR-0032).
//!
//! Every batch a client applies becomes one step on that client's undo stack, holding
//! the inverse batch `Graph::apply` returned. An undo applies the inverse and moves the
//! step to the redo stack, holding the inverse of *that* application (the step again);
//! a redo moves it back. Another client's batches never touch a client's stacks.
//!
//! Conflicts: every applied batch, undo and redo is logged with what it touched (the
//! objects it created, deleted or used, and the properties it set). Before an undo or
//! redo applies, the log entries by other clients since the step was last applied or
//! undone are checked against the step's own touched set. A match, or edits that no
//! longer apply, fails with `undo_conflict` and drops the step, so another client's work
//! is never overwritten.
//!
//! History is kept in memory for a client's session and dropped when the session ends.

use jstream_protocol::{
    ClientId, Edit, HistoryDirection, ItemId, OutputId, Rev, SceneId, SourceId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The default number of steps kept per client; older steps are dropped first.
pub(crate) const DEFAULT_HISTORY_LIMIT: usize = 100;

/// An object of the collection, named by its ID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Object {
    Scene(SceneId),
    Item(ItemId),
    Source(SourceId),
    Output(OutputId),
}

/// A property an edit sets, on its object.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Prop {
    Name,
    Transform,
    Crop,
    Visible,
    /// An item's stacking position.
    Index,
    /// One parameter of a source's settings.
    Setting(String),
    /// An output's publish target.
    Target,
}

/// What a batch touched: computed from its edits and its inverse's, so a delete also
/// names what it removed (the inverse re-creates it).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Touched {
    /// Objects created or deleted.
    exists: BTreeSet<Object>,
    /// Objects referred to without being changed (the scene and source of an item).
    uses: BTreeSet<Object>,
    /// Properties set.
    props: BTreeSet<(Object, Prop)>,
    /// Whether the scenes' display order was set.
    scene_order: bool,
    /// An edit this module doesn't know: it conflicts with everything.
    unknown: bool,
}

impl Touched {
    /// What `forward` and `inverse` (the edits that undo it) touch together.
    pub(crate) fn of(forward: &[Edit], inverse: &[Edit]) -> Self {
        let mut touched = Touched::default();
        for edit in forward.iter().chain(inverse) {
            touched.add(edit);
        }
        touched
    }

    fn add(&mut self, edit: &Edit) {
        match edit {
            Edit::SceneCreate { scene_id, .. } | Edit::SceneDelete { scene_id } => {
                self.exists.insert(Object::Scene(scene_id.clone()));
            }
            Edit::SceneRename { scene_id, .. } => {
                self.prop(Object::Scene(scene_id.clone()), Prop::Name);
            }
            Edit::SceneReorder { .. } => self.scene_order = true,
            Edit::ItemAdd {
                scene_id,
                item_id,
                source_id,
                ..
            } => {
                self.exists.insert(Object::Item(item_id.clone()));
                self.uses.insert(Object::Scene(scene_id.clone()));
                self.uses.insert(Object::Source(source_id.clone()));
            }
            Edit::ItemRemove { item_id } => {
                self.exists.insert(Object::Item(item_id.clone()));
            }
            Edit::ItemSetTransform { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Transform);
            }
            Edit::ItemSetCrop { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Crop);
            }
            Edit::ItemSetVisible { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Visible);
            }
            Edit::ItemReorder { item_id, .. } => {
                self.prop(Object::Item(item_id.clone()), Prop::Index);
            }
            Edit::SourceCreate {
                source_id, kind, ..
            } => {
                self.exists.insert(Object::Source(source_id.clone()));
                if let jstream_protocol::SourceKind::Scene { scene_id } = kind {
                    self.uses.insert(Object::Scene(scene_id.clone()));
                }
            }
            Edit::SourceRename { source_id, .. } => {
                self.prop(Object::Source(source_id.clone()), Prop::Name);
            }
            Edit::SourceSetSettings { source_id, patch } => {
                for param in patch.0.keys() {
                    self.prop(
                        Object::Source(source_id.clone()),
                        Prop::Setting(param.clone()),
                    );
                }
            }
            Edit::SourceDuplicate {
                source_id,
                new_source_id,
                ..
            } => {
                self.uses.insert(Object::Source(source_id.clone()));
                self.exists.insert(Object::Source(new_source_id.clone()));
            }
            Edit::SourceDelete { source_id } => {
                self.exists.insert(Object::Source(source_id.clone()));
            }
            Edit::OutputCreate { output_id, .. } | Edit::OutputDelete { output_id } => {
                self.exists.insert(Object::Output(output_id.clone()));
            }
            Edit::OutputSetTarget { output_id, .. } => {
                self.prop(Object::Output(output_id.clone()), Prop::Target);
            }
            _ => self.unknown = true,
        }
    }

    fn prop(&mut self, object: Object, prop: Prop) {
        self.props.insert((object, prop));
    }

    /// Every object this touches in any way.
    fn objects(&self) -> impl Iterator<Item = &Object> {
        self.exists
            .iter()
            .chain(&self.uses)
            .chain(self.props.iter().map(|(object, _)| object))
    }

    /// Whether undoing or redoing one of these would overwrite the other: one creates or
    /// deletes an object the other touches at all, both set the same property, or both
    /// set the scenes' order.
    pub(crate) fn conflicts_with(&self, other: &Touched) -> bool {
        self.unknown
            || other.unknown
            || (self.scene_order && other.scene_order)
            || other.objects().any(|o| self.exists.contains(o))
            || self.objects().any(|o| other.exists.contains(o))
            || !self.props.is_disjoint(&other.props)
    }
}

/// One undoable step of a client's history.
#[derive(Clone, Debug)]
pub(crate) struct Step {
    /// The revision the step's batch originally produced. It names the step.
    pub(crate) step_rev: Rev,
    /// The edits to apply next: the inverse on the undo stack, the step again on the
    /// redo stack.
    pub(crate) edits: Vec<Edit>,
    /// What the step touches.
    touched: Touched,
    /// The revision at which the step was last applied or undone. Other clients'
    /// batches after it are checked for conflicts.
    since: Rev,
}

/// One client's stacks.
#[derive(Debug, Default)]
struct Stacks {
    undo: VecDeque<Step>,
    redo: Vec<Step>,
}

/// One revision in the conflict log.
#[derive(Debug)]
struct Entry {
    rev: Rev,
    client_id: ClientId,
    touched: Touched,
}

/// Every client's history, and the log of recent changes it checks for conflicts.
#[derive(Debug)]
pub(crate) struct History {
    clients: BTreeMap<ClientId, Stacks>,
    /// Applied batches, undos and redos in `rev` order, kept back to the oldest `since`
    /// of any step.
    log: VecDeque<Entry>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        History {
            clients: BTreeMap::new(),
            log: VecDeque::new(),
            limit: DEFAULT_HISTORY_LIMIT,
        }
    }
}

impl History {
    /// Sets how many steps each client keeps, dropping the oldest of any client over it.
    pub(crate) fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        for stacks in self.clients.values_mut() {
            while stacks.undo.len() > limit {
                stacks.undo.pop_front();
            }
            stacks.redo.truncate(limit);
        }
        self.prune();
    }

    /// Records a batch `client_id` applied: a new step on its undo stack, holding
    /// `inverse`, and its redo stack cleared.
    pub(crate) fn record_batch(
        &mut self,
        client_id: &ClientId,
        rev: Rev,
        forward: &[Edit],
        inverse: Vec<Edit>,
    ) {
        let touched = Touched::of(forward, &inverse);
        self.log(client_id, rev, touched.clone());
        if self.limit > 0 {
            let stacks = self.clients.entry(client_id.clone()).or_default();
            stacks.redo.clear();
            stacks.undo.push_back(Step {
                step_rev: rev,
                edits: inverse,
                touched,
                since: rev,
            });
            if stacks.undo.len() > self.limit {
                stacks.undo.pop_front();
            }
        }
        self.prune();
    }

    /// Takes the step an undo or redo by `client_id` would apply next.
    pub(crate) fn take(
        &mut self,
        client_id: &ClientId,
        direction: HistoryDirection,
    ) -> Option<Step> {
        let stacks = self.clients.get_mut(client_id)?;
        match direction {
            HistoryDirection::Redo => stacks.redo.pop(),
            _ => stacks.undo.pop_back(),
        }
    }

    /// Puts back a step [`take`](Self::take) returned, when nothing was applied and the
    /// step stays.
    pub(crate) fn put_back(
        &mut self,
        client_id: &ClientId,
        direction: HistoryDirection,
        step: Step,
    ) {
        let stacks = self.clients.entry(client_id.clone()).or_default();
        match direction {
            HistoryDirection::Redo => stacks.redo.push(step),
            _ => stacks.undo.push_back(step),
        }
    }

    /// Records that `step`, taken in `direction`, was applied at `rev`, and that applying
    /// it again (the other way) means applying `next`. It moves to the other stack.
    pub(crate) fn moved(
        &mut self,
        client_id: &ClientId,
        direction: HistoryDirection,
        step: Step,
        rev: Rev,
        next: Vec<Edit>,
    ) {
        let touched = Touched::of(&step.edits, &next);
        self.log(client_id, rev, touched.clone());
        let moved = Step {
            step_rev: step.step_rev,
            edits: next,
            touched,
            since: rev,
        };
        let stacks = self.clients.entry(client_id.clone()).or_default();
        match direction {
            HistoryDirection::Redo => stacks.undo.push_back(moved),
            _ => stacks.redo.push(moved),
        }
        self.prune();
    }

    /// The first batch by another client, after `step` was last applied or undone, that
    /// touched what the step touches: its client and `rev`.
    pub(crate) fn conflict(&self, client_id: &ClientId, step: &Step) -> Option<(ClientId, Rev)> {
        self.others_since(client_id, step.since)
            .find(|entry| entry.touched.conflicts_with(&step.touched))
            .map(|entry| (entry.client_id.clone(), entry.rev))
    }

    /// The latest batch by another client after `step` was last applied or undone: the
    /// one to name when the step's edits no longer apply.
    pub(crate) fn latest_other(
        &self,
        client_id: &ClientId,
        step: &Step,
    ) -> Option<(ClientId, Rev)> {
        self.others_since(client_id, step.since)
            .last()
            .map(|entry| (entry.client_id.clone(), entry.rev))
    }

    /// Drops a step that conflicted, and the log it no longer needs.
    pub(crate) fn dropped(&mut self) {
        self.prune();
    }

    /// How many steps `client_id` can undo and redo.
    pub(crate) fn depths(&self, client_id: &ClientId) -> (u32, u32) {
        let depth = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        self.clients
            .get(client_id)
            .map_or((0, 0), |s| (depth(s.undo.len()), depth(s.redo.len())))
    }

    /// Forgets `client_id`'s history: its session ended.
    pub(crate) fn forget(&mut self, client_id: &ClientId) {
        self.clients.remove(client_id);
        self.prune();
    }

    fn others_since<'a>(
        &'a self,
        client_id: &'a ClientId,
        since: Rev,
    ) -> impl Iterator<Item = &'a Entry> {
        self.log
            .iter()
            .filter(move |e| e.rev > since && &e.client_id != client_id)
    }

    fn log(&mut self, client_id: &ClientId, rev: Rev, touched: Touched) {
        self.log.push_back(Entry {
            rev,
            client_id: client_id.clone(),
            touched,
        });
    }

    /// Drops log entries no step can conflict with: those at or before every step's
    /// `since`.
    fn prune(&mut self) {
        let oldest = self
            .clients
            .values()
            .flat_map(|s| s.undo.iter().chain(&s.redo))
            .map(|step| step.since)
            .min();
        match oldest {
            Some(oldest) => {
                while self.log.front().is_some_and(|e| e.rev <= oldest) {
                    self.log.pop_front();
                }
            }
            None => self.log.clear(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Touched;
    use jstream_protocol::{Edit, ItemId, ItemTransform, SceneId, SourceId};

    fn add(scene: &str, item: &str, source: &str) -> Edit {
        Edit::ItemAdd {
            scene_id: SceneId::new(scene),
            item_id: ItemId::new(item),
            source_id: SourceId::new(source),
            transform: None,
        }
    }

    fn remove(item: &str) -> Edit {
        Edit::ItemRemove {
            item_id: ItemId::new(item),
        }
    }

    fn transform(item: &str) -> Edit {
        Edit::ItemSetTransform {
            item_id: ItemId::new(item),
            transform: ItemTransform::IDENTITY,
        }
    }

    fn visible(item: &str) -> Edit {
        Edit::ItemSetVisible {
            item_id: ItemId::new(item),
            visible: false,
        }
    }

    #[test]
    fn adding_items_to_one_scene_does_not_conflict() {
        let a = Touched::of(&[add("s", "a", "cam")], &[remove("a")]);
        let b = Touched::of(&[add("s", "b", "cam")], &[remove("b")]);
        assert!(!a.conflicts_with(&b));
    }

    #[test]
    fn the_same_property_conflicts_and_another_does_not() {
        let a = Touched::of(&[transform("i")], &[transform("i")]);
        assert!(a.conflicts_with(&Touched::of(&[transform("i")], &[transform("i")])));
        assert!(!a.conflicts_with(&Touched::of(&[visible("i")], &[visible("i")])));
        assert!(!a.conflicts_with(&Touched::of(&[transform("j")], &[transform("j")])));
    }

    #[test]
    fn deleting_an_object_conflicts_with_anything_on_it() {
        let moved = Touched::of(&[transform("i")], &[transform("i")]);
        let removed = Touched::of(&[remove("i")], &[add("s", "i", "cam")]);
        assert!(moved.conflicts_with(&removed));
        assert!(removed.conflicts_with(&moved));
        // Placing an item in a scene the step created.
        let created = Touched::of(
            &[Edit::SceneCreate {
                scene_id: SceneId::new("s"),
                name: "S".into(),
            }],
            &[Edit::SceneDelete {
                scene_id: SceneId::new("s"),
            }],
        );
        let placed = Touched::of(&[add("s", "j", "cam")], &[remove("j")]);
        assert!(created.conflicts_with(&placed));
    }
}
