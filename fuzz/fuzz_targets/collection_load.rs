//! The collection file loader: a saved collection is user-editable JSON on disk.
//!
//! Loading must never panic. A file that loads must save, and the saved file must load
//! back to the same state and revision.

#![no_main]

use jstream_graph::Graph;
use jstream_sources::Registry;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let empty = Graph::new().with_kinds(Registry::builtin().descriptors());
    let Ok(graph) = empty.clone().with_collection_json(text) else {
        return;
    };
    let saved = graph.to_collection_json();
    let again = empty
        .with_collection_json(&saved)
        .unwrap_or_else(|e| panic!("a saved collection doesn't load: {e}\n{saved}"));
    assert_eq!(again.state(), graph.state(), "round trip of\n{saved}");
    assert_eq!(again.rev(), graph.rev());
});
