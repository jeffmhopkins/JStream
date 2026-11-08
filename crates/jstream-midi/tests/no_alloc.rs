//! Parsing messages and tracking the tempo allocate nothing, so a MIDI input thread can
//! run them on every event without touching the allocator.
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator, which
//! counts every allocation made inside an `assert_no_alloc` scope.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, violation_count};
use jstream_midi::{Message, Tempo};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn parsing_and_the_tempo_do_not_allocate() {
    // The counter works: an allocation inside a scope is seen. (One test, so no other
    // test's scope can move the global counter while this one measures.)
    let before = violation_count();
    let v = assert_no_alloc(|| std::hint::black_box(vec![0u8; 64]));
    assert!(violation_count() > before);
    drop(v);

    let mut tempo = Tempo::new(4);
    let before = violation_count();
    assert_no_alloc(|| {
        tempo.apply(&Message::Start, 0);
        for i in 0..1_000_u64 {
            let at = i * 20_833_333;
            if let Ok(m) = Message::parse(&[0xF8]) {
                tempo.apply(&m, at);
            }
            let _ = std::hint::black_box(Message::parse(&[0x90, (i % 128) as u8, 100]));
            let _ = std::hint::black_box(tempo.at(at + 1));
            let _ = std::hint::black_box(tempo.next_bar_at(at + 1));
        }
    });
    assert_eq!(violation_count(), before);
}
