//! `cargo xtask perf`: the reusable perf suite (#1140). It builds the daemon and the CLI,
//! starts the daemon on a fresh collection, and for each stress scene under
//! `xtask/perf/` builds it through MCP, shows it, waits, and reads `profile.get`
//! ([ADR-0103](../../../docs/adr/0103-per-item-performance-profile.md)) and the daemon's
//! memory, threads, handles and child processes. Then it removes the scene and goes on.
//! It measures the plain base scene first and last, so a leak shows as growth between
//! the two, prints a table, writes a JSON report and, with `--check`, judges the rows
//! against `xtask/perf/thresholds.toml`. With `--bench <gates>`, it then runs
//! `jstream-cli bench` on the same engine, judged by that gates file, and fails when the
//! bench does.
//!
//! Docs: xtask/perf/README.md#runner

mod assets;
mod client;
mod scene;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use client::{Daemon, Mcp};
use scene::Scene;

const USAGE: &str = "usage: cargo xtask perf [--scene <name>]... [--seconds <n>] [--settle <n>] \
[--canvas <w>x<h>] [--openh264 <library>] [--out <report.json>] [--check [--thresholds <file>]] \
[--bench <gates.json>] [--browser] [--no-build] [--debug] [--list]";

/// The canvas when `--canvas` names none: the size #1140's first results were taken at.
const DEFAULT_CANVAS: (u32, u32) = (1280, 720);
/// The base scene every measurement starts from and returns to.
const BASE: &str = "perf_base";
/// How long a churn cycle shows its scene.
const CYCLE_HOLD: Duration = Duration::from_secs(2);

/// What `perf` was asked to do.
struct Options {
    scenes: Vec<String>,
    seconds: u32,
    settle: u32,
    canvas: (u32, u32),
    openh264: Option<PathBuf>,
    out: Option<PathBuf>,
    check: bool,
    thresholds: Option<PathBuf>,
    /// A `jstream-cli bench` gates file: run the bench after the scenes.
    bench: Option<PathBuf>,
    browser: bool,
    build: bool,
    release: bool,
    list: bool,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut o = Options {
        scenes: Vec::new(),
        seconds: 5,
        settle: 3,
        canvas: DEFAULT_CANVAS,
        openh264: None,
        out: None,
        check: false,
        thresholds: None,
        bench: None,
        browser: false,
        build: true,
        release: true,
        list: false,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || {
            rest.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--scene" => o.scenes.push(value()?),
            "--seconds" => {
                o.seconds = value()?
                    .parse()
                    .ok()
                    .filter(|s| (1..=60).contains(s))
                    .context("--seconds is 1 to 60 (profile.get's window)")?;
            }
            "--settle" => o.settle = value()?.parse().context("--settle is whole seconds")?,
            "--canvas" => {
                let v = value()?;
                o.canvas = v
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .filter(|&(w, h): &(u32, u32)| w >= 64 && h >= 64)
                    .with_context(|| format!("--canvas is <width>x<height>, not `{v}`"))?;
            }
            "--openh264" => o.openh264 = Some(value()?.into()),
            "--out" => o.out = Some(value()?.into()),
            "--check" => o.check = true,
            "--thresholds" => o.thresholds = Some(value()?.into()),
            "--bench" => o.bench = Some(value()?.into()),
            "--browser" => o.browser = true,
            "--no-build" => o.build = false,
            "--debug" => o.release = false,
            "--list" => o.list = true,
            _ => bail!("unknown argument `{arg}`\n{USAGE}"),
        }
    }
    if o.thresholds.is_some() && !o.check {
        bail!("--thresholds needs --check\n{USAGE}");
    }
    Ok(o)
}

/// Whether `name` is picked by one of `patterns`: a name, or a prefix ending in `*`.
fn picked(name: &str, patterns: &[String]) -> bool {
    patterns.is_empty()
        || patterns.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == p,
        })
}

/// Runs `cargo xtask perf`; `Ok(false)` when `--check` found a row over its limits.
pub fn run(args: &[String], root: &Path) -> Result<bool> {
    let o = parse(args)?;
    // The engine refuses a recording path with a `..` segment, which `root` has.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(root);
    let scene_dir = root.join("xtask/perf");
    let canvas = (f64::from(o.canvas.0), f64::from(o.canvas.1));
    let scenes = scene::load_dir(&scene_dir, canvas)?;
    let all = &scenes;
    for p in &o.scenes {
        if !all.iter().any(|s| picked(&s.name, std::slice::from_ref(p))) {
            bail!("no scene matches `{p}`; `cargo xtask perf --list` lists them");
        }
    }
    if o.list {
        let mut text = String::new();
        for s in all {
            let note = if s.needs_browser { " (--browser)" } else { "" };
            let _ = writeln!(text, "{:<24} {}{note}", s.name, s.about);
        }
        std::io::stdout().write_all(text.as_bytes())?;
        return Ok(true);
    }
    let thresholds = if o.check {
        let path = o
            .thresholds
            .clone()
            .unwrap_or_else(|| scene_dir.join("thresholds.toml"));
        Some(Thresholds::load(&path)?)
    } else {
        None
    };
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let profile = if o.release { "release" } else { "debug" };
    if o.build {
        build(root, o.release)?;
    }
    let exe = |name: &str| {
        target
            .join(profile)
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
    };
    let (daemon_bin, cli_bin) = (exe("jstream-daemon"), exe("jstream-cli"));
    for bin in [&daemon_bin, &cli_bin] {
        if !bin.is_file() {
            bail!(
                "{} isn't built (drop --no-build, or build it)",
                bin.display()
            );
        }
    }

    let run_dir = target.join("perf").join("run");
    if run_dir.exists() {
        std::fs::remove_dir_all(&run_dir)
            .with_context(|| format!("clearing {}", run_dir.display()))?;
    }
    std::fs::create_dir_all(&run_dir)?;
    assets::write_all(&run_dir.join(assets::DIR))?;
    let data_home = run_dir.join("data");
    if let Some(library) = &o.openh264 {
        install_openh264(library, &data_home)?;
    }
    let daemon = Daemon::start(&daemon_bin, &run_dir, &data_home)?;
    let mut mcp = Mcp::start(&cli_bin, &daemon, &run_dir.join("token"))?;
    eprintln!(
        "xtask perf: engine at {}, canvas {}x{}, {} s windows after {} s",
        daemon.url, o.canvas.0, o.canvas.1, o.seconds, o.settle
    );

    let mut run = Run {
        mcp: &mut mcp,
        pid: daemon.pid(),
        seconds: o.seconds,
        settle: o.settle,
        rows: Vec::new(),
    };
    run.build_base(o.canvas)?;
    run.record(&run_dir.join("program.mp4"))?;
    run.measure_base("baseline")?;
    for s in scenes.iter().filter(|s| picked(&s.name, &o.scenes)) {
        if s.needs_browser && !o.browser {
            run.rows
                .push(json!({"scene": s.name, "about": s.about, "skipped": "needs --browser"}));
            continue;
        }
        eprintln!("xtask perf: {}", s.name);
        if let Err(e) = run.scene(s) {
            eprintln!("xtask perf: {}: {e:#}", s.name);
            run.rows
                .push(json!({"scene": s.name, "about": s.about, "error": format!("{e:#}")}));
            // Best effort back to a clean engine for the next scene.
            let _ = run.mcp.switch(BASE);
            for target in s.teardown() {
                let _ = run.mcp.delete(&target);
            }
        }
    }
    run.measure_base("baseline_after")?;
    let _ = run
        .mcp
        .output(json!({"type": "output.stop", "output_id": RECORDING}));
    let rows = run.rows;

    std::io::stdout().write_all(table(&rows).as_bytes())?;
    let verdict = thresholds.as_ref().map(|t| t.judge(&rows));
    let out = o
        .out
        .clone()
        .unwrap_or_else(|| target.join("perf").join("report.json"));
    let report = json!({
        "canvas": {"width": o.canvas.0, "height": o.canvas.1},
        "seconds": o.seconds,
        "settle": o.settle,
        "rows": rows,
        "failures": verdict.clone().unwrap_or_default(),
    });
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", out.display()))?;
    eprintln!("xtask perf: report in {}", out.display());
    // After the report is written, so a bench that can't run doesn't lose the scenes'.
    let mut pass = match &o.bench {
        Some(gates) => bench(&cli_bin, &daemon, &run_dir, gates)?,
        None => true,
    };
    if !pass {
        eprintln!(
            "xtask perf: FAIL jstream bench: a gate failed (see {})",
            run_dir.join("bench").join("summary.md").display()
        );
    }
    match verdict {
        None => {}
        Some(failures) if failures.is_empty() => {
            eprintln!("xtask perf: every row within its thresholds");
        }
        Some(failures) => {
            for f in &failures {
                eprintln!("xtask perf: FAIL {f}");
            }
            pass = false;
        }
    }
    Ok(pass)
}

/// Runs `jstream-cli bench` on `daemon`, judged by `gates`, writing to `run_dir/bench`.
/// `Ok(false)` when a gate failed (the CLI's exit code 4).
///
/// # Errors
/// When the bench couldn't run at all.
fn bench(cli: &Path, daemon: &Daemon, run_dir: &Path, gates: &Path) -> Result<bool> {
    eprintln!("xtask perf: jstream bench, gates {}", gates.display());
    let status = Command::new(cli)
        .args(["--url", &daemon.url, "--token-file"])
        .arg(run_dir.join("token"))
        .args(["bench", "--scenario", "all", "--preset", "cut", "--out"])
        .arg(run_dir.join("bench"))
        .arg("--gates")
        .arg(gates)
        .status()
        .context("running jstream-cli bench")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(4) => Ok(false),
        _ => bail!("jstream bench couldn't run ({status})"),
    }
}

/// Builds the daemon and the CLI.
fn build(root: &Path, release: bool) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(root).args([
        "build",
        "--locked",
        "-p",
        "jstream-daemon",
        "-p",
        "jstream-cli",
    ]);
    if release {
        command.arg("--release");
    }
    let status = command.status().context("running cargo build")?;
    if !status.success() {
        bail!("building the daemon and the CLI failed");
    }
    Ok(())
}

/// Puts Cisco's OpenH264 `library` where the daemon, with `data_home` as its
/// `XDG_DATA_HOME`, looks for a downloaded one. The daemon checks it against its pin
/// when it loads it.
fn install_openh264(library: &Path, data_home: &Path) -> Result<()> {
    let name = library
        .file_name()
        .with_context(|| format!("{} names no file", library.display()))?;
    let store = data_home.join("jstream").join("codecs").join("openh264");
    std::fs::create_dir_all(&store)?;
    std::fs::copy(library, store.join(name))
        .with_context(|| format!("copying {}", library.display()))?;
    Ok(())
}

/// The output that records the program, so the program stage runs: it draws only
/// while something takes its frames.
const RECORDING: &str = "perf_recording";

/// One run against a started engine.
struct Run<'a> {
    mcp: &'a mut Mcp,
    pid: u32,
    seconds: u32,
    settle: u32,
    rows: Vec<Value>,
}

impl Run<'_> {
    fn build_base(&mut self, (pw, ph): (u32, u32)) -> Result<()> {
        let (w, h) = (f64::from(pw), f64::from(ph));
        self.mcp.batch(&[
            json!({"type": "canvas.set_size", "width": pw, "height": ph}),
            json!({"type": "scene.create", "scene_id": BASE, "name": BASE}),
            json!({"type": "source.create", "source_id": "perf_base_colour", "name": "perf_base_colour",
                   "kind": {"type": "builtin", "kind": "color"},
                   "settings": {"color": "#203040ff", "width": pw, "height": ph}}),
            json!({"type": "item.add", "scene_id": BASE, "item_id": "perf_base_item",
                   "source_id": "perf_base_colour",
                   "geometry": {"center": {"x": w / 2.0, "y": h / 2.0}, "size": {"width": w, "height": h}}}),
        ])?;
        self.mcp.switch(BASE)
    }

    /// Records the program to `path` for the rest of the run.
    fn record(&mut self, path: &Path) -> Result<()> {
        self.mcp.batch(&[json!({
            "type": "output.create", "output_id": RECORDING,
            "kind": {"type": "record", "container": "mp4", "path": path},
        })])?;
        self.mcp
            .output(json!({"type": "output.start", "output_id": RECORDING}))
            .context(
                "recording the program, which perf needs so the program stage draws; on \
                 Linux without VA-API, pass --openh264 <Cisco's pinned libopenh264 .so>",
            )?;
        Ok(())
    }

    /// Waits out the settle time and a full window, so the window never reaches back
    /// before the switch, then reads the profile and the process.
    fn sample(&mut self) -> Result<(Value, Option<Value>)> {
        thread::sleep(Duration::from_secs(u64::from(self.settle + self.seconds)));
        let profile = self.mcp.profile(self.seconds, 20)?;
        Ok((profile, client::process(self.pid)))
    }

    fn measure_base(&mut self, name: &str) -> Result<()> {
        self.mcp.switch(BASE)?;
        let (profile, process) = self.sample()?;
        self.rows.push(row(
            name,
            "the plain base scene",
            &profile,
            process,
            self.seconds,
        ));
        Ok(())
    }

    fn teardown(&mut self, s: &Scene) -> Result<()> {
        for target in s.teardown() {
            self.mcp.delete(&target)?;
        }
        Ok(())
    }

    fn scene(&mut self, s: &Scene) -> Result<()> {
        match s.cycles {
            None => {
                for chunk in scene::batches(&s.edits) {
                    self.mcp.batch(chunk)?;
                }
                self.mcp.switch(&s.name)?;
                let (profile, process) = self.sample()?;
                self.rows
                    .push(row(&s.name, &s.about, &profile, process, self.seconds));
                self.mcp.switch(BASE)?;
                self.teardown(s)?;
            }
            Some(n) => {
                let mut first = None;
                for cycle in 0..n {
                    for chunk in scene::batches(&s.edits) {
                        self.mcp.batch(chunk)?;
                    }
                    self.mcp.switch(&s.name)?;
                    thread::sleep(CYCLE_HOLD);
                    self.mcp.switch(BASE)?;
                    self.teardown(s)?;
                    if cycle == 0 {
                        thread::sleep(CYCLE_HOLD);
                        first = client::process(self.pid);
                    }
                }
                let (profile, process) = self.sample()?;
                let about = format!("{} (measured on the base scene after)", s.about);
                let mut r = row(&s.name, &about, &profile, process, self.seconds);
                r["process_after_first_cycle"] = first.unwrap_or(Value::Null);
                self.rows.push(r);
            }
        }
        Ok(())
    }
}

/// A report row: the program drawer's numbers, the plan, the process, and the profile.
fn row(name: &str, about: &str, profile: &Value, process: Option<Value>, seconds: u32) -> Value {
    let program = profile["drawers"]
        .as_array()
        .and_then(|d| d.iter().find(|d| d["drawer"] == "program"))
        .cloned()
        .unwrap_or(Value::Null);
    let frames = program["frames"].as_u64().unwrap_or(0);
    let covered = profile["seconds"].as_f64().unwrap_or(f64::from(seconds));
    let fps = if covered > 0.0 {
        frames as f64 / covered
    } else {
        0.0
    };
    json!({
        "scene": name,
        "about": about,
        "fps": (fps * 10.0).round() / 10.0,
        "program": program,
        "plan": profile["plan"],
        "process_cpu_pct": profile["process_cpu_pct"],
        "process": process,
        "profile": profile,
    })
}

/// The printed table, one line per row.
fn table(rows: &[Value]) -> String {
    let mut out = format!(
        "{:<24} {:>6} {:>8} {:>8} {:>5} {:>5} {:>6} {:>5} {:>7} {:>4} {:>4} {:>4}\n",
        "scene",
        "fps",
        "frame_ms",
        "max_ms",
        "over",
        "skip",
        "plans",
        "cpu%",
        "rss_mb",
        "thr",
        "fds",
        "kids"
    );
    let num = |v: &Value, digits: usize| match v.as_f64() {
        Some(x) => format!("{x:.digits$}"),
        None => "-".to_owned(),
    };
    for r in rows {
        let name = r["scene"].as_str().unwrap_or("?");
        if let Some(why) = r
            .get("error")
            .or_else(|| r.get("skipped"))
            .and_then(Value::as_str)
        {
            let short: String = why.chars().take(90).collect();
            let _ = writeln!(out, "{name:<24} {short}");
            continue;
        }
        let p = &r["program"];
        let pr = &r["process"];
        let _ = writeln!(
            out,
            "{name:<24} {:>6} {:>8} {:>8} {:>5} {:>5} {:>6} {:>5} {:>7} {:>4} {:>4} {:>4}",
            num(&r["fps"], 1),
            num(&p["frame_ms"], 2),
            num(&p["frame_max_ms"], 1),
            num(&p["over_budget"], 0),
            num(&p["skipped"], 0),
            num(&r["plan"]["plans"], 0),
            num(&r["process_cpu_pct"], 0),
            num(&pr["rss_mb"], 0),
            num(&pr["threads"], 0),
            num(&pr["handles"], 0),
            num(&pr["children"], 0),
        );
    }
    out
}

/// The limits `--check` judges a run by.
#[derive(Debug, Clone, Default)]
struct Thresholds {
    default: Limits,
    scenes: Vec<(String, Limits)>,
    max_rss_growth_mb: Option<f64>,
    max_thread_growth: Option<f64>,
    max_handle_growth: Option<f64>,
}

/// One row's limits; `None` checks nothing.
#[derive(Debug, Clone, Default)]
struct Limits {
    min_fps: Option<f64>,
    max_frame_ms: Option<f64>,
    max_over_budget_pct: Option<f64>,
}

impl Limits {
    fn read(table: &toml::Table, what: &str) -> Result<Limits> {
        let mut l = Limits::default();
        for (k, v) in table {
            let n = v
                .as_float()
                .or_else(|| v.as_integer().map(|i| i as f64))
                .with_context(|| format!("{what}.{k} is a number"))?;
            match k.as_str() {
                "min_fps" => l.min_fps = Some(n),
                "max_frame_ms" => l.max_frame_ms = Some(n),
                "max_over_budget_pct" => l.max_over_budget_pct = Some(n),
                _ => bail!("{what}: unknown limit `{k}`"),
            }
        }
        Ok(l)
    }

    /// `self` with `over`'s limits on top.
    fn with(&self, over: &Limits) -> Limits {
        Limits {
            min_fps: over.min_fps.or(self.min_fps),
            max_frame_ms: over.max_frame_ms.or(self.max_frame_ms),
            max_over_budget_pct: over.max_over_budget_pct.or(self.max_over_budget_pct),
        }
    }
}

impl Thresholds {
    fn load(path: &Path) -> Result<Thresholds> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Thresholds::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    fn parse(text: &str) -> Result<Thresholds> {
        let table: toml::Table = text.parse()?;
        let mut t = Thresholds::default();
        for (k, v) in &table {
            let sub = v.as_table().with_context(|| format!("[{k}] is a table"))?;
            match k.as_str() {
                "default" => t.default = Limits::read(sub, "default")?,
                "scene" => {
                    for (name, limits) in sub {
                        let limits = limits
                            .as_table()
                            .with_context(|| format!("[scene.{name}] is a table"))?;
                        t.scenes.push((
                            name.clone(),
                            Limits::read(limits, &format!("scene.{name}"))?,
                        ));
                    }
                }
                "leak" => {
                    for (lk, lv) in sub {
                        let n = lv
                            .as_float()
                            .or_else(|| lv.as_integer().map(|i| i as f64))
                            .with_context(|| format!("leak.{lk} is a number"))?;
                        match lk.as_str() {
                            "max_rss_growth_mb" => t.max_rss_growth_mb = Some(n),
                            "max_thread_growth" => t.max_thread_growth = Some(n),
                            "max_handle_growth" => t.max_handle_growth = Some(n),
                            _ => bail!("leak: unknown limit `{lk}`"),
                        }
                    }
                }
                _ => bail!("unknown table [{k}]"),
            }
        }
        Ok(t)
    }

    fn limits(&self, scene: &str) -> Limits {
        self.scenes
            .iter()
            .find(|(name, _)| name == scene)
            .map_or_else(|| self.default.clone(), |(_, l)| self.default.with(l))
    }

    /// Every limit a row broke, as one line each.
    fn judge(&self, rows: &[Value]) -> Vec<String> {
        let mut fails = Vec::new();
        for r in rows {
            let name = r["scene"].as_str().unwrap_or("?");
            if let Some(e) = r.get("error").and_then(Value::as_str) {
                fails.push(format!("{name}: didn't run: {e}"));
                continue;
            }
            if r.get("skipped").is_some() {
                continue;
            }
            let l = self.limits(name);
            let fps = r["fps"].as_f64().unwrap_or(0.0);
            if let Some(min) = l.min_fps
                && fps < min
            {
                fails.push(format!("{name}: {fps:.1} fps, under {min}"));
            }
            let frame_ms = r["program"]["frame_ms"].as_f64().unwrap_or(f64::INFINITY);
            if let Some(max) = l.max_frame_ms
                && frame_ms > max
            {
                fails.push(format!("{name}: {frame_ms:.2} ms a frame, over {max}"));
            }
            let frames = r["program"]["frames"].as_f64().unwrap_or(0.0);
            let over = r["program"]["over_budget"].as_f64().unwrap_or(0.0);
            let pct = if frames > 0.0 {
                over * 100.0 / frames
            } else {
                100.0
            };
            if let Some(max) = l.max_over_budget_pct
                && pct > max
            {
                fails.push(format!(
                    "{name}: {pct:.1}% of frames over budget, over {max}%"
                ));
            }
        }
        // RSS only across churn: the allocator keeps what a heavy scene used, so the base
        // scene after the run sits higher than before it without anything leaking.
        let base = |name: &str| rows.iter().find(|r| r["scene"] == name);
        let mut pairs: Vec<(String, &Value, &Value, bool)> = rows
            .iter()
            .filter(|r| r["process_after_first_cycle"].is_object())
            .map(|r| {
                let name = r["scene"].as_str().unwrap_or("?").to_owned();
                (name, &r["process_after_first_cycle"], &r["process"], true)
            })
            .collect();
        if let (Some(a), Some(b)) = (base("baseline"), base("baseline_after")) {
            pairs.push(("the run".to_owned(), &a["process"], &b["process"], false));
        }
        for (what, before, after, rss) in pairs {
            for (limit, key, unit) in [
                (
                    self.max_rss_growth_mb.filter(|_| rss),
                    "rss_mb",
                    "resident MB",
                ),
                (self.max_thread_growth, "threads", "threads"),
                (self.max_handle_growth, "handles", "handles"),
            ] {
                if let (Some(max), Some(x), Some(y)) =
                    (limit, before[key].as_f64(), after[key].as_f64())
                    && y - x > max
                {
                    fails.push(format!(
                        "leak: {unit} grew {x} -> {y} across {what}, more than {max}"
                    ));
                }
            }
        }
        fails
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(name: &str, fps: f64, frame_ms: f64, frames: u64, over: u64, rss: u64) -> Value {
        json!({"scene": name, "fps": fps,
               "program": {"frame_ms": frame_ms, "frames": frames, "over_budget": over},
               "process": {"rss_mb": rss, "threads": 30, "handles": 12}})
    }

    #[test]
    fn thresholds_judge_rows_scene_overrides_and_leaks() {
        let t = Thresholds::parse(
            "[default]\nmin_fps = 25\nmax_frame_ms = 40\nmax_over_budget_pct = 5\n\
             [scene.slow]\nmax_frame_ms = 500\n\
             [leak]\nmax_rss_growth_mb = 50\n",
        )
        .expect("parses");
        let rows = vec![
            row_of("baseline", 30.0, 20.0, 150, 0, 200),
            row_of("slow", 29.0, 300.0, 150, 3, 260),
            row_of("fast", 20.0, 45.0, 100, 10, 260),
            json!({"scene": "broken", "error": "nope"}),
            json!({"scene": "page", "skipped": "needs --browser"}),
            row_of("baseline_after", 30.0, 20.0, 150, 0, 300),
            {
                let mut r = row_of("churn", 30.0, 20.0, 150, 0, 400);
                r["process_after_first_cycle"] =
                    json!({"rss_mb": 320, "threads": 30, "handles": 12});
                r
            },
        ];
        let fails = t.judge(&rows);
        assert_eq!(fails.len(), 5, "{fails:#?}");
        assert!(fails.iter().any(|f| f.starts_with("fast: 20.0 fps")));
        assert!(fails.iter().any(|f| f.starts_with("fast: 45.00 ms")));
        assert!(fails.iter().any(|f| f.starts_with("fast: 10.0% of frames")));
        assert!(fails.iter().any(|f| f.starts_with("broken: didn't run")));
        assert!(
            fails
                .iter()
                .any(|f| f.starts_with("leak: resident MB grew 320 -> 400 across churn"))
        );
        assert!(
            !fails.iter().any(|f| f.contains("across the run")),
            "RSS isn't judged run-wide"
        );
    }

    #[test]
    fn thresholds_refuse_unknown_keys() {
        assert!(Thresholds::parse("[default]\nmax_fram_ms = 1\n").is_err());
        assert!(Thresholds::parse("[defaults]\n").is_err());
        assert!(Thresholds::parse("[leak]\nmax_rss = 1\n").is_err());
    }

    #[test]
    fn the_shipped_thresholds_parse_and_name_real_scenes() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("perf");
        let t = Thresholds::load(&dir.join("thresholds.toml")).expect("parses");
        let scenes = scene::load_dir(&dir, (1920.0, 1080.0)).expect("loads");
        for (name, _) in &t.scenes {
            assert!(
                scenes.iter().any(|s| &s.name == name),
                "[scene.{name}] names no scene"
            );
        }
    }

    #[test]
    fn scene_patterns_take_names_and_prefixes() {
        let p = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert!(picked("vf_blur", &[]));
        assert!(picked("vf_blur", &p(&["vf_*"])));
        assert!(picked("vf_blur", &p(&["text_100", "vf_blur"])));
        assert!(!picked("vf_blur", &p(&["vf"])));
    }

    #[test]
    fn options_parse_and_refuse() {
        let a = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        let o = parse(&a(&[
            "--scene",
            "x",
            "--seconds",
            "7",
            "--check",
            "--no-build",
        ]))
        .expect("ok");
        assert_eq!(
            (o.scenes.len(), o.seconds, o.check, o.build),
            (1, 7, true, false)
        );
        assert!(parse(&a(&["--seconds", "0"])).is_err());
        assert!(parse(&a(&["--thresholds", "t.toml"])).is_err());
        assert!(parse(&a(&["--nope"])).is_err());
    }
}
