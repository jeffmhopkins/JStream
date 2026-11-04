//! A page's reload hold in the helper (#1151): from a reload until the reloaded document
//! has finished loading, the helper sends the engine none of the page's paints.
//!
//! The hold used to end when the new document started loading (`OnLoadStart`). But
//! Chromium can paint the new document once before its style applies, and on Linux that
//! first paint came out opaque white over the whole page, `background: transparent` or
//! not: about one reload in ten showed a white frame across the program. So the hold now
//! lasts until the main frame's load ends (`OnLoadEnd`) or fails, by which point the
//! page's own style decides what it paints. A load aborted by another doesn't end it: the
//! load that replaced it does.
//!
//! Docs: crates/jstream-browser/README.md#purpose, docs/adr/0021-browser-source.md

/// Whether a page's paints are held after a reload.
#[derive(Debug, Default)]
pub(crate) struct ReloadHold {
    held: bool,
}

impl ReloadHold {
    /// The page is reloading: its paints are held from now on.
    pub(crate) fn reload(&mut self) {
        self.held = true;
    }

    /// The main frame's load ended or failed: the reloaded document's paints go through.
    pub(crate) fn load_finished(&mut self) {
        self.held = false;
    }

    /// Whether a paint now goes to the engine.
    pub(crate) fn takes_paint(&self) -> bool {
        !self.held
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_that_never_reloaded_paints() {
        assert!(ReloadHold::default().takes_paint());
    }

    #[test]
    fn a_reload_holds_paints_until_the_load_finishes() {
        let mut hold = ReloadHold::default();
        hold.reload();
        // The old document's paints, and the new one's from its load start (#1151):
        // Chromium's first paint of it can be opaque white.
        assert!(!hold.takes_paint());
        hold.load_finished();
        assert!(hold.takes_paint());
    }

    #[test]
    fn a_reload_during_a_reload_holds_until_a_load_finishes() {
        let mut hold = ReloadHold::default();
        hold.reload();
        hold.reload();
        assert!(!hold.takes_paint());
        hold.load_finished();
        assert!(hold.takes_paint());
    }
}
