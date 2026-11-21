use super::{Held, NUM_REF_FRAMES, SURFACES, SURFACES_WITH_GRAIN, free_surface};

#[test]
fn a_frame_never_decodes_into_a_surface_a_slot_holds() {
    // Eight different frames without grain: two surfaces stay free.
    let slots: Vec<Option<Held>> = (0..NUM_REF_FRAMES)
        .map(|i| {
            Some(Held {
                decoded: i,
                shown: i,
            })
        })
        .collect();
    assert_eq!(free_surface(&slots, SURFACES, None), 8);
    assert_eq!(free_surface(&slots, SURFACES, Some(8)), 9);
}

#[test]
fn with_film_grain_a_frame_still_finds_two_free_surfaces() {
    // Eight different frames, each shown from a second surface.
    let slots: Vec<Option<Held>> = (0..NUM_REF_FRAMES)
        .map(|i| {
            Some(Held {
                decoded: 2 * i,
                shown: 2 * i + 1,
            })
        })
        .collect();
    let decoded = free_surface(&slots, SURFACES_WITH_GRAIN, None);
    let shown = free_surface(&slots, SURFACES_WITH_GRAIN, Some(decoded));
    assert_eq!((decoded, shown), (16, 17));
}

#[test]
fn a_frame_in_several_slots_holds_its_surfaces_once() {
    let held = Some(Held {
        decoded: 3,
        shown: 4,
    });
    let slots = [held; NUM_REF_FRAMES];
    assert_eq!(free_surface(&slots, SURFACES_WITH_GRAIN, None), 0);
    assert_eq!(free_surface(&slots, SURFACES_WITH_GRAIN, Some(0)), 1);
    assert_eq!(free_surface(&slots, SURFACES_WITH_GRAIN, Some(2)), 0);
}
