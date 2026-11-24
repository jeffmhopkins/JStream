//! The window's zoom, for the UI's scale (`jstream-ui`'s `ui_scale.rs`): `set_zoom` zooms
//! the main window's webview, as a browser's own zoom does, so the device pixel ratio
//! follows and the preview stays sharp. The UI keeps the scale in its own storage and asks
//! again at each start; the app keeps nothing.
//!
//! The page reaches it through `setZoom` on `__JSTREAM_DESKTOP__` (`bootstrap.rs`), and
//! the `zoom` capability grants it to the main window only.
//!
//! Docs: apps/jstream-desktop/README.md#design

/// The smallest zoom the page may ask for: the UI scale's smallest step, 50 %.
pub const MIN: f64 = 0.5;
/// The largest: 150 %.
pub const MAX: f64 = 1.5;

/// `factor` within [`MIN`] and [`MAX`]; 1 when it isn't a number.
pub fn bounded(factor: f64) -> f64 {
    if factor.is_finite() {
        factor.clamp(MIN, MAX)
    } else {
        1.0
    }
}

/// `set_zoom`: zooms the calling window's webview to `factor`, bounded. Answers whether
/// the webview took it.
#[cfg(webview)]
#[tauri::command]
pub(crate) fn set_zoom(webview: tauri::Webview, factor: f64) -> bool {
    match webview.set_zoom(bounded(factor)) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "could not zoom the window");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::bounded;

    #[test]
    fn the_zoom_stays_within_the_ui_scales_steps() {
        assert_eq!(bounded(1.25), 1.25);
        assert_eq!(bounded(0.1), 0.5);
        assert_eq!(bounded(4.0), 1.5);
        assert_eq!(bounded(f64::NAN), 1.0);
        assert_eq!(bounded(f64::INFINITY), 1.0);
    }
}
