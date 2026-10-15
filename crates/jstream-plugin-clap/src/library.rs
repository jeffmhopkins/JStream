//! A loaded plugin file and the plugins it offers.

use std::ffi::CString;
use std::path::Path;
use std::sync::Arc;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::plugin::clap_plugin_descriptor;
use clap_sys::plugin_features::CLAP_PLUGIN_FEATURE_AUDIO_EFFECT;
use clap_sys::version::clap_version_is_compatible;

use crate::{ClapError, strings};

/// The most plugins read from one file's factory. Only bounds a broken plugin.
const MAX_PLUGINS: u32 = 1024;
/// The most features read from one descriptor. Only bounds a broken plugin.
const MAX_FEATURES: usize = 256;

/// One plugin a file offers, from its descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    /// The plugin's id (`com.vendor.plugin`), which [`Plugin::new`](crate::Plugin::new)
    /// takes.
    pub id: String,
    /// The plugin's name.
    pub name: String,
    /// Its vendor, if given.
    pub vendor: Option<String>,
    /// Its version, if given.
    pub version: Option<String>,
    /// Its description, if given.
    pub description: Option<String>,
    /// Its features (`audio-effect`, `compressor`, `stereo`…).
    pub features: Vec<String>,
}

impl PluginInfo {
    /// Whether the plugin declares itself an audio effect, the only kind ADR-0035 hosts.
    pub fn is_audio_effect(&self) -> bool {
        let effect = CLAP_PLUGIN_FEATURE_AUDIO_EFFECT.to_bytes();
        self.features.iter().any(|f| f.as_bytes() == effect)
    }

    /// Reads a descriptor.
    ///
    /// # Safety
    /// `desc` points to a valid descriptor whose strings stay valid for this call.
    pub(crate) unsafe fn from_descriptor(desc: &clap_plugin_descriptor) -> Option<Self> {
        // SAFETY: every pointer comes from the descriptor, which the caller guarantees
        // valid; `from_ptr` accepts null; the feature list is null-terminated.
        unsafe {
            let id = strings::from_ptr(desc.id)?;
            let mut features = Vec::new();
            if !desc.features.is_null() {
                for i in 0..MAX_FEATURES {
                    let f = *desc.features.add(i);
                    match strings::from_ptr(f) {
                        Some(f) => features.push(f),
                        None => break,
                    }
                }
            }
            Some(Self {
                name: strings::from_ptr(desc.name).unwrap_or_else(|| id.clone()),
                id,
                vendor: strings::from_ptr(desc.vendor),
                version: strings::from_ptr(desc.version),
                description: strings::from_ptr(desc.description),
                features,
            })
        }
    }
}

/// A plugin file, loaded and its entry initialised. Dropping it deinitialises the entry
/// and unloads the file, so every [`Plugin`](crate::Plugin) holds it in an [`Arc`].
pub struct Library {
    entry: *const clap_plugin_entry,
    /// The loaded file; `None` for a test's built-in entry. Dropped after `deinit`.
    _lib: Option<libloading::Library>,
}

// SAFETY: the entry is a table of C function pointers and a version, valid while the
// library stays loaded, which `Library` guarantees. CLAP makes `get_factory` and the
// factory thread-safe; `init` runs once in `load` and `deinit` once in `Drop`.
unsafe impl Send for Library {}
// SAFETY: as for `Send`: shared use only calls the thread-safe `get_factory`.
unsafe impl Sync for Library {}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library").finish_non_exhaustive()
    }
}

impl Library {
    /// Loads the plugin file at `path` and initialises its entry.
    ///
    /// Loading a native library runs its initialisers: this is the step that trusts the
    /// file (ADR-0035 §4). Only the plugin host calls it, never the engine.
    ///
    /// # Errors
    /// [`ClapError::Load`] when the file can't be loaded, [`ClapError::NoEntry`] when it
    /// exports no `clap_entry`, [`ClapError::Version`] for an incompatible CLAP version,
    /// and [`ClapError::EntryInit`] when its `init` fails.
    pub fn load(path: &Path) -> Result<Arc<Self>, ClapError> {
        // SAFETY: loading a library runs its initialisers, which is the trust the user
        // gave by choosing the plugin; the host process exists to contain it.
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|e| ClapError::Load(e.to_string()))?;
        // SAFETY: `clap_entry` is a data symbol of type `clap_plugin_entry` in every CLAP
        // plugin; `get` returns the symbol's address, read as a pointer to that type.
        let entry: *const clap_plugin_entry = unsafe {
            let symbol = lib
                .get::<*const clap_plugin_entry>(b"clap_entry\0")
                .map_err(|_| ClapError::NoEntry)?;
            symbol.try_as_raw_ptr().ok_or(ClapError::NoEntry)?.cast()
        };
        if entry.is_null() {
            return Err(ClapError::NoEntry);
        }
        let path_c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| ClapError::Load("the path contains a NUL byte".into()))?;
        // SAFETY: `entry` points into `lib`, which outlives this call and moves into the
        // returned `Library`.
        unsafe { Self::init(entry, &path_c, Some(lib)) }
    }

    /// A library from an entry built into this binary, for tests.
    #[cfg(test)]
    pub(crate) fn from_static(entry: &'static clap_plugin_entry) -> Result<Arc<Self>, ClapError> {
        // SAFETY: a static entry is valid for the whole program.
        unsafe { Self::init(entry, c"test", None) }
    }

    /// Checks the entry's version and runs its `init`.
    ///
    /// # Safety
    /// `entry` points to a valid entry that stays valid while `lib` is loaded (or for
    /// ever, when `lib` is `None`).
    unsafe fn init(
        entry: *const clap_plugin_entry,
        path: &std::ffi::CStr,
        lib: Option<libloading::Library>,
    ) -> Result<Arc<Self>, ClapError> {
        // SAFETY: the caller guarantees `entry` is valid.
        let e = unsafe { &*entry };
        if !clap_version_is_compatible(e.clap_version) {
            return Err(ClapError::Version {
                major: e.clap_version.major,
                minor: e.clap_version.minor,
            });
        }
        let init = e.init.ok_or(ClapError::EntryInit)?;
        // SAFETY: CLAP's entry contract: `init` is called once, with the plugin's path,
        // before anything else.
        if !unsafe { init(path.as_ptr()) } {
            return Err(ClapError::EntryInit);
        }
        Ok(Arc::new(Self { entry, _lib: lib }))
    }

    /// The file's plugin factory, if it has one.
    pub(crate) fn factory(&self) -> Option<&clap_plugin_factory> {
        // SAFETY: `entry` stays valid while `self` exists.
        let get = unsafe { (*self.entry).get_factory }?;
        // SAFETY: `get_factory` is thread-safe and takes a NUL-terminated id.
        let factory = unsafe { get(CLAP_PLUGIN_FACTORY_ID.as_ptr()) };
        // SAFETY: a non-null result for this id is a plugin factory that lives as long as
        // the library.
        unsafe { factory.cast::<clap_plugin_factory>().as_ref() }
    }

    /// The plugins the file offers, in the factory's order. Empty when it has no
    /// factory. Descriptors without an id are skipped.
    pub fn plugins(&self) -> Vec<PluginInfo> {
        let Some(factory) = self.factory() else {
            return Vec::new();
        };
        let (Some(count), Some(get)) = (factory.get_plugin_count, factory.get_plugin_descriptor)
        else {
            return Vec::new();
        };
        // SAFETY: the factory's functions are thread-safe and take the factory itself.
        let n = unsafe { count(factory) }.min(MAX_PLUGINS);
        (0..n)
            .filter_map(|i| {
                // SAFETY: `i` is below the count the factory gave.
                let desc = unsafe { get(factory, i).as_ref() }?;
                // SAFETY: the factory's descriptors live as long as the library.
                unsafe { PluginInfo::from_descriptor(desc) }
            })
            .collect()
    }

    /// Plugin `id`'s descriptor.
    pub(crate) fn find(&self, id: &str) -> Result<PluginInfo, ClapError> {
        if self.factory().is_none() {
            return Err(ClapError::NoFactory);
        }
        self.plugins()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| ClapError::NoSuchPlugin(id.into()))
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: `entry` is still valid (the library unloads after this), and every
        // plugin holding this library is gone, so `deinit` is the last call.
        if let Some(deinit) = unsafe { (*self.entry).deinit } {
            // SAFETY: CLAP's entry contract: `deinit` once, after a successful `init`.
            unsafe { deinit() };
        }
    }
}

/// The `char` pointer of a C string for the plugin, kept alive by the caller.
pub(crate) fn c_id(id: &str) -> Result<CString, ClapError> {
    CString::new(id).map_err(|_| ClapError::NoSuchPlugin(id.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testplugin;

    #[test]
    fn lists_the_plugins_of_a_file() {
        let lib = Library::from_static(&testplugin::ENTRY).unwrap();
        let plugins = lib.plugins();
        let ids: Vec<&str> = plugins.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                testplugin::GAIN_ID,
                testplugin::SYNTH_ID,
                testplugin::MONO_ID
            ]
        );
        let gain = &plugins[0];
        assert_eq!(gain.name, "Test gain");
        assert_eq!(gain.vendor.as_deref(), Some("JStream tests"));
        assert!(gain.is_audio_effect());
        assert!(!plugins[1].is_audio_effect());
        assert!(matches!(lib.find("nope"), Err(ClapError::NoSuchPlugin(_))));
    }

    #[test]
    fn a_missing_file_is_a_load_error() {
        let err = Library::load(Path::new("/nonexistent/plugin.clap")).unwrap_err();
        assert!(matches!(err, ClapError::Load(_)), "{err:?}");
    }

    /// A real native library without a CLAP entry (the C library) is refused.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_library_without_an_entry_is_refused() {
        let err = Library::load(Path::new("libc.so.6")).unwrap_err();
        assert_eq!(err, ClapError::NoEntry);
    }
}
