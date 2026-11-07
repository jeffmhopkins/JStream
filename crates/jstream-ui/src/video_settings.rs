//! File › Video settings…: the collection's base resolution (the canvas every scene is
//! laid out on) and its base frame rate, as OBS's Video settings set them, and the space
//! its blend modes mix in (docs/UI-FEATURES.md#header).
//!
//! All three are collection state, so the dialog reads them from `state.get` and
//! applies what changed as one batch: one undo step, `canvas.set_size`,
//! `canvas.set_frame_rate` (ADR-0119) and `canvas.set_blending` (ADR-0126). Encoders scale after this picture: an output's
//! video mix has its own resolution and divides the base rate. A program stage that is
//! running keeps the rate it opened with, so the dialog says the new rate applies once
//! every output has stopped.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::store::Studio;
use jstream_control::protocol::{
    Batch, Blending, Call, Canvas, DEFAULT_BASE_FPS, Edit, FAST_BASE_FPS,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The largest canvas side the engine takes, in pixels.
pub const MAX_SIDE: u32 = 8192;

/// The base resolutions offered first: landscape HD to UHD, then vertical.
pub const PRESETS: [(u32, u32); 6] = [
    (1280, 720),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (1080, 1920),
    (1080, 1080),
];

/// The rates the engine runs at (ADR-0119).
pub const RATES: [u32; 2] = [DEFAULT_BASE_FPS, FAST_BASE_FPS];

/// A side typed into the dialog, or why it can't be used.
pub fn side(text: &str) -> Result<u32, String> {
    match text.trim().parse::<u32>() {
        Ok(value) if (1..=MAX_SIDE).contains(&value) => Ok(value),
        _ => Err(format!("Enter a whole number from 1 to {MAX_SIDE}.")),
    }
}

/// What the dialog sets: the base resolution, the base rate and the blending space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Video {
    /// The canvas.
    pub canvas: Canvas,
    /// Frames per second.
    pub fps: u32,
    /// The space blend modes mix in.
    pub blending: Blending,
}

impl Default for Video {
    fn default() -> Self {
        Self {
            canvas: Canvas::DEFAULT,
            fps: DEFAULT_BASE_FPS,
            blending: Blending::default(),
        }
    }
}

/// How each blending space reads in the picker.
pub fn blending_label(blending: Blending) -> &'static str {
    match blending {
        Blending::Linear => "Linear light",
        _ => "Browser-style (sRGB)",
    }
}

/// The edits that take the collection from `now` to `wanted`: only what changed, so
/// applying the same values sends nothing.
pub fn edits(now: Video, wanted: Video) -> Vec<Edit> {
    let mut edits = Vec::new();
    if now.canvas != wanted.canvas {
        edits.push(Edit::CanvasSetSize {
            width: wanted.canvas.width,
            height: wanted.canvas.height,
        });
    }
    if now.fps != wanted.fps {
        edits.push(Edit::CanvasSetFrameRate { fps: wanted.fps });
    }
    if now.blending != wanted.blending {
        edits.push(Edit::CanvasSetBlending {
            blending: wanted.blending,
        });
    }
    edits
}

/// How a preset or a size reads in the picker: `1920 × 1080 (16:9)`.
pub fn label(width: u32, height: u32) -> String {
    let (mut a, mut b) = (width, height);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let gcd = a.max(1);
    format!("{width} × {height} ({}:{})", width / gcd, height / gcd)
}

/// The dialog. It reads the collection as it opens and applies on Apply.
#[component]
pub fn VideoSettingsDialog(
    /// Closes the dialog.
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let studio = expect_context::<Studio>();
    let read = move || {
        studio.collection.with_untracked(|c| {
            c.as_ref().map_or_else(Video::default, |c| Video {
                canvas: c.canvas,
                fps: c.base_fps,
                blending: c.blending,
            })
        })
    };
    let now = read();
    let width = RwSignal::new(now.canvas.width.to_string());
    let height = RwSignal::new(now.canvas.height.to_string());
    let rate = RwSignal::new(now.fps);
    let blending = RwSignal::new(now.blending);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let preset = move || {
        let (w, h) = (side(&width.get()), side(&height.get()));
        PRESETS
            .iter()
            .position(|p| Ok(p.0) == w && Ok(p.1) == h)
            .map_or_else(|| "custom".to_owned(), |i| i.to_string())
    };
    let apply = move || {
        let (w, h) = match (side(&width.get_untracked()), side(&height.get_untracked())) {
            (Ok(w), Ok(h)) => (w, h),
            (Err(why), _) | (_, Err(why)) => {
                error.set(Some(why));
                return;
            }
        };
        let wanted = Video {
            canvas: Canvas {
                width: w,
                height: h,
            },
            fps: rate.get_untracked(),
            blending: blending.get_untracked(),
        };
        let edits = edits(read(), wanted);
        if edits.is_empty() {
            close();
            return;
        }
        let Some(session) = session.get() else {
            error.set(Some("Not connected to an engine.".into()));
            return;
        };
        busy.set(true);
        spawn_local(async move {
            let call = Call::Batch(Batch {
                edits,
                expected_rev: None,
                idempotency_key: None,
                ends_gesture: None,
            });
            let sent = session.control.call(call).await;
            busy.set(false);
            match sent {
                Ok(_) => close(),
                Err(err) => error.set(Some(err.to_string())),
            }
        });
    };
    view! {
        <Modal label="Video settings" class="settings-dialog video-settings" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Video settings"</h2>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>"×"</button>
            </div>
            <div class="settings-body">
                <label class="settings-field">
                    <span class="settings-check-label">"Base (canvas) resolution"</span>
                    <select
                        aria-label="Base resolution"
                        prop:value=preset
                        on:change=move |event| {
                            if let Some(&(w, h)) = event_target_value(&event)
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| PRESETS.get(i))
                            {
                                width.set(w.to_string());
                                height.set(h.to_string());
                                error.set(None);
                            }
                        }
                    >
                        {PRESETS.iter().enumerate().map(|(i, &(w, h))| view! {
                            <option value=i.to_string()>{label(w, h)}</option>
                        }).collect_view()}
                        <option value="custom">"Custom"</option>
                    </select>
                </label>
                <div class="settings-size">
                    <input
                        type="number"
                        min="1"
                        max=MAX_SIDE.to_string()
                        aria-label="Width"
                        prop:value=move || width.get()
                        on:input=move |event| {
                            width.set(event_target_value(&event));
                            error.set(None);
                        }
                    />
                    <span>"×"</span>
                    <input
                        type="number"
                        min="1"
                        max=MAX_SIDE.to_string()
                        aria-label="Height"
                        prop:value=move || height.get()
                        on:input=move |event| {
                            height.set(event_target_value(&event));
                            error.set(None);
                        }
                    />
                </div>
                <p class="settings-check-note">
                    "Every scene is laid out on this canvas, and the program and preview show it at this size. Items keep their pixel positions when it changes. Each output's video mix scales from it."
                </p>
                <label class="settings-field">
                    <span class="settings-check-label">"Frame rate"</span>
                    <select
                        aria-label="Frame rate"
                        prop:value=move || rate.get().to_string()
                        on:change=move |event| {
                            if let Ok(fps) = event_target_value(&event).parse::<u32>() {
                                rate.set(fps);
                            }
                        }
                    >
                        {RATES.iter().map(|fps| view! {
                            <option value=fps.to_string()>{format!("{fps} fps")}</option>
                        }).collect_view()}
                    </select>
                </label>
                <p class="settings-check-note">
                    "Outputs run at this rate, or at a share of it set by their video mix. 60 fps doubles every mix's rate and the work per second. While an output is live, a new rate applies once every output has stopped."
                </p>
                <label class="settings-field">
                    <span class="settings-check-label">"Blending"</span>
                    <select
                        aria-label="Blending"
                        prop:value=move || blending.get().as_str()
                        on:change=move |event| {
                            if let Some(space) = Blending::from_name(&event_target_value(&event)) {
                                blending.set(space);
                            }
                        }
                    >
                        {Blending::ALL.iter().map(|space| view! {
                            <option value=space.as_str()>{blending_label(*space)}</option>
                        }).collect_view()}
                    </select>
                </label>
                <p class="settings-check-note">
                    "How every blend mode mixes colours, in every scene. Browser-style matches CSS and design tools, so a design made there keeps its look. Linear light mixes light as it adds, for even glows and fades, but makes a faint veil brighter than a browser draws it."
                </p>
                {move || error.get().map(|why| view! { <p class="settings-error" role="alert">{why}</p> })}
            </div>
            <div class="modal-footer">
                <button type="button" on:click=move |_| close()>"Cancel"</button>
                <button
                    type="button"
                    class="primary"
                    data-primary
                    prop:disabled=move || busy.get()
                    on:click=move |_| apply()
                >
                    "Apply"
                </button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_changed_is_sent() {
        let now = Video::default();
        let with = |width, height, fps, blending| Video {
            canvas: Canvas { width, height },
            fps,
            blending,
        };
        assert_eq!(edits(now, now), Vec::<Edit>::new());
        assert_eq!(
            edits(now, with(1920, 1080, 60, Blending::Srgb)),
            vec![Edit::CanvasSetFrameRate { fps: 60 }]
        );
        assert_eq!(
            edits(now, with(1280, 720, 30, Blending::Srgb)),
            vec![Edit::CanvasSetSize {
                width: 1280,
                height: 720
            }]
        );
        assert_eq!(
            edits(now, with(1920, 1080, 30, Blending::Linear)),
            vec![Edit::CanvasSetBlending {
                blending: Blending::Linear
            }]
        );
        assert_eq!(
            edits(
                with(1920, 1080, 60, Blending::Linear),
                with(3840, 2160, 30, Blending::Srgb)
            )
            .len(),
            3
        );
    }

    #[test]
    fn sides_are_checked() {
        assert_eq!(side(" 1920 "), Ok(1920));
        assert_eq!(side("8192"), Ok(MAX_SIDE));
        assert!(side("0").is_err());
        assert!(side("8193").is_err());
        assert!(side("19.5").is_err());
        assert!(side("").is_err());
    }

    #[test]
    fn presets_read_with_their_aspect() {
        assert_eq!(label(1920, 1080), "1920 × 1080 (16:9)");
        assert_eq!(label(1080, 1920), "1080 × 1920 (9:16)");
        assert_eq!(label(1080, 1080), "1080 × 1080 (1:1)");
        for (w, h) in PRESETS {
            assert_eq!(side(&w.to_string()), Ok(w));
            assert_eq!(side(&h.to_string()), Ok(h));
        }
        assert!(RATES.contains(&DEFAULT_BASE_FPS));
    }
}
