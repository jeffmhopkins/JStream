# CLAUDE.md

Instructions for AI agents working in this repo. Humans: this is a good summary too.

## Read first
- `docs/ARCHITECTURE.md`: crate map and layering. Know which layer you're in before you edit.
- `docs/RULES.md`: the rules. Everything below is a reminder, not a replacement.
- `docs/adr/`: check before proposing a design change. If you contradict an accepted ADR, write a new ADR.

## Non-negotiables
1. New capability → command/query/event in `jstream-protocol` **first**, then implement it in core,
   then expose it (MCP tools are generated, so no hand-written MCP plumbing per command).
2. `unsafe` only in `jstream-platform-*` / `*-sys` crates, always with `// SAFETY:`.
3. No allocation, locks, blocking or `.await` on the audio/render/encode threads.
4. No FFmpeg, no GPL/LGPL dependencies. Check `deny.toml` before adding a crate.
5. `jstream-ui` never depends on `jstream-core`.
6. Secrets never in logs, files, events or MCP responses.
7. No `unwrap()` in non-test code.

## Before you push
```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo xtask lint-deps
```
If you changed the protocol: `cargo insta review` and include the schema diff in the PR.
If you changed rendering: `cargo xtask golden` and describe any image changes.

## Working style
- Small PRs, one logical change, Conventional Commit title with the crate as scope.
- Don't add a crate when a module will do. Don't add a dependency without saying why.
- Update `docs/ARCHITECTURE.md` in the same PR if your change makes it wrong.
- Platform work you can't run on this machine (e.g. Windows APIs from Linux): make sure it
  compiles for the target (`cargo check --target x86_64-pc-windows-msvc` where possible),
  put hardware tests behind `hw-tests`, and say clearly in the PR what was not run.
