//! The render pipelines made on one device, shared by every compositor on it.
//!
//! Making a pipeline compiles its shaders for the backend, which on Direct3D 12 takes
//! from tens of milliseconds to seconds each. A compositor is made at every output
//! start, beside the preview's and the snapshot thread's, all on the engine's one
//! context, so each of them asks here first ([`Built::render`]) and compiles only what
//! no compositor on the device has made yet. A pipeline another thread is making is
//! waited for, not made twice, so one made ahead of time on a thread of its own
//! ([`crate::Compositor::warm`]) is ready, or at least under way, when a draw asks.
//!
//! A pipeline is found by everything it was made from ([`Key`]). Its bind group
//! layouts are named by the caller, which makes them again for each compositor: wgpu
//! treats two bind group layouts with the same entries as one, so a pipeline made with
//! one compositor's layouts draws with another's bind groups.

use std::collections::{HashMap, VecDeque};
use std::fmt::{Debug, Write as _};
use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// What a pipeline was made from: every field of its descriptor, its shader's source,
/// and what the caller says its layout is.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key(String);

impl Key {
    /// The key of the pipeline `desc` describes, whose shader modules were both made
    /// from `source`. `layout` tells apart the bind group layouts this label's pipeline
    /// can have (`()` where it has one).
    pub(crate) fn of(
        source: &str,
        layout: &dyn Debug,
        desc: &wgpu::RenderPipelineDescriptor<'_>,
    ) -> Self {
        let mut key = String::new();
        // The descriptor's parts one by one: its modules and layout print as ids that
        // differ for each compositor.
        let _ = write!(
            key,
            "{:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {}",
            desc.label,
            layout,
            (
                desc.vertex.entry_point,
                desc.vertex.buffers,
                &desc.vertex.compilation_options,
            ),
            desc.fragment
                .as_ref()
                .map(|f| (f.entry_point, f.targets, &f.compilation_options)),
            desc.primitive,
            desc.depth_stencil,
            desc.multisample,
            desc.multiview_mask,
            source.len(),
            // The source by a hash, not held once more for each pipeline made from it.
            Self::hash(source),
        );
        Self(key)
    }

    /// `source`'s hash, by a hasher seeded once for the process.
    fn hash(source: &str) -> u64 {
        static SEED: OnceLock<RandomState> = OnceLock::new();
        SEED.get_or_init(RandomState::new).hash_one(source)
    }
}

/// A pipeline, or why wgpu refused it.
type Made = Result<wgpu::RenderPipeline, String>;

/// The most pipelines kept. The engine's own come to a few hundred; a pack's shader
/// edited again and again adds one for each source, so the oldest is let go past this.
/// A compositor that draws with it holds its own clone, and one that asks after builds
/// it again.
const MOST: usize = 1024;

/// Each pipeline by its key, once made, and the keys from the oldest.
#[derive(Default)]
struct Slots {
    made: HashMap<Key, Arc<OnceLock<Made>>>,
    order: VecDeque<Key>,
}

/// The pipelines made on one device.
#[derive(Default)]
pub(crate) struct Built {
    made: Mutex<Slots>,
    /// How many pipelines were made, not counting the ones found.
    pub(crate) makes: AtomicU64,
}

impl Built {
    /// The pipeline of `key`, made by `make` if no compositor on the device made it
    /// yet. While another thread makes it, this waits for that one. A refusal is kept
    /// as the pipeline would be, so the shader is not compiled again.
    pub(crate) fn get(&self, key: Key, make: impl FnOnce() -> Made) -> Made {
        let slot = {
            let mut made = self.made.lock().unwrap_or_else(PoisonError::into_inner);
            let Slots { made, order } = &mut *made;
            if !made.contains_key(&key) {
                if order.len() >= MOST
                    && let Some(oldest) = order.pop_front()
                {
                    made.remove(&oldest);
                }
                order.push_back(key.clone());
            }
            Arc::clone(made.entry(key).or_default())
        };
        // The map is not held while a pipeline is made: other keys go on.
        slot.get_or_init(|| {
            self.makes.fetch_add(1, Ordering::Relaxed);
            make()
        })
        .clone()
    }

    /// Lets go of `key`'s pipeline or refusal, so the next ask makes it again. A
    /// compositor that draws with it holds its own clone.
    pub(crate) fn forget(&self, key: &Key) {
        let mut made = self.made.lock().unwrap_or_else(PoisonError::into_inner);
        if made.made.remove(key).is_some() {
            made.order.retain(|kept| kept != key);
        }
    }

    /// [`Self::get`] for a pipeline of the engine's own shaders, which wgpu does not
    /// refuse: `desc`'s, made on `device`.
    pub(crate) fn render(
        &self,
        device: &wgpu::Device,
        source: &str,
        layout: &dyn Debug,
        desc: &wgpu::RenderPipelineDescriptor<'_>,
    ) -> wgpu::RenderPipeline {
        let made = self.get(Key::of(source, layout, desc), || {
            Ok(device.create_render_pipeline(desc))
        });
        match made {
            Ok(pipeline) => pipeline,
            // `make` above never refuses.
            Err(_) => device.create_render_pipeline(desc),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::{Built, Key, MOST, Ordering};

    fn key(name: &str) -> Key {
        Key(name.to_owned())
    }

    /// A refusal is kept as a pipeline is: the second ask doesn't make it again.
    #[test]
    fn a_refusal_is_kept_and_not_made_again() {
        let built = Built::default();
        let first = built.get(key("a"), || Err("refused".into()));
        let again = built.get(key("a"), || panic!("made twice"));
        assert_eq!(first.err().as_deref(), Some("refused"));
        assert_eq!(again.err().as_deref(), Some("refused"));
        assert_eq!(built.makes.load(Ordering::Relaxed), 1);
    }

    /// Two threads asking for one key make it once: the second waits for the first.
    /// Another key is not held up by it.
    #[test]
    fn a_key_being_made_is_waited_for_and_other_keys_go_on() {
        let built = Arc::new(Built::default());
        let started = Arc::new(Barrier::new(2));
        let finish = Arc::new(Barrier::new(2));
        let maker = {
            let (built, started, finish) = (built.clone(), started.clone(), finish.clone());
            std::thread::spawn(move || {
                built.get(key("slow"), || {
                    started.wait();
                    finish.wait();
                    Err("slow".into())
                })
            })
        };
        started.wait();
        // While "slow" is being made, another key is made at once.
        assert!(built.get(key("quick"), || Err("quick".into())).is_err());
        let waiter = {
            let built = built.clone();
            std::thread::spawn(move || built.get(key("slow"), || Err("made twice".into())))
        };
        finish.wait();
        assert_eq!(maker.join().expect("joins").err().as_deref(), Some("slow"));
        assert_eq!(waiter.join().expect("joins").err().as_deref(), Some("slow"));
        assert_eq!(built.makes.load(Ordering::Relaxed), 2);
    }

    /// Past the most kept, the oldest key is let go and made again when asked for.
    #[test]
    fn the_oldest_key_is_let_go_past_the_most_kept() {
        let built = Built::default();
        for n in 0..=MOST {
            let _ = built.get(key(&n.to_string()), || Err(String::new()));
        }
        let _ = built.get(key("1"), || panic!("the second oldest is kept"));
        built.forget(&key("1"));
        let forgotten = built.get(key("1"), || Err("forgotten".into()));
        assert_eq!(forgotten.err().as_deref(), Some("forgotten"));
        let again = built.get(key("0"), || Err("again".into()));
        assert_eq!(again.err().as_deref(), Some("again"));
    }
}
