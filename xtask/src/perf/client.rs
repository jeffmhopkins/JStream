//! The engine a perf run measures: the daemon, started on a free port with a fresh token,
//! collection and folders, and `jstream-cli mcp` as the client, spoken to as MCP over its
//! stdin and stdout. Using the CLI's MCP proxy keeps xtask free of the engine's client
//! stack: it only writes and reads JSON lines.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// How long the daemon may take to print its URL.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one MCP call may take: building a scene of hundreds of items, or a
/// `profile.get`, is well inside it.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// A running daemon, stopped on drop.
pub struct Daemon {
    child: Child,
    /// Its WebSocket URL.
    pub url: String,
}

impl Daemon {
    /// Starts `bin` with everything it writes under `dir`. On Linux its data folder,
    /// where it looks for a downloaded OpenH264, is `data_home`/jstream.
    pub fn start(bin: &Path, dir: &Path, data_home: &Path) -> Result<Daemon> {
        let mut command = Command::new(bin);
        command
            .env("XDG_DATA_HOME", data_home)
            .arg("--token-file")
            .arg(dir.join("token"))
            .args(["--port", "0", "--collection"])
            .arg(dir.join("collection.json"))
            .arg("--log-dir")
            .arg(dir.join("logs"))
            .arg("--effects-dir")
            .arg(dir.join("effects"))
            .arg("--plugin-dir")
            .arg(dir.join("plugins"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if std::env::var_os("VK_DRIVER_FILES").is_none()
            && let Some(icd) = crate::e2e_stream::lavapipe()
        {
            eprintln!("xtask perf: Vulkan on lavapipe ({})", icd.display());
            command.env("VK_DRIVER_FILES", icd);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {}", bin.display()))?;
        let stdout = child.stdout.take().context("the daemon's stdout")?;
        let lines = reader(stdout);
        let mut daemon = Daemon {
            child,
            url: String::new(),
        };
        loop {
            match lines.recv_timeout(START_TIMEOUT) {
                Ok(line) => {
                    if let Some(url) = line.strip_prefix("listening on ") {
                        daemon.url = url.trim().to_owned();
                        // Keep draining its stdout, so a later line never blocks it.
                        thread::spawn(move || while lines.recv().is_ok() {});
                        return Ok(daemon);
                    }
                }
                Err(_) => bail!("the daemon exited or didn't print its URL"),
            }
        }
    }

    /// Its process ID.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads `from`'s lines on a thread of its own, so a caller can wait with a timeout.
fn reader(from: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        for line in BufReader::new(from).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// An MCP session through `jstream-cli mcp`, closed on drop.
pub struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    /// Starts the proxy against `daemon` and initializes the session.
    pub fn start(cli: &Path, daemon: &Daemon, token: &Path) -> Result<Mcp> {
        let mut child = Command::new(cli)
            .args(["--url", &daemon.url, "--token-file"])
            .arg(token)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting {}", cli.display()))?;
        let stdin = child.stdin.take().context("the proxy's stdin")?;
        let lines = reader(child.stdout.take().context("the proxy's stdout")?);
        let mut mcp = Mcp {
            child,
            stdin,
            lines,
            next_id: 0,
        };
        mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "xtask-perf", "version": "0"},
            }),
        )?;
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(mcp)
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        writeln!(self.stdin, "{message}").context("writing to the MCP proxy")?;
        self.stdin.flush().context("writing to the MCP proxy")
    }

    /// Sends a request and waits for its reply's `result`.
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let line = self
                .lines
                .recv_timeout(CALL_TIMEOUT)
                .with_context(|| format!("no reply to {method} (the proxy exited or hung)"))?;
            let Ok(reply) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if reply.get("id") != Some(&json!(id)) {
                continue; // a notification
            }
            if let Some(error) = reply.get("error") {
                bail!("{method}: {error}");
            }
            return reply
                .get("result")
                .cloned()
                .context("a reply without a result");
        }
    }

    /// Calls a tool and returns its structured result, failing on a tool error.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<Value> {
        let shown = arguments.to_string();
        let result = self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        let structured = result
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null);
        if result.get("isError") == Some(&Value::Bool(true)) {
            let short: String = shown.chars().take(300).collect();
            bail!("{tool} {short}: {structured}");
        }
        Ok(structured)
    }

    /// Applies edits in one `batch` call.
    pub fn batch(&mut self, edits: &[Value]) -> Result<()> {
        self.call("batch", json!({"edits": edits})).map(drop)
    }

    /// Deletes `target` (a `delete.plan` target) and everything that refers to it, by
    /// sending the batch its plan returns. A target already gone is no error: an
    /// earlier plan took it.
    pub fn delete(&mut self, target: &Value) -> Result<()> {
        let plan = match self.call(
            "delete",
            json!({"op": {"type": "delete.plan", "target": target}}),
        ) {
            Ok(plan) => plan,
            Err(e) if format!("{e:#}").contains("not_found") => return Ok(()),
            Err(e) => return Err(e),
        };
        let edits = find_key(&plan, "edits")
            .and_then(Value::as_array)
            .with_context(|| format!("delete.plan for {target} has no edits"))?
            .clone();
        if edits.is_empty() {
            return Ok(());
        }
        self.batch(&edits)
    }

    /// Runs an `output` tool operation.
    pub fn output(&mut self, op: Value) -> Result<Value> {
        self.call("output", json!({ "op": op }))
    }

    /// Switches the program to `scene`, cutting.
    pub fn switch(&mut self, scene: &str) -> Result<()> {
        self.call(
            "scene",
            json!({"op": {"type": "scene.switch", "scene_id": scene}}),
        )
        .map(drop)
    }

    /// `profile.get` over the last `seconds`, with up to `top` units and sources.
    pub fn profile(&mut self, seconds: u32, top: u32) -> Result<Value> {
        let out = self.call(
            "profile",
            json!({"op": {"type": "profile.get", "seconds": seconds, "top": top}}),
        )?;
        find_profile(&out).context("profile.get's answer has no `drawers`")
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The profile object inside a tool answer: the first object holding `drawers`, however
/// deep the answer wraps it.
fn find_profile(value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) if map.contains_key("drawers") => Some(value.clone()),
        Value::Object(map) => map.values().find_map(find_profile),
        _ => None,
    }
}

/// The first value under `key` in `value`, however deep.
fn find_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find_key(v, key))),
        _ => None,
    }
}

/// The daemon's resident memory, threads, open handles and child processes, from
/// `/proc`. `None` off Linux.
pub fn process(pid: u32) -> Option<Value> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let proc_dir = Path::new("/proc").join(pid.to_string());
    let status = std::fs::read_to_string(proc_dir.join("status")).ok()?;
    let rss_kb: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let count = |sub: &str| std::fs::read_dir(proc_dir.join(sub)).map_or(0, Iterator::count);
    let children = std::fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter(|e| {
            std::fs::read_to_string(e.path().join("stat")).is_ok_and(|stat| {
                // The parent PID is the second field after the command's closing paren.
                stat.rsplit_once(')')
                    .and_then(|(_, rest)| rest.split_whitespace().nth(1).map(str::to_owned))
                    .is_some_and(|ppid| ppid == pid.to_string())
            })
        })
        .count();
    Some(json!({
        "rss_mb": rss_kb / 1024,
        "threads": count("task"),
        "handles": count("fd"),
        "children": children,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_profile_however_wrapped() {
        let answer =
            json!({"id": "r", "result": {"kind": "profile", "result": {"drawers": [], "x": 1}}});
        assert_eq!(find_profile(&answer), Some(json!({"drawers": [], "x": 1})));
        let plan = json!({"result": {"kind": "delete_plan", "result": {"edits": [1], "rev": 3}}});
        assert_eq!(find_key(&plan, "edits"), Some(&json!([1])));
    }

    #[test]
    fn reads_this_process() {
        if cfg!(target_os = "linux") {
            let p = process(std::process::id()).expect("our own /proc entry");
            assert!(p["rss_mb"].as_u64().is_some());
            assert!(p["threads"].as_u64().unwrap_or(0) >= 1);
        } else {
            assert!(process(std::process::id()).is_none());
        }
    }
}
