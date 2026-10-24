//! R-SEC-003: a plugin gets nothing it wasn't granted. No WASI, no filesystem, no
//! network, no imports beyond the `jstream` functions, and no `Control` call without a
//! grant.

use crate::{Capabilities, Error, Plugin, PluginConfig, PluginHost, abi};
use jstream_control::{Control, MockEngine};
use std::sync::Arc;

fn control() -> Arc<dyn Control> {
    Arc::new(MockEngine::new().connect())
}

fn load(wat: &str, config: PluginConfig) -> Result<Plugin, Error> {
    let host = PluginHost::new().expect("the engine configures");
    let wasm = wat::parse_str(wat).expect("valid WAT");
    Plugin::load(&host, wasm, config, control())
}

fn refused_import(wat: &str) -> (String, String) {
    match load(wat, PluginConfig::default()) {
        Err(Error::ForbiddenImport { module, name }) => (module, name),
        Err(other) => panic!("expected a forbidden import, got {other:?}"),
        Ok(_) => panic!("expected a forbidden import, but the plugin loaded"),
    }
}

#[test]
fn wasi_is_not_provided() {
    let wat = r#"(module
        (import "wasi_snapshot_preview1" "fd_write"
            (func (param i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 1))"#;
    assert_eq!(
        refused_import(wat),
        ("wasi_snapshot_preview1".into(), "fd_write".into())
    );
    let wat = r#"(module
        (import "wasi_snapshot_preview1" "path_open"
            (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
        (memory (export "memory") 1))"#;
    assert_eq!(
        refused_import(wat),
        ("wasi_snapshot_preview1".into(), "path_open".into())
    );
    let wat = r#"(module
        (import "wasi:sockets/tcp" "connect" (func))
        (memory (export "memory") 1))"#;
    assert_eq!(
        refused_import(wat),
        ("wasi:sockets/tcp".into(), "connect".into())
    );
}

#[test]
fn only_the_abi_functions_can_be_imported() {
    let wat = r#"(module
        (import "jstream" "open_file" (func))
        (memory (export "memory") 1))"#;
    assert_eq!(refused_import(wat), ("jstream".into(), "open_file".into()));
    // Not even host memory, tables or globals.
    let wat = r#"(module (import "jstream" "memory" (memory 1)))"#;
    assert_eq!(refused_import(wat), ("jstream".into(), "memory".into()));
    let wat = r#"(module
        (import "env" "now" (global i64))
        (memory (export "memory") 1))"#;
    assert_eq!(refused_import(wat), ("env".into(), "now".into()));
}

#[test]
fn every_abi_function_is_provided() {
    // Each import with its real signature; the linker checks the types.
    let wat = r#"(module
        (import "jstream" "call" (func (param i32 i32) (result i32)))
        (import "jstream" "result_len" (func (result i32)))
        (import "jstream" "result_read" (func (param i32)))
        (import "jstream" "input_len" (func (result i32)))
        (import "jstream" "input_read" (func (param i32)))
        (memory (export "memory") 1))"#;
    assert_eq!(abi::IMPORTS.len(), 5);
    load(wat, PluginConfig::default()).expect("every ABI import links");
}

#[test]
fn a_plugin_with_no_grants_reaches_nothing() {
    assert_eq!(PluginConfig::default().capabilities, Capabilities::none());
    // `state.get` is the read with the most in it; nothing is granted, so it's denied.
    let call = r#"{"kind":"query","query":{"type":"state.get"}}"#;
    let wat = format!(
        r#"(module
            (import "jstream" "call" (func $call (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "{data}")
            (func (export "run")
                (if (i32.ne (call $call (i32.const 0) (i32.const {len})) (i32.const {denied}))
                    (then unreachable))))"#,
        data = call.replace('"', "\\\""),
        len = call.len(),
        denied = abi::STATUS_DENIED,
    );
    let mut plugin = load(&wat, PluginConfig::default()).expect("the plugin loads");
    plugin
        .invoke("run", &[])
        .expect("the call is denied, not trapped");
}
