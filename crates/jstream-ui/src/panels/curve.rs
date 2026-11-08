//! A custom easing curve's four points (ADR-0129): number fields for `x1`, `y1`, `x2` and
//! `y2`, and a small drawing of the curve. The preset form and the Inspector share it.

use jstream_control::protocol::{Bezier, Finite32, MAX_BEZIER_OVERSHOOT};
use leptos::prelude::*;

use super::number::NumberField;

/// One of a curve's four numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// The first control point's time.
    X1,
    /// The first control point's progress.
    Y1,
    /// The second control point's time.
    X2,
    /// The second control point's progress.
    Y2,
}

impl Point {
    /// The four, in the order the fields show them.
    pub const ALL: [Point; 4] = [Point::X1, Point::Y1, Point::X2, Point::Y2];

    /// The field's label: the number's protocol name.
    pub fn label(self) -> &'static str {
        match self {
            Point::X1 => "x1",
            Point::Y1 => "y1",
            Point::X2 => "x2",
            Point::Y2 => "y2",
        }
    }

    /// The range the engine takes: a time in `[0, 1]`, a progress in
    /// `[-2, 3]` ([`MAX_BEZIER_OVERSHOOT`]).
    pub fn range(self) -> (f64, f64) {
        let over = f64::from(MAX_BEZIER_OVERSHOOT);
        match self {
            Point::X1 | Point::X2 => (0.0, 1.0),
            Point::Y1 | Point::Y2 => (-over, 1.0 + over),
        }
    }

    /// Its value in `curve`.
    pub fn of(self, curve: &Bezier) -> f64 {
        f64::from(
            match self {
                Point::X1 => curve.x1,
                Point::Y1 => curve.y1,
                Point::X2 => curve.x2,
                Point::Y2 => curve.y2,
            }
            .get(),
        )
    }
}

/// `curve` with `point` set to `value`, kept to its range and rounded to two places, or
/// `None` when nothing changes or the value isn't a number.
pub fn with_point(curve: &Bezier, point: Point, value: f64) -> Option<Bezier> {
    let (min, max) = point.range();
    let rounded = (value.clamp(min, max) * 100.0).round() / 100.0;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "kept to the curve's small range first"
    )]
    let v = Finite32::new(rounded as f32)?;
    let mut next = *curve;
    match point {
        Point::X1 => next.x1 = v,
        Point::Y1 => next.y1 = v,
        Point::X2 => next.x2 = v,
        Point::Y2 => next.y2 = v,
    }
    (next != *curve).then_some(next)
}

/// The drawing's width and height, in its own units. Progress 0 to 1 takes the middle
/// half of the height, so a curve that overshoots by up to half either way still shows.
const SIZE: f64 = 100.0;

/// Where progress `y` sits on the drawing, measured down from its top.
fn y_at(y: f64) -> f64 {
    SIZE * 0.75 - y * SIZE * 0.5
}

/// The SVG path of `curve` from (0, 0) to (1, 1), on a [`SIZE`]-unit square.
pub fn curve_path(curve: &Bezier) -> String {
    let x = |t: f64| t * SIZE;
    format!(
        "M {} {} C {} {} {} {} {} {}",
        x(0.0),
        y_at(0.0),
        x(Point::X1.of(curve)),
        y_at(Point::Y1.of(curve)),
        x(Point::X2.of(curve)),
        y_at(Point::Y2.of(curve)),
        x(1.0),
        y_at(1.0),
    )
}

/// The drawing's `viewBox`: the [`SIZE`]-unit square, grown up or down to take in a
/// handle that overshoots past it, so the whole curve shows.
pub fn view_box(curve: &Bezier) -> String {
    let ys = [y_at(Point::Y1.of(curve)), y_at(Point::Y2.of(curve))];
    let top = ys.iter().copied().fold(0.0, f64::min);
    let bottom = ys.iter().copied().fold(SIZE, f64::max);
    format!("0 {top} {SIZE} {}", bottom - top)
}

/// A custom curve's points: a drawing of the curve with its control handles, then the
/// four fields. `curve` is the curve shown, `None` while there is none; `send` gets each
/// changed curve and says whether it sent it.
#[component]
pub fn CurveEditor(
    /// The curve shown.
    #[prop(into)]
    curve: Signal<Option<Bezier>>,
    /// Takes a changed curve; returns whether it was sent.
    send: impl Fn(Bezier) -> bool + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let path = move || curve.get().map(|c| curve_path(&c)).unwrap_or_default();
    let frame = move || {
        curve
            .get()
            .map_or_else(|| format!("0 0 {SIZE} {SIZE}"), |c| view_box(&c))
    };
    let handle = move |time: Point, progress: Point, from: f64| {
        move || {
            curve
                .get()
                .map(|c| {
                    format!(
                        "M {} {} L {} {}",
                        from * SIZE,
                        y_at(from),
                        time.of(&c) * SIZE,
                        y_at(progress.of(&c))
                    )
                })
                .unwrap_or_default()
        }
    };
    let field = move |point: Point| {
        let (min, max) = point.range();
        let value = Signal::derive(move || curve.get().map(|c| point.of(&c)));
        let send_point = move |v: f64| {
            curve
                .get_untracked()
                .and_then(|c| with_point(&c, point, v))
                .is_some_and(send)
        };
        view! {
            <NumberField label=point.label() value=value step=0.05 decimals=2 min=min max=max
                send=send_point />
        }
    };
    view! {
        <div class="curve-editor">
            <svg class="curve-drawing" viewBox=frame role="img"
                aria-label="The custom curve">
                <line class="curve-guide" x1="0" y1="75" x2="100" y2="75" />
                <line class="curve-guide" x1="0" y1="25" x2="100" y2="25" />
                <path class="curve-handle" d=handle(Point::X1, Point::Y1, 0.0) />
                <path class="curve-handle" d=handle(Point::X2, Point::Y2, 1.0) />
                <path class="curve-line" d=path />
            </svg>
            <div class="field-grid">
                {Point::ALL.into_iter().map(field).collect_view()}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f32) -> Finite32 {
        Finite32::new(v).expect("a number")
    }

    /// A point is kept to its range and rounded, and an unchanged one sends nothing.
    #[test]
    fn a_point_is_kept_to_its_range() {
        let c = Bezier::BACK_OUT;
        let next = with_point(&c, Point::X1, 1.7).expect("a change");
        assert_eq!(next.x1, f(1.0));
        let next = with_point(&c, Point::Y2, -9.0).expect("a change");
        assert_eq!(next.y2, f(-2.0));
        let next = with_point(&c, Point::Y1, 2.345).expect("a change");
        assert_eq!(next.y1, f(2.35));
        assert_eq!(with_point(&c, Point::X2, Point::X2.of(&c)), None);
        assert_eq!(with_point(&c, Point::X2, f64::NAN), None);
    }

    /// The drawing runs from the bottom left to the top right through the handles.
    #[test]
    fn the_path_runs_through_the_handles() {
        let c = Bezier {
            x1: f(0.25),
            y1: f(0.0),
            x2: f(0.75),
            y2: f(1.0),
        };
        assert_eq!(curve_path(&c), "M 0 75 C 25 75 75 25 100 25");
    }

    /// The drawing grows to take in a handle past its square, and keeps the square
    /// otherwise.
    #[test]
    fn the_drawing_takes_in_an_overshoot() {
        let inside = Bezier {
            x1: f(0.25),
            y1: f(0.0),
            x2: f(0.75),
            y2: f(1.0),
        };
        assert_eq!(view_box(&inside), "0 0 100 100");
        let over = Bezier {
            y1: f(3.0),
            y2: f(-2.0),
            ..inside
        };
        // y 3 sits at 75 − 150 = −75, y −2 at 75 + 100 = 175.
        assert_eq!(view_box(&over), "0 -75 100 250");
    }
}
