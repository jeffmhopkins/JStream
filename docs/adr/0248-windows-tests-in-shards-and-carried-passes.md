<!-- @doc
id = "ADR-0248"
title = "The Windows tests run in shards, and a Windows pass carries to a head with the same build inputs"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0248: The Windows tests run in shards, and a Windows pass carries to a head with the same build inputs

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0054](0054-platform-ci-after-review.md): its `test (windows-latest)` was one
job, and the gate skipped a dispatch only when that job was queued, running or passed.

<a id="context"></a>
## Context
`test (windows-latest)` took about 72 minutes for each PR head, and every merge waits on
it. Three things made it slow:
- **The tests run one GPU test at a time.** WARP crashes when one process opens several
  Dx12 devices at once, so `jstream-render` and `jstream-core` serialize their GPU tests
  on a lock in each test binary, and `cargo test` runs one binary at a time. Those two
  binaries took 22 and 20 of the job's 52 minutes of tests.
- **The build never hit a cache.** A merge lands on `main` about every 20 minutes, and
  each push to `main` cancelled the after-review run before it, so `main` rarely finished
  one and saved a cache. Every PR branch saved its own cache of more than a gigabyte, which
  pushed the rest out of the repository's quota. The build took 16 minutes from scratch.
- **Heads that change nothing it builds ran it again.** A restamp, or another head that
  only touches the docs, gets a new review pass and a full new run.

<a id="decision"></a>
## Decision
- **One build, four test shards.** `test-build` builds the workspace's tests once with
  [cargo-nextest](https://github.com/nextest-rs/nextest), packs them into an archive, and
  runs the doctests (which nextest doesn't run) and the browser engine side's `cef` tests.
  Four `test-shard` runners unpack the archive into their own checkout's `target` and each
  run a quarter of the tests (`--partition count:N/4`). `test (windows-latest)`, the
  required check, passes when the build and every shard did. nextest is the project's
  release binary, pinned and checked against its SHA-256, as trunk is.
- **One GPU test at a time on each runner.** nextest runs each test in its own process, so
  the locks in the test binaries no longer order the GPU tests. A nextest test group with
  one thread does, over every test of the packages that open compositors
  (`.config/nextest.toml`). Across the shards, four run at once, each on its own runner.
- **Only `main` saves the Windows `clippy` and test build caches**, and runs on `main` queue
  rather than cancel each other. A PR branch's run restores `main`'s cache.
- **A Windows pass carries to a head with the same build inputs.** The inputs are every
  file of the head's tree but `docs/` (except `docs/facts.toml`, which a test includes)
  and the Markdown files at the root, hashed by `.github/after-review-inputs.sh`. When
  `clippy (windows-latest)` or `test (windows-latest)` passes, the run leaves a marker under
  that hash in the Actions cache. A later run on the branch, or on any branch for a marker
  from `main`, finds it, skips that job, and reports the pass to the PR, saying it passed
  at an earlier head with the same build inputs.
- **The gate doesn't dispatch while an after-review run for the head is queued or
  running.** `test (windows-latest)` starts only after the shards, so its check run no
  longer shows a run in flight, and a second dispatch would cancel it.

<a id="consequences"></a>
## Consequences
- The Windows tests take about the build plus a quarter of the tests' time, rather than
  the whole of both, and reach the merge box sooner. A restamp-only head, or one that only
  changes the docs, gets its Windows `clippy` and `test` at once.
- Each run uses four more Windows runners for its shards, but for less time. A run that
  restores a cache builds in minutes.
- nextest doesn't build the examples, so `cargo test`'s check that they link on Windows is
  gone. The Windows `clippy --all-targets` still type-checks them.
- A docs change that breaks a test reading the docs (`xtask`'s check of the repository's
  own docs) is caught by the Linux `docs` job and `test`, not by the Windows ones.
- A carried pass trusts that the runner image hasn't changed in a way that breaks the
  tests since the pass. A marker that isn't read for seven days leaves the cache.
- A test that relied on another test running before it in the same process would now
  fail, and a test that set process state for the next one can no longer leak it.
