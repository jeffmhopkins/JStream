//! The built-in source kinds' descriptors, snapshot-tested (ADR-0025, "Kinds get their
//! own snapshot"): adding or changing a kind changes no protocol schema, so this diff is
//! where a kind's settings are reviewed like an API. Regenerate it with
//! `INSTA_UPDATE=always cargo test -p jstream-sources --test kinds`.

use jstream_sources::Registry;

#[test]
fn builtin_kinds_snapshot() {
    let kinds = serde_json::to_string_pretty(&Registry::builtin().descriptors())
        .expect("descriptors serialize");
    insta::with_settings!({
        snapshot_path => "../schema",
        prepend_module_to_snapshot => false,
        omit_expression => true,
    }, {
        insta::assert_snapshot!("kinds", kinds);
    });
}
