//! The helper on macOS (ADR-0157): what CEF needs of a process before it starts, which on
//! Windows the bootstrap exe and on Linux the executable's own linking provide.
//!
//! **The bundle.** The browser process is `JStream.app/Contents/MacOS/jstream-browser-helper`.
//! CEF's framework is `Contents/Frameworks/Chromium Embedded Framework.framework`, and
//! its subprocesses run from the helper apps beside it, `JStream Helper.app` and its
//! `(GPU)`, `(Renderer)`, `(Plugin)` and `(Alerts)` variants, each holding a copy of the
//! same executable (`scripts/mac/bundle-app.sh` lays them out). [`bundle_settings`] names
//! the bundle, the framework and the plain helper to CEF; Chromium finds the variants
//! beside the plain one by their names.
//!
//! **A subprocess** has `--type=` on its command line ([`is_subprocess`]). It enters CEF's
//! Seatbelt sandbox first ([`Sandbox`]), through `libcef_sandbox.dylib` in the framework,
//! before it loads anything else, then loads the framework three folders up from its
//! executable ([`Framework`]).
//!
//! **The browser process** loads the framework from `../Frameworks`, then makes `NSApp`
//! an instance of [`HelperApplication`], which speaks Chromium's `CrAppControlProtocol`
//! as CEF requires of an app's `NSApplication` on macOS, and keeps it out of the Dock
//! and the app switcher: the helper never shows a window.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ::cef::application_mac::{CefAppProtocol, CrAppControlProtocol, CrAppProtocol};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEvent, NSResponder};

/// The plain helper app's name, in `Contents/Frameworks`; its variants add ` (GPU)`,
/// ` (Renderer)`, ` (Plugin)` and ` (Alerts)`. `scripts/mac/bundle-app.sh` uses the same.
const HELPER_APP: &str = "JStream Helper";

/// CEF's framework, in `Contents/Frameworks`.
const FRAMEWORK: &str = "Chromium Embedded Framework.framework";

/// What the engine is told when `NSApp` can't be the helper's application class.
pub(super) const NO_APPLICATION: &str = "the browser helper couldn't set up its application \
     (NSApp was already another class, or the helper isn't on its main thread)";

/// Whether this process is one of CEF's subprocesses (renderer, GPU, utility and the
/// rest), which Chromium starts with `--type=<kind>`; the browser process has none.
pub(super) fn is_subprocess() -> bool {
    std::env::args()
        .skip(1)
        .any(|arg| arg.starts_with("--type="))
}

/// CEF's sandbox, entered for the life of a subprocess; nothing in the browser process,
/// which Chromium never sandboxes.
pub(super) struct Sandbox(*mut c_void);

impl Sandbox {
    /// Enters the sandbox when `subprocess` (`cef_scoped_sandbox_initialize`, which loads
    /// `libcef_sandbox.dylib` from the framework). `None` when it can't: the process must
    /// then exit, as Chromium expects every subprocess it asked to be sandboxed to be.
    pub(super) fn enter(args: &::cef::args::Args, subprocess: bool) -> Option<Self> {
        if !subprocess {
            return Some(Self(std::ptr::null_mut()));
        }
        let main = args.as_main_args();
        let context = ::cef::scoped_sandbox_initialize(main.argc, main.argv);
        (!context.is_null()).then_some(Self(context))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            ::cef::scoped_sandbox_destroy(self.0.cast());
        }
    }
}

/// CEF's framework, loaded for the life of the process (`cef_scoped_library_loader_create`).
pub(super) struct Framework(*mut c_void);

impl Framework {
    /// Loads the framework where the bundle keeps it: three folders up from a helper
    /// app's executable when `subprocess`, else in `../Frameworks` from the browser
    /// process's. Must come before any other CEF call.
    ///
    /// # Errors
    /// What to tell the engine when it isn't there or won't load.
    pub(super) fn load(subprocess: bool) -> Result<Self, String> {
        let loader = ::cef::scoped_library_loader_create(i32::from(subprocess));
        if loader.is_null() {
            return Err(format!(
                "CEF's framework didn't load: JStream.app must hold {FRAMEWORK} in \
                 Contents/Frameworks, as scripts/mac/bundle-app.sh lays it out \
                 (docs/adr/0157-browser-sources-on-macos.md)"
            ));
        }
        Ok(Self(loader))
    }
}

impl Drop for Framework {
    fn drop(&mut self) {
        ::cef::scoped_library_loader_free(self.0.cast());
    }
}

/// `settings` with the app bundle, CEF's framework and the plain helper app named, when
/// the helper runs from `JStream.app/Contents/MacOS`. Run from anywhere else (a build
/// folder), they are left to CEF's defaults.
pub(super) fn bundle_settings(settings: ::cef::Settings) -> ::cef::Settings {
    let Some(bundle) = std::env::current_exe()
        .ok()
        .and_then(|exe| app_bundle(&exe))
    else {
        return settings;
    };
    let paths = BundlePaths::of(&bundle);
    let path = |p: &Path| -> ::cef::CefString { p.display().to_string().as_str().into() };
    ::cef::Settings {
        main_bundle_path: path(&bundle),
        framework_dir_path: path(&paths.framework),
        browser_subprocess_path: path(&paths.helper),
        ..settings
    }
}

/// The app bundle that holds `exe` in its `Contents/MacOS`, if one does.
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_app = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    is_app.then(|| bundle.to_path_buf())
}

/// Where CEF's parts are in an app bundle.
#[derive(Debug, PartialEq, Eq)]
struct BundlePaths {
    framework: PathBuf,
    /// The plain helper app's executable.
    helper: PathBuf,
}

impl BundlePaths {
    fn of(bundle: &Path) -> Self {
        let frameworks = bundle.join("Contents").join("Frameworks");
        Self {
            framework: frameworks.join(FRAMEWORK),
            helper: frameworks
                .join(format!("{HELPER_APP}.app"))
                .join("Contents")
                .join("MacOS")
                .join(HELPER_APP),
        }
    }
}

/// Whether `NSApp` is inside `sendEvent:` now: Chromium asks (`isHandlingSendEvent`)
/// and sets it (`setHandlingSendEvent:`). One application per process, so one flag.
static HANDLING_SEND_EVENT: AtomicBool = AtomicBool::new(false);

define_class!(
    /// The browser process's `NSApplication`: AppKit's own, plus the protocols CEF
    /// requires `NSApp` to speak on macOS (`CefAppProtocol` and the Chromium protocols
    /// under it), which track whether it is dispatching an event.
    // SAFETY: `NSApplication` may be subclassed; this class adds no instance variables
    // and no `Drop`, and its one override, `sendEvent:`, keeps AppKit's signature and
    // calls AppKit's own. The class is only used on the main thread, as AppKit requires.
    #[unsafe(super(NSApplication, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "JStreamBrowserHelperApplication"]
    pub(super) struct HelperApplication;

    impl HelperApplication {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            let was = HANDLING_SEND_EVENT.swap(true, Ordering::Relaxed);
            // SAFETY: AppKit's own `sendEvent:`, on this object, with the event AppKit
            // gave this override.
            let () = unsafe { msg_send![super(self), sendEvent: event] };
            HANDLING_SEND_EVENT.store(was, Ordering::Relaxed);
        }
    }

    // SAFETY: `isHandlingSendEvent` returns a `BOOL`, as the protocol declares.
    unsafe impl CrAppProtocol for HelperApplication {
        #[unsafe(method(isHandlingSendEvent))]
        fn is_handling_send_event(&self) -> Bool {
            Bool::new(HANDLING_SEND_EVENT.load(Ordering::Relaxed))
        }
    }

    // SAFETY: `setHandlingSendEvent:` takes a `BOOL` and returns nothing, as the
    // protocol declares.
    unsafe impl CrAppControlProtocol for HelperApplication {
        #[unsafe(method(setHandlingSendEvent:))]
        fn set_handling_send_event(&self, handling: Bool) {
            HANDLING_SEND_EVENT.store(handling.as_bool(), Ordering::Relaxed);
        }
    }

    // SAFETY: the protocol adds no methods of its own.
    unsafe impl CefAppProtocol for HelperApplication {}
);

/// Makes `NSApp` a [`HelperApplication`], with no Dock icon and no menu bar, before CEF
/// starts. `false` when it can't be: off the main thread, or `NSApp` already exists as
/// another class. Must come after the framework has loaded, as the protocols the class
/// speaks are the framework's.
pub(super) fn become_shared_application() -> bool {
    if MainThreadMarker::new().is_none() {
        return false;
    }
    // SAFETY: `sharedApplication` is `NSApplication`'s class method; sent to the
    // subclass on the main thread before any `NSApp` exists, it creates one of the
    // subclass and returns it, retained by AppKit for the life of the process.
    let app: Retained<NSApplication> =
        unsafe { msg_send![HelperApplication::class(), sharedApplication] };
    let shared: &AnyObject = &app;
    if shared.class() != HelperApplication::class() {
        return false;
    }
    // The helper never shows a window: no Dock icon, no menu bar, never activated.
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_found_only_from_its_macos_folder() {
        let app = Path::new("/Applications/JStream.app");
        assert_eq!(
            app_bundle(&app.join("Contents/MacOS/jstream-browser-helper")).as_deref(),
            Some(app)
        );
        assert_eq!(
            app_bundle(Path::new("/tmp/build/jstream-browser-helper")),
            None
        );
        assert_eq!(
            app_bundle(Path::new(
                "/tmp/JStream/Contents/MacOS/jstream-browser-helper"
            )),
            None
        );
    }

    #[test]
    fn cefs_parts_are_in_the_bundles_frameworks_folder() {
        let paths = BundlePaths::of(Path::new("/Applications/JStream.app"));
        assert_eq!(
            paths.framework,
            Path::new(
                "/Applications/JStream.app/Contents/Frameworks/Chromium Embedded Framework.framework"
            )
        );
        assert_eq!(
            paths.helper,
            Path::new(
                "/Applications/JStream.app/Contents/Frameworks/JStream Helper.app/Contents/MacOS/JStream Helper"
            )
        );
    }
}
