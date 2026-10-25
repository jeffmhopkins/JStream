//! Lock-free commands that add and remove mixer inputs, buses, output taps and filters,
//! and edit filters, while the mixer runs.
//!
//! The control thread allocates the ring and the producer (or a bus's shared state),
//! then `try_send`s it to the mixer. The mixer applies the command at the start of the next block and does
//! not allocate to do it (R-RT-001). The queue is bounded and lock-free
//! ([`crossbeam_channel`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crossbeam_channel::Sender;
use jstream_media::{AudioFormat, SampleFormat};

use super::bus::{BusShared, MASTER_ID, MAX_BUSES};
use super::chain::{
    Chain, FilterBook, FilterParams, FilterSpec, Kernel, MAX_CHAIN_LATENCY_MS, MAX_FILTERS,
};
use super::host::{HostFilter, HostMemory};
use super::input::{InputProducer, InputShared, PrivacyLink};
use super::retire::Retirer;
use super::ring::FrameRing;
use super::sinc::{MAX_DOWN_RATIO, MAX_UP_RATIO};
use super::tap::{MAX_OUTPUT_TAPS, Tap};
use crate::error::AudioError;

/// Inputs the mixer can hold without growing its list on the mixer thread.
pub(crate) const MAX_INPUTS: usize = 64;

/// Pending adds and removes. Larger than [`MAX_INPUTS`] so a full set can be replaced
/// before the mixer has applied the removals.
pub(crate) const COMMAND_CAP: usize = 128;

/// The filter chain a command changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ChainTarget {
    Input(u64),
    Bus {
        slot: usize,
        id: u64,
    },
    /// The master chain, which every bus has its own copy of.
    Master,
    OutputTap(u64),
}

/// One copy of a new master filter for each bus, by slot, with the id of the bus it
/// was built for.
pub(crate) type MasterKernels = Box<[Option<(u64, Box<Kernel>)>]>;

pub(crate) enum Command {
    Add {
        id: u64,
        ring: Arc<FrameRing>,
        shared: Arc<InputShared>,
        chain: Box<Chain>,
        /// The input's monitor block ([`Mixer::input_block`](super::mixer::Mixer::input_block)):
        /// `block_frames` frames, allocated here so the mixer thread never does.
        block: Box<[[f32; 2]]>,
    },
    Remove(u64),
    AddBus {
        slot: usize,
        id: u64,
        shared: Arc<BusShared>,
        chain: Box<Chain>,
        /// Its copy of the master chain, with every master filter queued so far.
        master: Box<Chain>,
    },
    RemoveBus {
        slot: usize,
        id: u64,
    },
    AddFilter {
        target: ChainTarget,
        index: usize,
        id: u64,
        kernel: Box<Kernel>,
    },
    /// A master filter, one copy per bus. The emptied list goes to the collector.
    AddMasterFilter {
        index: usize,
        id: u64,
        kernels: MasterKernels,
    },
    AddTap(Tap),
    RemoveTap(u64),
    RemoveFilter {
        target: ChainTarget,
        id: u64,
    },
    EditFilter {
        target: ChainTarget,
        id: u64,
        params: FilterParams,
    },
}

#[derive(Debug)]
pub(crate) struct ControlPlane {
    tx: Sender<Command>,
    next_id: AtomicU64,
    /// Ids accepted and not yet removed. Touched only from the control plane, never
    /// from the mixer thread.
    admitted: Mutex<BTreeSet<u64>>,
    /// The id of the bus in each slot, as the control plane has queued it. Slot 0 is
    /// the master. Touched only from the control plane.
    buses: Mutex<[Option<u64>; MAX_BUSES]>,
    next_bus_id: AtomicU64,
    /// Each live input's and bus's filters, as the control plane has queued them.
    /// Touched only from the control plane.
    books: Mutex<BTreeMap<ChainTarget, Vec<FilterBook>>>,
    next_filter_id: AtomicU64,
    /// Output taps accepted and not yet removed. Touched only from the control plane.
    taps: Mutex<BTreeSet<u64>>,
    next_tap_id: AtomicU64,
    retirer: Retirer,
    sample_rate: u32,
    ring_frames: usize,
    /// This mixer's tag, unique in the process and never 0, carried by its
    /// [`BusId`](crate::BusId)s so another mixer's id is refused (#228 N2).
    tag: u64,
    block_frames: usize,
    /// The mixer's privacy screen as every input's audio thread reads it.
    privacy: Arc<PrivacyLink>,
}

/// The next mixer's tag. 0 is [`BusId::MASTER`](crate::BusId::MASTER)'s, which names
/// every mixer's master.
static NEXT_TAG: AtomicU64 = AtomicU64::new(1);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ControlPlane {
    pub(crate) fn new(
        tx: Sender<Command>,
        retirer: Retirer,
        sample_rate: u32,
        ring_frames: usize,
        block_frames: usize,
        privacy: Arc<PrivacyLink>,
    ) -> Self {
        Self {
            tx,
            next_id: AtomicU64::new(1),
            admitted: Mutex::new(BTreeSet::new()),
            buses: Mutex::new(std::array::from_fn(|slot| (slot == 0).then_some(MASTER_ID))),
            next_bus_id: AtomicU64::new(MASTER_ID + 1),
            books: Mutex::new(BTreeMap::from([
                (
                    ChainTarget::Bus {
                        slot: 0,
                        id: MASTER_ID,
                    },
                    Vec::new(),
                ),
                (ChainTarget::Master, Vec::new()),
            ])),
            next_filter_id: AtomicU64::new(1),
            taps: Mutex::new(BTreeSet::new()),
            next_tap_id: AtomicU64::new(1),
            retirer,
            sample_rate,
            ring_frames,
            tag: NEXT_TAG.fetch_add(1, Ordering::Relaxed),
            block_frames,
            privacy,
        }
    }

    /// This mixer's tag.
    pub(crate) fn tag(&self) -> u64 {
        self.tag
    }

    /// Allocates an input and queues it for the mixer. The producer may push audio
    /// before the mixer has installed the input; the ring holds it.
    pub(crate) fn add(
        &self,
        format: AudioFormat,
        privacy_muted: bool,
    ) -> Result<(InputProducer, u64, Arc<InputShared>, Arc<FrameRing>), AudioError> {
        if format.sample_format() != SampleFormat::F32Interleaved {
            return Err(AudioError::UnsupportedFormat);
        }
        let rate = format.sample_rate();
        let engine = self.sample_rate;
        if u64::from(rate) > u64::from(engine) * u64::from(MAX_DOWN_RATIO)
            || u64::from(rate) * u64::from(MAX_UP_RATIO) < u64::from(engine)
        {
            return Err(AudioError::UnsupportedRate { rate, engine });
        }
        let mut admitted = lock(&self.admitted);
        if admitted.len() >= MAX_INPUTS {
            return Err(AudioError::TooManyInputs);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let ring = Arc::new(FrameRing::new(self.ring_frames));
        let mut shared = InputShared::new();
        // Before the command is sent, so the mixer installs the input already marked.
        shared.privacy_muted = AtomicBool::new(privacy_muted);
        shared.privacy = Arc::clone(&self.privacy);
        let shared = Arc::new(shared);
        let queued = Command::Add {
            id,
            ring: Arc::clone(&ring),
            shared: Arc::clone(&shared),
            chain: Box::new(Chain::new(engine)?),
            block: vec![[0.0; 2]; self.block_frames].into_boxed_slice(),
        };
        if self.tx.try_send(queued).is_err() {
            return Err(AudioError::CommandQueueFull);
        }
        admitted.insert(id);
        lock(&self.books).insert(ChainTarget::Input(id), Vec::new());
        drop(admitted);
        let producer = InputProducer::new(
            format,
            engine,
            Arc::clone(&ring),
            Arc::clone(&shared),
            self.retirer.clone(),
        );
        Ok((producer, id, shared, ring))
    }

    /// The collector this mixer's objects are retired to.
    pub(crate) fn retirer(&self) -> &Retirer {
        &self.retirer
    }

    /// Queues removal of `id`. False when `id` is unknown or already removed, or when
    /// the command queue cannot take it (the input stays).
    pub(crate) fn remove(&self, id: u64) -> bool {
        let mut admitted = lock(&self.admitted);
        if !admitted.remove(&id) {
            return false;
        }
        if self.tx.try_send(Command::Remove(id)).is_err() {
            admitted.insert(id);
            return false;
        }
        lock(&self.books).remove(&ChainTarget::Input(id));
        true
    }

    /// Allocates a bus and queues it for the mixer. Returns its slot, its id and its
    /// shared state.
    pub(crate) fn add_bus(&self) -> Result<(usize, u64, Arc<BusShared>), AudioError> {
        let mut buses = lock(&self.buses);
        let Some(slot) = buses.iter().position(Option::is_none) else {
            return Err(AudioError::TooManyBuses);
        };
        // Held while the master copy is built and queued, so a master filter is either
        // in it or queued after it with a copy for this bus.
        let mut books = lock(&self.books);
        let master = self.master_copy(books.get(&ChainTarget::Master))?;
        let id = self.next_bus_id.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::new(BusShared::new());
        let queued = Command::AddBus {
            slot,
            id,
            shared: Arc::clone(&shared),
            chain: Box::new(Chain::new(self.sample_rate)?),
            master: Box::new(master),
        };
        if self.tx.try_send(queued).is_err() {
            return Err(AudioError::CommandQueueFull);
        }
        buses[slot] = Some(id);
        books.insert(ChainTarget::Bus { slot, id }, Vec::new());
        Ok((slot, id, shared))
    }

    /// A bus's copy of the master chain: each filter in `book` built from its spec,
    /// edits included.
    fn master_copy(&self, book: Option<&Vec<FilterBook>>) -> Result<Chain, AudioError> {
        let mut chain = Chain::new(self.sample_rate)?;
        for filter in book.into_iter().flatten() {
            // Every master filter has a spec: `add_host_filter` refuses the master.
            let spec = filter.spec.ok_or(AudioError::InvalidConfig {
                what: "a CLAP filter can't be on the master chain",
            })?;
            let kernel = Box::new(Kernel::build(self.sample_rate, spec)?);
            // The book holds at most MAX_FILTERS, which the chain has room for.
            if chain.insert(usize::MAX, filter.id, kernel).is_err() {
                return Err(AudioError::TooManyFilters);
            }
        }
        Ok(chain)
    }

    /// Adds an output tap on bus `bus` in `slot` and queues it for the mixer. Returns
    /// its id.
    pub(crate) fn add_tap(&self, slot: usize, bus: u64) -> Result<u64, AudioError> {
        if lock(&self.buses).get(slot).copied().flatten() != Some(bus) {
            return Err(AudioError::Removed);
        }
        let mut taps = lock(&self.taps);
        if taps.len() >= MAX_OUTPUT_TAPS {
            return Err(AudioError::TooManyOutputTaps);
        }
        let id = self.next_tap_id.fetch_add(1, Ordering::Relaxed);
        let chain = Box::new(Chain::new(self.sample_rate)?);
        let tap = Tap::new(id, slot, bus, chain, self.block_frames);
        if self.tx.try_send(Command::AddTap(tap)).is_err() {
            return Err(AudioError::CommandQueueFull);
        }
        taps.insert(id);
        lock(&self.books).insert(ChainTarget::OutputTap(id), Vec::new());
        Ok(id)
    }

    /// Queues removal of output tap `id`. False when it is already removed, or when the
    /// command queue cannot take it (the tap stays).
    pub(crate) fn remove_tap(&self, id: u64) -> bool {
        let mut taps = lock(&self.taps);
        if !taps.contains(&id) || self.tx.try_send(Command::RemoveTap(id)).is_err() {
            return false;
        }
        taps.remove(&id);
        lock(&self.books).remove(&ChainTarget::OutputTap(id));
        true
    }

    /// Queues removal of bus `id` in `slot`. False for the master, for a bus already
    /// removed, or when the command queue cannot take it (the bus stays).
    pub(crate) fn remove_bus(&self, slot: usize, id: u64) -> bool {
        if id == MASTER_ID {
            return false;
        }
        let mut buses = lock(&self.buses);
        if buses.get(slot).copied().flatten() != Some(id) {
            return false;
        }
        if self.tx.try_send(Command::RemoveBus { slot, id }).is_err() {
            return false;
        }
        buses[slot] = None;
        lock(&self.books).remove(&ChainTarget::Bus { slot, id });
        true
    }

    /// Builds the filter `spec` describes and queues it for `target`'s chain at `index`
    /// (clamped to the end). Returns its id and latency.
    pub(crate) fn add_filter(
        &self,
        target: ChainTarget,
        index: usize,
        spec: FilterSpec,
    ) -> Result<(u64, usize), AudioError> {
        if target == ChainTarget::Master {
            return self.add_master_filter(index, spec);
        }
        self.queue_filter(target, index, Some(spec), || {
            Kernel::build(self.sample_rate, spec)
        })
    }

    /// Builds the mixer side of the CLAP filter whose host uses `memory`, and queues it
    /// for `target`'s chain at `index` (clamped to the end). Returns its id and latency.
    /// Not on the master chain, which every bus runs its own copy of: each copy would
    /// need its own host.
    pub(crate) fn add_host_filter(
        &self,
        target: ChainTarget,
        index: usize,
        memory: HostMemory,
    ) -> Result<(u64, usize), AudioError> {
        if target == ChainTarget::Master {
            return Err(AudioError::InvalidConfig {
                what: "a CLAP filter can't be on the master chain",
            });
        }
        self.queue_filter(target, index, None, || {
            HostFilter::new(self.sample_rate, self.block_frames, memory).map(Kernel::Host)
        })
    }

    /// Builds a filter with `build` for `target`'s chain, if the chain has room for it,
    /// and queues it at `index`. `spec` is what it was built from, if anything.
    fn queue_filter(
        &self,
        target: ChainTarget,
        index: usize,
        spec: Option<FilterSpec>,
        build: impl FnOnce() -> Result<Kernel, AudioError>,
    ) -> Result<(u64, usize), AudioError> {
        let mut books = lock(&self.books);
        let book = books.get_mut(&target).ok_or(AudioError::Removed)?;
        let kernel = self.admit(book, build)?;
        let latency = kernel.latency_frames();
        let id = self.next_filter_id.fetch_add(1, Ordering::Relaxed);
        let filter = FilterBook::new(id, spec, &kernel);
        let queued = Command::AddFilter {
            target,
            index,
            id,
            kernel: Box::new(kernel),
        };
        if self.tx.try_send(queued).is_err() {
            return Err(AudioError::CommandQueueFull);
        }
        book.insert(index.min(book.len()), filter);
        Ok((id, latency))
    }

    /// Builds a filter with `build` for a chain holding `book`, if the chain has room
    /// for it: at most [`MAX_FILTERS`] filters and [`MAX_CHAIN_LATENCY_MS`] of latency.
    fn admit(
        &self,
        book: &[FilterBook],
        build: impl FnOnce() -> Result<Kernel, AudioError>,
    ) -> Result<Kernel, AudioError> {
        if book.len() >= MAX_FILTERS {
            return Err(AudioError::TooManyFilters);
        }
        let kernel = build()?;
        let total: usize = book.iter().map(|f| f.latency).sum::<usize>() + kernel.latency_frames();
        let cap = (f64::from(MAX_CHAIN_LATENCY_MS) * f64::from(self.sample_rate) / 1000.0).round()
            as usize;
        if total > cap {
            return Err(AudioError::InvalidConfig {
                what: "a filter chain's latency must be at most 100 ms",
            });
        }
        Ok(kernel)
    }

    /// Builds a master filter once per bus and queues the copies in one command, so
    /// every bus's master chain gets it at the same block.
    fn add_master_filter(
        &self,
        index: usize,
        spec: FilterSpec,
    ) -> Result<(u64, usize), AudioError> {
        let buses = lock(&self.buses);
        let mut books = lock(&self.books);
        let book = books.entry(ChainTarget::Master).or_default();
        let first = self.admit(book, || Kernel::build(self.sample_rate, spec))?;
        let latency = first.latency_frames();
        let id = self.next_filter_id.fetch_add(1, Ordering::Relaxed);
        let filter = FilterBook::new(id, Some(spec), &first);
        let mut first = Some(first);
        let mut kernels: Vec<Option<(u64, Box<Kernel>)>> = Vec::with_capacity(MAX_BUSES);
        for bus in buses.iter() {
            kernels.push(match bus {
                Some(bus) => {
                    let kernel = match first.take() {
                        Some(kernel) => kernel,
                        None => Kernel::build(self.sample_rate, spec)?,
                    };
                    Some((*bus, Box::new(kernel)))
                }
                None => None,
            });
        }
        let queued = Command::AddMasterFilter {
            index,
            id,
            kernels: kernels.into_boxed_slice(),
        };
        if self.tx.try_send(queued).is_err() {
            return Err(AudioError::CommandQueueFull);
        }
        book.insert(index.min(book.len()), filter);
        Ok((id, latency))
    }

    /// Queues removal of filter `id` from `target`'s chain. False when it is not there
    /// (removed already, or its input or bus was), or the queue cannot take it.
    pub(crate) fn remove_filter(&self, target: ChainTarget, id: u64) -> bool {
        let mut books = lock(&self.books);
        let Some(book) = books.get_mut(&target) else {
            return false;
        };
        let Some(index) = book.iter().position(|f| f.id == id) else {
            return false;
        };
        if self
            .tx
            .try_send(Command::RemoveFilter { target, id })
            .is_err()
        {
            return false;
        }
        book.remove(index);
        true
    }

    /// Checks `params` and queues them for filter `id` in `target`'s chain.
    pub(crate) fn edit_filter(
        &self,
        target: ChainTarget,
        id: u64,
        params: FilterParams,
    ) -> Result<(), AudioError> {
        let mut books = lock(&self.books);
        let filter = books
            .get_mut(&target)
            .and_then(|book| book.iter_mut().find(|f| f.id == id))
            .ok_or(AudioError::Removed)?;
        filter.check(self.sample_rate, &params)?;
        self.tx
            .try_send(Command::EditFilter { target, id, params })
            .map_err(|_| AudioError::CommandQueueFull)?;
        // A bus added later builds its master copy from this.
        filter.spec = filter.spec.map(|spec| spec.with(params));
        Ok(())
    }
}
