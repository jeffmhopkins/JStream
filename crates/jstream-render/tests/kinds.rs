//! The built-in video filter kinds' descriptors, snapshot-tested (ADR-0025, "Kinds get
//! their own snapshot"; #369): adding or changing a kind changes no protocol schema, so
//! this diff is where a filter's settings are reviewed like an API. Regenerate it with
//! `INSTA_UPDATE=always cargo test -p jstream-render --test kinds`.

#[test]
fn video_filter_kinds_snapshot() {
    let kinds = serde_json::to_string_pretty(&jstream_render::video_filter_descriptors())
        .expect("descriptors serialize");
    insta::with_settings!({
        snapshot_path => "../schema",
        prepend_module_to_snapshot => false,
        omit_expression => true,
    }, {
        insta::assert_snapshot!("kinds", kinds);
    });
}
