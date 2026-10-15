//! Per-client undo and redo (ADR-0013, ADR-0032).
//!
//! Every batch a client applies becomes one step on that client's undo stack, holding
//! the inverse batch `Graph::apply` returned. An undo applies the inverse and moves the
//! step to the redo stack, holding the inverse of *that* application (the step again);
//! a redo moves it back. Another client's batches never touch a client's stacks.
//!
//! Conflicts: every applied batch, undo and redo is logged with the [`Touched`] set the
//! graph returned for it (the objects it created, deleted or used, and the properties it
//! set). Before an undo or
//! redo applies, the log entries by other clients since the step was last applied or
//! undone are checked against the step's own touched set. A match, or edits that no
//! longer apply, fails with `undo_conflict` and drops the step, so another client's work
//! is never overwritten.
//!
//! History is kept in memory for a client's session and dropped when the session ends.

use jstream_graph::Touched;
use jstream_protocol::{ClientId, Edit, HistoryDirection, Rev};
use std::collections::{BTreeMap, VecDeque};

/// The default number of steps kept per client; older steps are dropped first.
pub(crate) const DEFAULT_HISTORY_LIMIT: usize = 100;

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
            // The bottom of the redo stack is the step furthest from being redone.
            let excess = stacks.redo.len().saturating_sub(limit);
            stacks.redo.drain(..excess);
        }
        self.prune();
    }

    /// Records a batch `client_id` applied: a new step on its undo stack, holding
    /// `inverse`, and its redo stack cleared. `touched` is what the graph says it
    /// touched.
    pub(crate) fn record_batch(
        &mut self,
        client_id: &ClientId,
        rev: Rev,
        touched: Touched,
        inverse: Vec<Edit>,
    ) {
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

    /// Records that `step`, taken in `direction`, was applied at `rev`, touching
    /// `touched`, and that applying it again (the other way) means applying `next`. It
    /// moves to the other stack.
    pub(crate) fn moved(
        &mut self,
        client_id: &ClientId,
        direction: HistoryDirection,
        step: Step,
        rev: Rev,
        next: Vec<Edit>,
        touched: Touched,
    ) {
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
    /// touched what applying the step touches: its client and `rev`.
    ///
    /// `now` is what applying the step's edits to the current state touches, its new
    /// inverse included, when they apply. It can name more than the step did when it
    /// was recorded: undoing `scene.create` deletes whatever items the scene holds now,
    /// including ones whose own step was dropped (ADR-0032: an undo never overwrites
    /// another client's work).
    pub(crate) fn conflict(
        &self,
        client_id: &ClientId,
        step: &Step,
        now: Option<&Touched>,
    ) -> Option<(ClientId, Rev)> {
        let mut touched = step.touched.clone();
        if let Some(now) = now {
            touched.extend(now);
        }
        self.others_since(client_id, step.since)
            .find(|entry| entry.touched.conflicts_with(&touched))
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
