#!/usr/bin/env python3
"""The Linux release pass: checks a JStream release package on this machine, with nobody
at the keyboard, and writes one report that marks each item of the Linux hardware
checklist (#1357) as passed, failed, or waiting for a person.

What each check does and what passes is in docs/runbooks/agent-pre-pass.md (The Linux
release pass). It is the Linux counterpart of scripts/release-pass.ps1's automatic checks:
it builds nothing for the package checks, and runs the package's own jstream-daemon and
jstream-cli. With --checkout it also runs the hardware tests and the stream harness from
a JStream checkout, which needs Rust.

Nothing of yours is read or changed: each daemon it starts listens on a free port with
its own token, log folder and data and config folders (XDG_DATA_HOME, XDG_CONFIG_HOME) in
the run folder, and the streaming daemon runs without the session bus, so its test key
goes to a file in the run folder and never to your keyring. No output goes anywhere but
a receiver on 127.0.0.1 and a file in the run folder. It never uses sudo: with --deb it
unpacks the package into the run folder, and checks the installed package only when
`dpkg -s jstream` finds one.

Python 3.8 or later, standard library only. ffmpeg and ffprobe (apt install ffmpeg) are
needed for the stream, recording and media checks, and vainfo (apt install vainfo) for
the VA-API check; without them those checks are skipped and say so.

Usage:
    python3 scripts/release-pass.py --deb ~/Downloads/jstream-v0.1.0-beta.1-linux-amd64.deb \\
        --appimage ~/Downloads/jstream-v0.1.0-beta.1-linux-x86_64.AppImage --checkout ~/src/JStream
    python3 scripts/release-pass.py --installed            # the installed package
    python3 scripts/release-pass.py --list                 # the checks and the checklist map
"""

import argparse
import datetime
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import zipfile
import zlib
from pathlib import Path

# --- Results ---------------------------------------------------------------------------

RESULTS = []
SECRETS = []
COLORS = {"PASS": "\033[32m", "FAIL": "\033[31m", "WARN": "\033[33m", "SKIP": "\033[90m"}
TTY = sys.stdout.isatty()


def add(status, check, detail):
    RESULTS.append({"status": status, "check": check, "detail": detail})
    line = f"{status:<4} {check}: {detail}"
    print(f"{COLORS[status]}{line}\033[0m" if TTY else line, flush=True)


def check(ok, name, detail):
    add("PASS" if ok else "FAIL", name, detail)


def section(text):
    print(f"\n=== {text} ===", flush=True)


def run(cmd, timeout=120, env=None, cwd=None):
    """Runs a command to completion: (exit code or 'timeout' or 'missing', stdout, stderr)."""
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env,
                           cwd=cwd, stdin=subprocess.DEVNULL)
        return p.returncode, p.stdout, p.stderr
    except FileNotFoundError:
        return "missing", "", f"{cmd[0]} is not installed"
    except subprocess.TimeoutExpired as e:
        out = e.stdout.decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
        return "timeout", out, f"timed out after {timeout} s"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def oneline(text, limit=300):
    text = re.sub(r"\s+", " ", text or "").strip()
    return text if len(text) <= limit else text[: limit - 1] + "…"


# --- PNG -------------------------------------------------------------------------------


def read_png(path):
    """Decodes an 8-bit, non-interlaced RGB or RGBA PNG: (width, height, rows of bytes, bpp)."""
    data = Path(path).read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, idat, width = 8, b"", 0
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if depth != 8 or ctype not in (2, 6) or interlace:
                raise ValueError(f"unsupported PNG (depth {depth}, colour type {ctype})")
            bpp = 3 if ctype == 2 else 4
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = width * bpp
    rows, prev, i = [], bytearray(stride), 0
    for _ in range(height):
        ftype, line = raw[i], bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if ftype == 1:
                line[x] = (line[x] + a) & 255
            elif ftype == 2:
                line[x] = (line[x] + b) & 255
            elif ftype == 3:
                line[x] = (line[x] + ((a + b) >> 1)) & 255
            elif ftype == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(line)
        prev = line
    return width, height, rows, bpp


def look(path):
    """A PNG's size, its centre colour, and how many of 64 sampled points differ from the
    top-left corner by more than a little (as release-pass.ps1's Get-Look)."""
    w, h, rows, bpp = read_png(path)

    def px(x, y):
        r = rows[y]
        return r[x * bpp], r[x * bpp + 1], r[x * bpp + 2]

    first, varied = px(0, 0), 0
    for i in range(8):
        for j in range(8):
            p = px(int((w - 1) * (i + 0.5) / 8), int((h - 1) * (j + 0.5) / 8))
            if sum(abs(p[k] - first[k]) for k in range(3)) > 24:
                varied += 1
    return {"width": w, "height": h, "centre": px(w // 2, h // 2), "varied": varied}


# --- The daemon ------------------------------------------------------------------------


class Daemon:
    """The release's daemon, with its own port, token, logs, data and config folders."""

    count = 0

    def __init__(self, release, out_dir, name, session_bus=True, extra_args=()):
        Daemon.count += 1
        self.dir = out_dir / f"daemon-{Daemon.count:02d}-{name}"
        self.home = self.dir / "home"
        for sub in ("data", "config", "cache", "state"):
            (self.home / sub).mkdir(parents=True, exist_ok=True)
        self.token_file = self.dir / "token"
        self.log_dir = self.dir / "logs"
        self.cli_path = str(release / "jstream-cli")
        env = dict(os.environ)
        env.update({
            "XDG_DATA_HOME": str(self.home / "data"),
            "XDG_CONFIG_HOME": str(self.home / "config"),
            "XDG_CACHE_HOME": str(self.home / "cache"),
            "XDG_STATE_HOME": str(self.home / "state"),
            "RUST_LOG": env.get("RUST_LOG", "info"),
        })
        if not session_bus:
            env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        self.port = free_port()
        args = [str(release / "jstream-daemon"), "--port", str(self.port), "--token-file",
                str(self.token_file), "--log-dir", str(self.log_dir), *extra_args]
        self.stdout_file = open(self.dir / "stdout.txt", "w")
        self.stderr_file = open(self.dir / "stderr.txt", "w")
        self.proc = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL,
                                     stdout=subprocess.PIPE, stderr=self.stderr_file,
                                     text=True, cwd=str(self.dir), start_new_session=True)
        self.lines, self.ws_url, self.mcp_url, self.token = [], None, None, None
        self.session, self.next_id = None, 0
        ready = threading.Event()

        def pump():
            for line in self.proc.stdout:
                self.lines.append(line.rstrip("\n"))
                self.stdout_file.write(line)
                self.stdout_file.flush()
                m = re.match(r"^mcp endpoint: (\S+)", line)
                if m:
                    self.mcp_url = m.group(1)
                m = re.match(r"^listening on (ws://\S+)", line)
                if m:
                    self.ws_url = m.group(1)
                    ready.set()
            ready.set()

        threading.Thread(target=pump, daemon=True).start()
        ready.wait(90)
        if self.ws_url:
            for _ in range(50):
                if self.token_file.exists() and self.token_file.read_text().strip():
                    break
                time.sleep(0.1)
            try:
                self.token = self.token_file.read_text().strip()
            except OSError:
                self.token = ""
            if not self.token:
                # Treated as a start failure, so the pass goes on with the other checks.
                self.ws_url = None
                self.stderr_file.write(f"release-pass: no token in {self.token_file} after `listening on`\n")
                self.stderr_file.flush()
                return
            SECRETS.append(self.token)
            if not self.mcp_url:
                time.sleep(1)

    def source_error(self, source_id):
        """Why the daemon's log says a source couldn't start, or ''."""
        try:
            log = (self.dir / "stderr.txt").read_text(errors="replace")
        except OSError:
            return ""
        m = re.findall(rf"source could not be started.*source_id={re.escape(source_id)}\b.*?error=(.*)", log)
        return f"; the daemon says: {oneline(m[-1], 300)}" if m else ""

    def why_not(self):
        code = self.proc.poll()
        err = (self.dir / "stderr.txt").read_text(errors="replace")
        lines = [ln for ln in err.splitlines() if re.search(r"error|ERROR|panick", ln)][:2]
        return f"did not start (exit {code}): {oneline(' '.join(lines) or err[-300:])}"

    # MCP over streamable HTTP, as release-pass.ps1 drives it.
    def _post(self, body, method="POST"):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.mcp_url, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.token}")
        req.add_header("Accept", "application/json, text/event-stream")
        req.add_header("Content-Type", "application/json")
        if self.session:
            req.add_header("Mcp-Session-Id", self.session)
            req.add_header("MCP-Protocol-Version", "2025-11-25")
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(req, timeout=60) as resp:
            sid = resp.headers.get("Mcp-Session-Id")
            if sid:
                self.session = sid
            return resp.read().decode()

    def mcp(self, method, params):
        self.next_id += 1
        text = self._post({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        m = re.search(r"(?m)^data:\s*(\{.*\})\s*$", text)
        return json.loads(m.group(1) if m else text)

    def tool(self, name, arguments):
        if not self.session:
            init = self.mcp("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                           "clientInfo": {"name": "jstream-release-pass", "version": "1"}})
            if "result" not in init:
                raise RuntimeError(f"MCP initialize failed: {json.dumps(init)[:300]}")
        r = self.mcp("tools/call", {"name": name, "arguments": arguments})
        if "result" not in r or r["result"].get("isError"):
            raise RuntimeError(f"{name} failed: {json.dumps(r)[:400]}")
        return r["result"].get("structuredContent")

    def op(self, tool, op):
        return self.tool(tool, {"op": op})

    def batch(self, edits):
        return self.tool("batch", {"edits": edits})

    def cli_raw(self, args, timeout=120, stdin=None):
        cmd = [self.cli_path, "--json", "--url", self.ws_url, "--token-file", str(self.token_file), *args]
        try:
            p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, input=stdin)
            return p.returncode, p.stdout, p.stderr
        except subprocess.TimeoutExpired:
            return "timeout", "", f"timed out after {timeout} s"

    def cli(self, args, timeout=120):
        """One jstream-cli call: its parsed `result.result`, or raises with its error."""
        code, out, err = self.cli_raw(args, timeout)
        if code != 0:
            try:
                e = json.loads(err)["error"]
                raise RuntimeError(f"{e.get('message')} ({e.get('code')}, exit {code})")
            except (ValueError, KeyError, TypeError):
                raise RuntimeError(f"{oneline(err)} (exit {code})")
        reply = json.loads(out).get("result") or {}
        return reply.get("result") or {}

    def stop(self, graceful=False, timeout=30):
        """Stops it. Graceful: SIGINT, then (exited, exit code, printed `stopped`)."""
        if self.session:
            try:
                self._post(None, method="DELETE")
            except Exception:
                pass
            self.session = None
        result = None
        if self.proc.poll() is None:
            if graceful:
                self.proc.send_signal(signal.SIGINT)
                try:
                    self.proc.wait(timeout)
                except subprocess.TimeoutExpired:
                    pass
                time.sleep(0.5)
                exited = self.proc.poll() is not None
                result = (exited, self.proc.returncode, any(ln.strip() == "stopped" for ln in self.lines))
            if self.proc.poll() is None:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait(10)
        self.stdout_file.close()
        self.stderr_file.close()
        return result


SCENE_N = [0]


def program_scene(d, kind, settings=None):
    """A new scene on program holding one source that fills a 1920x1080 canvas."""
    SCENE_N[0] += 1
    n = SCENE_N[0]
    create = {"type": "source.create", "source_id": f"src_{n}", "name": f"Source {n}", "kind": kind}
    if settings:
        create["settings"] = settings
    d.batch([
        {"type": "canvas.set_size", "width": 1920, "height": 1080},
        {"type": "scene.create", "scene_id": f"main_{n}", "name": f"Main {n}"},
        create,
        {"type": "item.add", "scene_id": f"main_{n}", "item_id": f"item_{n}", "source_id": f"src_{n}",
         "geometry": {"center": {"x": 960, "y": 540}, "size": {"width": 1920, "height": 1080},
                      "fit_mode": "fit"}},
    ])
    d.op("scene", {"type": "scene.switch", "scene_id": f"main_{n}"})


def remove_program_scene(d):
    n = SCENE_N[0]
    try:
        d.batch([{"type": "item.remove", "item_id": f"item_{n}"},
                 {"type": "source.delete", "source_id": f"src_{n}"}])
    except Exception as e:
        add("WARN", "clean up", f"item_{n} and src_{n} not removed: {e}")


def snapshot(d, path):
    import base64
    r = d.cli(["video", "snapshot"])
    Path(path).write_bytes(base64.b64decode(r["png"]))
    return path


def poll_snapshot(d, path, want, seconds=30):
    """Snapshots until `want(look)` holds or time runs out; the last look, or raises."""
    deadline, last, error = time.time() + seconds, None, None
    while time.time() < deadline:
        time.sleep(2)
        try:
            snapshot(d, path)
            last = look(path)
            if want(last):
                return last
        except Exception as e:
            error = e
    if last is None:
        raise RuntimeError(f"video.snapshot failed: {error}")
    return last


# --- The checks ------------------------------------------------------------------------

CORE_KINDS = ["browser", "display", "window", "webcam", "color", "text", "image", "media"]


class Pass:
    def __init__(self, args):
        self.args = args
        stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
        base = Path(args.work_dir).expanduser() if args.work_dir else \
            Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share") / "jstream-release-pass"
        self.work = base
        self.out = base / "runs" / stamp
        self.out.mkdir(parents=True, exist_ok=True)
        self.release = None
        self.version = args.expect
        self.vaapi = None
        self.ffmpeg = shutil.which("ffmpeg") and shutil.which("ffprobe")
        self.n_webcams = 0
        self.kinds = []
        self.screenshots = []

    def clean_up(self):
        """Removes the unpacked packages, unless --keep, and says what the run leaves."""
        unpacked = [self.work / "release", self.work / "appimage"]
        if not self.args.keep:
            for folder in unpacked:
                shutil.rmtree(folder, ignore_errors=True)
            for home in list(self.out.glob("daemon-*/home")) + [self.out / "appimage-home"]:
                shutil.rmtree(home, ignore_errors=True)
        left = [str(self.out), f"{self.out}.zip"] + [str(f) for f in unpacked if f.exists()]
        return "Left on this machine: " + ", ".join(left)

    # --- The machine ---

    def machine(self):
        info = {}
        try:
            osr = dict(re.findall(r'^(\w+)="?([^"\n]*)"?$', Path("/etc/os-release").read_text(), re.M))
            info["os"] = osr.get("PRETTY_NAME", "?")
        except OSError:
            info["os"] = "?"
        info["kernel"] = platform.release()
        info["session"] = os.environ.get("XDG_SESSION_TYPE", "?")
        info["desktop"] = os.environ.get("XDG_CURRENT_DESKTOP", "?")
        try:
            cpu = re.search(r"^model name\s*:\s*(.+)$", Path("/proc/cpuinfo").read_text(), re.M)
            info["cpu"] = f"{cpu.group(1) if cpu else '?'} ({os.cpu_count()} threads)"
        except OSError:
            info["cpu"] = "?"
        code, out, _ = run(["lspci"], 20)
        if code == 0:
            info["gpu"] = "; ".join(re.sub(r"^\S+\s+", "", ln) for ln in out.splitlines()
                                    if re.search(r"VGA|3D|Display", ln)) or "?"
        return info

    def check_vaapi(self):
        code, out, err = run(["vainfo"], 30)
        text = out + err
        (self.out / "vainfo.txt").write_text(text)
        if code == "missing":
            add("SKIP", "VA-API H.264 encode", "vainfo is not installed (sudo apt install vainfo)")
            return
        driver = re.search(r"Driver version:\s*(.+)", text)
        drv = driver.group(1).strip() if driver else "no driver"
        h264 = re.search(r"VAProfileH264(High|Main)\s*:\s*VAEntrypointEncSlice", text)
        hevc = re.search(r"VAProfileHEVCMain\s*:\s*VAEntrypointEncSlice", text)
        self.vaapi = bool(h264)
        check(self.vaapi, "VA-API H.264 encode",
              f"{drv}; H.264 encode: {'yes' if h264 else 'no'}; HEVC encode: {'yes' if hevc else 'no'}")

    # --- The package ---

    def checksum(self, path, name):
        digest = sha256(path)
        for sums in (Path(f"{path}.sha256"), path.parent / "SHA256SUMS", path.parent / "sha256sums.txt"):
            if sums.is_file():
                text = sums.read_text()
                line = next((ln for ln in text.splitlines() if path.name in ln), text.strip())
                want = line.split()[0].lower() if line.split() else ""
                check(digest == want, f"{name} checksum", f"sha256 {digest}; {sums.name} says {want}")
                return
        add("WARN", f"{name} checksum", f"sha256 {digest}; no .sha256 or SHA256SUMS beside it to compare with")

    def package_tree(self, root, name):
        """Checks a package's tree under `root` (a / for the installed one)."""
        lib = root / "usr/lib/jstream"
        want = ["jstream", "jstream-daemon", "jstream-cli", "ui/index.html",
                "browser/jstream-browser-helper", "browser/chrome-sandbox"]
        missing = [w for w in want if not (lib / w).exists()]
        links = [b for b in ("jstream", "jstream-daemon", "jstream-cli")
                 if not (root / "usr/bin" / b).is_symlink()]
        check(not missing and not links, f"{name} files",
              f"missing in usr/lib/jstream: {', '.join(missing)}; " if missing else
              f"programs, ui, browser helper and sandbox in {lib}" +
              (f"; not linked from usr/bin: {', '.join(links)}" if links else "; linked from usr/bin"))
        entry = root / "usr/share/applications/dev.jstream.desktop.desktop"
        icon = root / "usr/share/icons/hicolor/256x256/apps/dev.jstream.desktop.png"
        text = entry.read_text() if entry.is_file() else ""
        check("Exec=jstream" in text and icon.is_file(), f"{name} menu entry",
              f"{entry.name}: {'Exec=jstream' if 'Exec=jstream' in text else 'missing or no Exec=jstream'}; "
              f"icon: {'there' if icon.is_file() else 'missing'}")
        notice = root / "usr/share/doc/jstream/NOTICE.txt"
        ntext = notice.read_text(errors="replace") if notice.is_file() else ""
        check("AAC" in ntext, f"{name} AAC notice",
              f"{notice} names AAC" if "AAC" in ntext else f"{notice}: {'no AAC notice' if ntext else 'missing'}")
        return lib

    def sandbox_installed(self, lib):
        sb = lib / "browser/chrome-sandbox"
        try:
            st = sb.stat()
            ok = st.st_uid == 0 and bool(st.st_mode & stat.S_ISUID)
            check(ok, "installed chrome-sandbox setuid root",
                  f"owner uid {st.st_uid}, mode {oct(st.st_mode & 0o7777)}")
        except OSError as e:
            add("FAIL", "installed chrome-sandbox setuid root", str(e))

    def from_deb(self, deb):
        deb = Path(deb).expanduser().resolve()
        if not deb.is_file():
            add("FAIL", "deb", f"no such file: {deb}")
            return None
        self.checksum(deb, "deb")
        code, out, err = run(["dpkg-deb", "-f", str(deb), "Package", "Version", "Architecture", "Depends"], 60)
        if code != 0:
            add("FAIL", "deb control", oneline(err))
            return None
        fields = dict(re.findall(r"^(\w+): (.*)$", out, re.M))
        if not self.version:
            self.version = re.sub(r"-\d+$", "", fields.get("Version", "")).replace("~", "-")
        check(fields.get("Package") == "jstream" and "libwebkit2gtk-4.1" in fields.get("Depends", ""),
              "deb control", f"{fields.get('Package')} {fields.get('Version')} {fields.get('Architecture')}; "
              f"depends on WebKitGTK 4.1: {'yes' if 'libwebkit2gtk-4.1' in fields.get('Depends', '') else 'no'}")
        code, out, _ = run(["dpkg-deb", "-c", str(deb)], 60)
        sb = next((ln for ln in out.splitlines() if ln.endswith("/browser/chrome-sandbox")), "")
        check(sb.startswith("-rws") and "root/root" in sb, "deb chrome-sandbox setuid root",
              oneline(sb) or "chrome-sandbox not in the package")
        dest = self.work / "release" / deb.stem
        shutil.rmtree(dest, ignore_errors=True)
        dest.mkdir(parents=True)
        code, _, err = run(["dpkg-deb", "-x", str(deb), str(dest)], 300)
        if code != 0:
            add("FAIL", "deb unpacks", oneline(err))
            return None
        return self.package_tree(dest, "deb")

    def installed(self):
        code, out, _ = run(["dpkg-query", "-W", "-f", "${Status}|${Version}", "jstream"], 20)
        if code != 0 or "install ok installed" not in out:
            add("SKIP", "installed package", "dpkg doesn't list jstream as installed: "
                "`sudo apt install ./<the .deb>` installs it (a person's step: it needs sudo)")
            return None
        version = out.split("|")[1]
        if not self.version:
            self.version = re.sub(r"-\d+$", "", version).replace("~", "-")
        add("PASS", "installed package", f"jstream {version} installed")
        lib = self.package_tree(Path("/"), "installed")
        self.sandbox_installed(lib)
        return lib

    def appimage(self, image):
        image = Path(image).expanduser().resolve()
        if not image.is_file():
            add("FAIL", "AppImage", f"no such file: {image}")
            return
        self.checksum(image, "AppImage")
        work = self.work / "appimage"
        shutil.rmtree(work, ignore_errors=True)
        work.mkdir(parents=True)
        copy = work / image.name
        shutil.copy2(image, copy)
        copy.chmod(0o755)
        code, _, err = run([str(copy), "--appimage-extract"], 300, cwd=str(work))
        root = work / "squashfs-root"
        if code != 0 or not root.is_dir():
            add("FAIL", "AppImage unpacks", f"--appimage-extract: exit {code}: {oneline(err)}")
            return
        bin_ = root / "usr/bin"
        want = ["jstream", "ui/index.html", "browser/jstream-browser-helper"]
        missing = [w for w in want if not (bin_ / w).exists()]
        webkit = list(root.glob("usr/lib/*/webkit2gtk-4.1/WebKitWebProcess"))
        no_setuid = not (bin_ / "browser/chrome-sandbox").exists()
        check(not missing and webkit and no_setuid, "AppImage contents",
              (f"missing: {', '.join(missing)}; " if missing else "jstream, ui and browser helper there; ") +
              f"WebKitGTK's processes: {'there' if webkit else 'missing'}; "
              f"no setuid chrome-sandbox: {no_setuid}")
        if not self.args.launch:
            add("SKIP", "AppImage starts", "not launched: --launch opens its window on this desktop")
            return
        self.launch_appimage(copy)

    def launch_appimage(self, image):
        """Runs the AppImage for 25 seconds: it must stay up and start WebKitGTK's processes."""
        log = open(self.out / "appimage-launch.log", "w")
        env = dict(os.environ)
        home = self.out / "appimage-home"
        for sub in ("data", "config", "cache", "state"):
            (home / sub).mkdir(parents=True, exist_ok=True)
        env.update({"XDG_DATA_HOME": str(home / "data"), "XDG_CONFIG_HOME": str(home / "config"),
                    "XDG_CACHE_HOME": str(home / "cache"), "XDG_STATE_HOME": str(home / "state")})
        p = subprocess.Popen([str(image)], env=env, stdout=log, stderr=log, stdin=subprocess.DEVNULL,
                             start_new_session=True)
        time.sleep(25)
        alive = p.poll() is None
        code, out, _ = run(["ps", "-o", "comm=", "--ppid", str(p.pid), "-g", str(p.pid)], 10)
        procs = sorted(set(out.split()))
        webkit = [x for x in procs if x.startswith("WebKit")]
        shot = self.out / "screenshot-appimage.png"
        if self.args.screenshot and shutil.which("gnome-screenshot"):
            run(["gnome-screenshot", "-f", str(shot)], 20)
            if shot.is_file():
                self.screenshots.append(shot)
        try:
            os.killpg(p.pid, signal.SIGTERM)
            p.wait(15)
        except Exception:
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except Exception:
                pass
        log.close()
        check(alive and bool(webkit), "AppImage starts",
              f"still running after 25 s: {alive}; exit {p.returncode if not alive else '-'}; "
              f"processes: {', '.join(procs) or 'none'}")

    def versions(self):
        for exe, arg in (("jstream-daemon", "--version"), ("jstream-cli", "version")):
            code, out, err = run([str(self.release / exe), arg], 30)
            text = oneline(f"{out} {err}")
            ok = code == 0 and (not self.version or self.version in text)
            check(ok, f"{exe} version", text + (f" (want {self.version})" if self.version else ""))

    # --- The engine ---

    def engine(self, d):
        kinds = d.op("kind", {"type": "kind.list"})
        have = sorted(set(re.findall(r'"kind_id":\s*"([^"]+)"', json.dumps(kinds))))
        self.kinds = have
        (self.out / "kinds.txt").write_text("\n".join(have) + "\n")
        missing = [k for k in CORE_KINDS if k not in have]
        check(not missing, "kind.list", f"missing: {', '.join(missing)} ({len(have)} kinds)" if missing
              else f"{len(have)} kinds, the capture, browser and media ones among them")

        program_scene(d, {"type": "builtin", "kind": "color"}, {"color": "#2040a0ff"})
        time.sleep(6)
        try:
            prof = d.cli(["profile", "get", "5"])
        except Exception as e:
            add("FAIL", "profile.get", str(e))
            return
        (self.out / "profile.json").write_text(json.dumps(prof, indent=2))
        names = thread_names(prof)
        check(bool(prof.get("thread_cpu")) and bool(names), "profile.get threads",
              f"thread_cpu: {prof.get('thread_cpu')}; threads: {', '.join(names)}")
        drawer = next((x for x in prof.get("drawers", []) if x.get("drawer") == "program"), None)
        if drawer:
            detail = (f"program: {drawer.get('frames')} frames, p95 {drawer.get('frame_p95_ms')} ms, "
                      f"over budget {drawer.get('over_budget')}, skipped {drawer.get('skipped')}")
            add("PASS" if not drawer.get("over_budget") and not drawer.get("skipped") else "WARN",
                "program frames on time", detail)
        remove_program_scene(d)

    def snapshots(self, d):
        html = '<html><body style="margin:0;background:#00ff00"></body></html>'
        program_scene(d, {"type": "builtin", "kind": "browser"}, {"html": html, "css": "body { margin: 0; }"})
        f = self.out / "snapshot-browser.png"
        green = lambda lk: lk["centre"][1] > 200 and lk["centre"][0] < 60 and lk["centre"][2] < 60
        try:
            lk = poll_snapshot(d, f, green, 40)
            check(green(lk), "snapshot of a browser page",
                  f"{lk['width']}x{lk['height']}, centre rgb{lk['centre']}, want the page's green"
                  + ("" if green(lk) else d.source_error(f"src_{SCENE_N[0]}")))
        except Exception as e:
            add("FAIL", "snapshot of a browser page", str(e))
        remove_program_scene(d)

        if "media" not in self.kinds:
            add("SKIP", "snapshot of a media file", "this build has no media kind")
        elif not self.ffmpeg:
            add("SKIP", "snapshot of a media file", "ffmpeg is not installed (sudo apt install ffmpeg)")
        else:
            clip = self.out / "media-red.mp4"
            code, _, err = run(["ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-f", "lavfi",
                                "-i", "color=c=red:size=1280x720:rate=30:duration=10", "-c:v", "libx264",
                                "-pix_fmt", "yuv420p", str(clip)], 120)
            if code != 0:
                add("SKIP", "snapshot of a media file", f"ffmpeg couldn't make a clip: {oneline(err)}")
            else:
                program_scene(d, {"type": "builtin", "kind": "media"}, {"file": {"type": "asset", "root": "absolute", "path": str(clip)}, "loop": True})
                f = self.out / "snapshot-media.png"
                red = lambda lk: lk["centre"][0] > 180 and lk["centre"][1] < 80 and lk["centre"][2] < 80
                try:
                    lk = poll_snapshot(d, f, red, 30)
                    check(red(lk), "snapshot of a media file",
                          f"{lk['width']}x{lk['height']}, centre rgb{lk['centre']}, want the clip's red"
                          + ("" if red(lk) else d.source_error(f"src_{SCENE_N[0]}")))
                except Exception as e:
                    add("FAIL", "snapshot of a media file", str(e))
                remove_program_scene(d)

        if not self.args.display:
            add("SKIP", "snapshot of a display capture",
                "not run: on Wayland it shows the screen picker (--display runs it with someone there)")
            return
        program_scene(d, {"type": "builtin", "kind": "display"}, {"target": ""})
        f = self.out / "snapshot-display.png"
        try:
            lk = poll_snapshot(d, f, lambda lk: lk["varied"] >= 4, 60)
            self.screenshots.append(f)
            check(lk["varied"] >= 4, "snapshot of a display capture",
                  f"{lk['width']}x{lk['height']}, {lk['varied']} of 64 sampled points differ from the corner "
                  "(a uniform picture means the capture drew nothing)")
        except Exception as e:
            add("FAIL", "snapshot of a display capture", str(e))
        remove_program_scene(d)

    def projects(self, d):
        pdir = self.out / "projects"
        pdir.mkdir(exist_ok=True)
        a, b, c = (str(pdir / n) for n in ("first.json", "second.json", "third.json"))
        try:
            d.cli(["collection", "new", a])
            d.batch([{"type": "scene.create", "scene_id": "saved", "name": "Saved scene"}])
            d.cli(["collection", "save-as", b])
            d.cli(["collection", "new", c])
            before = json.dumps(d.cli(["scene", "list"]))
            d.cli(["collection", "open", b])
            after = json.dumps(d.cli(["scene", "list"]))
            info = d.cli(["collection", "info"])
        except Exception as e:
            add("FAIL", "save, new, open", str(e))
            return
        empty, back = '"saved"' not in before, '"saved"' in after
        check(Path(b).is_file() and empty and back, "save, new, open",
              f"second.json written: {Path(b).is_file()}; the new project is empty: {empty}; "
              f"opening second.json brings its scene back: {back}")
        check(info.get("path") == b, "collection.info", f"path {info.get('path')}, want {b}")
        # The recent list is the desktop app's (its host keeps it), not the daemon's.
        recent = info.get("recent", [])
        add("PASS" if recent[:1] == [b] else "SKIP", "the recent list",
            f"recent: {', '.join(recent)}" if recent else "the daemon keeps no recent list: the app's is checked by hand")

    def webcams(self, d):
        try:
            targets = d.cli(["capture", "targets"]).get("targets", [])
        except Exception as e:
            add("FAIL", "capture.targets", str(e))
            return
        cams = [t for t in targets if t.get("kind") == "device"]
        if not cams:
            add("WARN", "webcam formats and controls", "no webcam found: plug one in for this and the bench's webcam scenario")
            return
        self.n_webcams = len(cams)
        for c in cams:
            formats, controls = c.get("formats") or [], c.get("controls") or []
            f = ", ".join(f"{x.get('width')}x{x.get('height')}@{x.get('fps')} {x.get('pixel_format')}" for x in formats[:4])
            k = ", ".join(x.get("name", "?") for x in controls)
            check(bool(formats), f"webcam formats: {c.get('name')}",
                  f"{len(formats)} formats ({f}); controls: {k or 'none'}")

    def bench(self, d):
        if self.args.no_bench:
            add("SKIP", "bench", "--no-bench")
            return
        out = self.out / "bench"
        scen = "all" + (",display" if self.args.display else "") + (",webcam" if self.n_webcams else "")
        args = ["bench", "--out", str(out), "--scenario", scen]
        media = self.args.media
        if not media and self.ffmpeg:
            media = str(self.out / "bench-media.mp4")
            code, _, _ = run(["ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "error", "-y",
                              "-f", "lavfi", "-i", "testsrc2=duration=10:size=1920x1080:rate=30",
                              "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=10",
                              "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", media], 120)
            if code != 0:
                media = None
        if media:
            args += ["--media", media]
        print(f"The bench runs for several minutes ({scen}).", flush=True)
        cmd = [str(self.release / "jstream-cli"), "--url", d.ws_url, "--token-file", str(d.token_file), *args]
        code, sout, serr = run(cmd, 1800)
        (self.out / "bench.log").write_text(f"{sout}\n{serr}")
        summary = out / "summary.md"
        lines = ""
        if summary.is_file():
            lines = " / ".join([ln for ln in summary.read_text().splitlines()
                                if re.search(r"\b(pass|fail|skip)", ln)][:12])
        if code == 0:
            add("PASS", "bench", f"every gate passed ({scen}){'' if media else '; no media scenario (no ffmpeg)'}")
        elif code == 4:
            add("FAIL", "bench", f"a gate failed: {oneline(lines, 600)}; see bench/summary.md")
        else:
            add("FAIL", "bench", f"exit {code}: {oneline(serr)}; see bench.log")

    # --- Going live and recording ---

    def outputs(self):
        """Streams to a receiver on 127.0.0.1 on VA-API, records to a file, and quits with
        the recording running, from a daemon without the session bus."""
        if not self.ffmpeg:
            for n in ("going live on VA-API", "recording to a file", "quitting stops outputs"):
                add("SKIP", n, "ffmpeg and ffprobe are not installed (sudo apt install ffmpeg)")
            return
        d = Daemon(self.release, self.out, "outputs", session_bus=False)
        if not d.ws_url:
            add("FAIL", "going live on VA-API", d.why_not())
            d.stop()
            return
        graceful = None
        try:
            d.batch([
                {"type": "canvas.set_size", "width": 1920, "height": 1080},
                {"type": "scene.create", "scene_id": "main", "name": "Main"},
                {"type": "source.create", "source_id": "bars", "name": "Bars",
                 "kind": {"type": "builtin", "kind": "color"}, "settings": {"color": "#ff0000ff"}},
                {"type": "item.add", "scene_id": "main", "item_id": "bars_1", "source_id": "bars",
                 "geometry": {"center": {"x": 960, "y": 540}, "size": {"width": 1920, "height": 1080},
                              "fit_mode": "stretch"}},
            ])
            d.op("scene", {"type": "scene.switch", "scene_id": "main"})
            self.live(d)
            graceful = self.record(d)
        except Exception as e:
            add("FAIL", "outputs", str(e))
        finally:
            if graceful is None:
                d.stop()

    def wait_state(self, d, output_id, seconds=30):
        deadline, st = time.time() + seconds, {}
        while time.time() < deadline:
            st = d.cli(["output", "status", output_id])
            if st.get("state") in ("active", "failed"):
                return st
            time.sleep(1)
        return st

    def live(self, d):
        if self.vaapi is False:
            add("SKIP", "going live on VA-API", "vainfo lists no H.264 encode entry point")
            return
        port, key = free_port(), "release-pass-key"
        SECRETS.append(key)
        flv = self.out / "live.flv"
        recv_log = open(self.out / "receiver.log", "w")
        recv = subprocess.Popen(["ffmpeg", "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-listen", "1",
                                 "-i", f"rtmp://127.0.0.1:{port}/live/{key}", "-c", "copy", "-f", "flv", str(flv)],
                                stdin=subprocess.DEVNULL, stdout=recv_log, stderr=recv_log)
        try:
            d.batch([
                {"type": "encoder.create", "profile_id": "hw", "name": "Hardware",
                 "profile": {"video": {"codec": "h264", "rate_control": {"type": "cbr", "kbps": 6000},
                                       "keyframe_interval_s": 2, "hardware": "require"},
                             "audio": {"codec": "aac", "kbps": 160}}},
                {"type": "output.create", "output_id": "live",
                 "kind": {"type": "rtmp", "target": {"type": "generic", "url": f"rtmp://127.0.0.1:{port}/live"}}},
                {"type": "output.set_encoder", "output_id": "live", "profile_id": "hw"},
            ])
            code, _, err = d.cli_raw(["output", "set-key", "live"], stdin=key + "\n")
            if code != 0:
                raise RuntimeError(f"output set-key: {oneline(err)}")
            time.sleep(1)
            try:
                d.op("output", {"type": "output.start", "output_id": "live"})
            except Exception as e:
                add("FAIL", "going live on VA-API", f"output.start: {e}")
                return
            st = self.wait_state(d, "live")
            enc = st.get("encoder") or {}
            if st.get("state") != "active":
                add("FAIL", "going live on VA-API", f"the output is {st.get('state')}; see {d.dir.name}/logs")
                return
            try:
                prof = d.cli(["profile", "get", "5"])
                (self.out / "profile-live.json").write_text(json.dumps(prof, indent=2))
                names = thread_names(prof)
                check("jstream-render" in names, "the render thread while live",
                      f"threads: {', '.join(names)}")
            except Exception as e:
                add("FAIL", "the render thread while live", str(e))
            time.sleep(5)
            d.op("output", {"type": "output.stop", "output_id": "live"})
            try:
                recv.wait(20)
            except subprocess.TimeoutExpired:
                recv.terminate()
            p = probe(flv)
            ok = (enc.get("hardware") and p.get("video") == "h264" and p.get("size") == "1920x1080"
                  and p.get("audio") == "aac" and p.get("duration", 0) > 6)
            check(ok, "going live on VA-API",
                  f"hardware encoder: {enc.get('hardware')}; the receiver got {p.get('video')} "
                  f"{p.get('size')} {p.get('fps')} fps, {p.get('audio')}, {p.get('duration', 0):.1f} s")
        finally:
            if recv.poll() is None:
                recv.kill()
            recv_log.close()

    def record(self, d):
        """Records for 8 s, then quits the daemon (SIGINT) with it running. Returns the stop."""
        path = self.out / "recording.mp4"
        d.batch([{"type": "output.create", "output_id": "rec",
                  "kind": {"type": "record", "path": str(path), "container": "mp4"}}])
        try:
            d.op("output", {"type": "output.start", "output_id": "rec"})
        except Exception as e:
            add("FAIL", "recording to a file", f"output.start: {e}")
            return None
        st = self.wait_state(d, "rec")
        enc = st.get("encoder") or {}
        if st.get("state") != "active":
            add("FAIL", "recording to a file", f"the output is {st.get('state')}; see {d.dir.name}/logs")
            return None
        time.sleep(8)
        g = d.stop(graceful=True)
        exited, code, stopped = g or (False, None, False)
        check(exited and code == 0 and stopped, "quitting stops outputs",
              f"SIGINT with a recording running: exited {exited}, exit code {code}, printed stopped: {stopped}")
        part = Path(f"{path}.part")
        if not path.is_file():
            add("FAIL", "recording to a file", f"{path.name} not there after the stop"
                + ("; the .part file is" if part.exists() else ""))
            return g
        p = probe(path)
        code, _, err = run(["ffmpeg", "-hide_banner", "-nostdin", "-v", "error", "-i", str(path), "-f", "null", "-"], 120)
        clean = code == 0 and not err.strip()
        check(p.get("video") == "h264" and p.get("audio") == "aac" and p.get("duration", 0) > 5 and clean,
              "recording to a file",
              f"hardware encoder: {enc.get('hardware')}; {p.get('video')} {p.get('size')}, {p.get('audio')}, "
              f"{p.get('duration', 0):.1f} s; decodes without errors: {clean}" + ("" if clean else f" ({oneline(err)})"))
        return g

    # --- From a checkout ---

    def checkout(self):
        src = Path(self.args.checkout).expanduser().resolve()
        if not (src / "Cargo.toml").is_file():
            add("FAIL", "checkout", f"not a JStream checkout: {src}")
            return
        if not shutil.which("cargo"):
            add("SKIP", "hardware tests", "cargo is not installed (rustup.rs)")
            return
        code, out, _ = run(["git", "-C", str(src), "log", "-1", "--format=%h %s"], 20)
        print(f"Checkout: {src} at {out.strip()}", flush=True)
        self.hw_tests(src, {}, "hw", "hw-tests.log")
        layer = Path("/usr/share/vulkan/explicit_layer.d/VkLayer_khronos_validation.json")
        if layer.is_file():
            self.hw_tests(src, {"VK_INSTANCE_LAYERS": "VK_LAYER_KHRONOS_validation"}, "hw+validation",
                          "hw-tests-validation.log", only="vaapi")
        else:
            add("SKIP", "hw+validation", "the Vulkan validation layer is not installed "
                "(sudo apt install vulkan-validationlayers)")
        if not self.ffmpeg:
            add("SKIP", "e2e-stream on OpenH264", "ffmpeg is not installed")
            return
        code, sout, serr = run(["cargo", "xtask", "e2e-stream", "--openh264-download"], 3600, cwd=str(src))
        (self.out / "e2e-stream.log").write_text(f"{sout}\n{serr}")
        passed = re.findall(r"^test (\S+) \.\.\. ok", sout + serr, re.M)
        failed = re.findall(r"^test (\S+) \.\.\. FAILED", sout + serr, re.M)
        check(code == 0 and not failed, "e2e-stream on OpenH264",
              f"{len(passed)} passed, {len(failed)} failed{': ' + ', '.join(failed) if failed else ''}; e2e-stream.log")

    def hw_tests(self, src, extra_env, prefix, log_name, only=None):
        env = dict(os.environ, **extra_env)
        cmd = ["cargo", "test", "-p", "jstream-platform-linux", "--features", "hw-tests", "--locked", "--",
               "--ignored", "--test-threads=1", "--skip", "portal", "--skip", "hotkeys"]
        if only:
            cmd.append(only)
        print(f"Running the hardware tests ({prefix}); the first build takes a while.", flush=True)
        code, sout, serr = run(cmd, 3600, env=env, cwd=str(src))
        (self.out / log_name).write_text(f"{sout}\n{serr}")
        tests = re.findall(r"^test (\S+) \.\.\. (ok|FAILED|ignored)", sout, re.M)
        if not tests:
            add("FAIL", prefix, f"no tests ran (exit {code}): {oneline(serr[-400:])}; {log_name}")
            return
        for name, result in tests:
            if result != "ignored":
                add("PASS" if result == "ok" else "FAIL", f"{prefix}: {name}", f"cargo test; {log_name}")


def thread_names(prof):
    return sorted({t.get("name") or t.get("remote_name") or "" for t in prof.get("threads", [])} - {""})


def probe(path):
    code, out, _ = run(["ffprobe", "-v", "error", "-show_entries",
                        "stream=codec_type,codec_name,width,height,avg_frame_rate:format=duration",
                        "-of", "json", str(path)], 60)
    if code != 0:
        return {}
    j = json.loads(out)
    got = {"duration": float(j.get("format", {}).get("duration") or 0)}
    for s in j.get("streams", []):
        if s.get("codec_type") == "video":
            got["video"] = s.get("codec_name")
            got["size"] = f"{s.get('width')}x{s.get('height')}"
            num, _, den = (s.get("avg_frame_rate") or "0/1").partition("/")
            got["fps"] = round(int(num) / int(den or 1), 2) if int(den or 1) else 0
        elif s.get("codec_type") == "audio":
            got["audio"] = s.get("codec_name")
    return got


# --- The checklist ---------------------------------------------------------------------

# Each line of the Linux hardware checklist (#1357): the checks that cover it (names, or a
# name's prefix ending in ':'), and what is left for a person once they pass ("" for
# nothing). Keep it in step with the issue and docs/runbooks/agent-pre-pass.md.
CHECKLIST = [
    ("GNOME", ".deb installs, starts from the app menu, AAC notice",
     ["deb control", "deb files", "deb chrome-sandbox setuid root", "deb menu entry", "deb AAC notice",
      "installed package", "installed files", "installed menu entry", "installed AAC notice",
      "installed chrome-sandbox setuid root", "jstream-daemon version", "jstream-cli version", "daemon starts"],
     "run `sudo apt install ./<deb>` (needs sudo) and open JStream from the app menu"),
    ("GNOME", "Screen capture through the portal picker, no prompt after a restart",
     ["snapshot of a display capture"],
     "pick a screen in the portal picker, restart, and see it come back without the picker"),
    ("GNOME", "A webcam, a microphone and desktop audio through PipeWire",
     ["webcam formats:", "hw: pipewire::", "hw: v4l2::", "hw: alsa::"],
     "see the webcam's picture and hear the microphone and desktop audio in JStream"),
    ("GNOME", "Going live on VA-API (radeonsi), and recording to a file",
     ["VA-API H.264 encode", "going live on VA-API", "recording to a file"], ""),
    ("GNOME", "Going live on OpenH264 (Cisco's download)",
     ["e2e-stream on OpenH264"],
     "pick OpenH264 in the encoder settings and accept Cisco's download in the app"),
    ("GNOME", "Browser pages, a media file source and a snapshot",
     ["snapshot of a browser page", "snapshot of a media file"], ""),
    ("GNOME", "Quitting stops outputs gracefully, and the recording plays back",
     ["quitting stops outputs", "recording to a file"],
     "quit from the app window with an output running"),
    ("GNOME", "The AppImage starts and a browser page loads (#1482)",
     ["AppImage contents", "AppImage starts"],
     "look at a browser page in the AppImage's window"),
    ("flagged", "KDE Plasma (Wayland): capture, going live and a browser page with the AppImage", [],
     "log in to Plasma (Wayland) and repeat the GNOME capture, live and browser checks"),
    ("flagged", "X11 global hotkeys (#1330)", [], "log in to Ubuntu on Xorg and use a hotkey from another app"),
    ("flagged", "Intel iHD VA-API (#514, #832)", [], "needs an Intel GPU machine; not a beta.1 gate"),
    ("earlier", "#1266 webcam device list and format settings", ["webcam formats:", "hw: v4l2::"],
     "pick a format in Webcam Properties and see the picture change"),
    ("earlier", "#1142 the one-command install on GNOME/Wayland and KDE", [],
     "run scripts/linux/install.sh's one-line install (it needs sudo)"),
    ("earlier", "#942 V4L2 GPU-path capture on a UVC webcam", ["hw: v4l2::device_tests::a_format_on_the_gpu_path"], ""),
    ("earlier", "#832 VA-API HEVC encode on radeonsi", ["hw: vaapi::session::device_tests::encodes_hevc"], ""),
    ("earlier", "#666 VA-API encode with Vulkan validation layers", ["hw+validation:"], ""),
    ("earlier", "#616 DMA-BUF GPU screen capture", [],
     "run `cargo test -p jstream-platform-linux --features hw-tests -- --ignored portal` and pick a screen"),
    ("earlier", "#514 VA-API H.264 encode on radeonsi", ["hw: vaapi::session::device_tests::encodes_nv12",
                                                        "hw: vaapi::session::device_tests::encodes_rgba",
                                                        "hw: vaapi::session::device_tests::encodes_the_contexts"], ""),
    ("earlier", "#1196 webcam camera controls on a real camera", ["hw: v4l2::device_tests::reads_and_sets_camera_controls"],
     "move exposure, focus, white balance and gain in Webcam Properties and see the picture change"),
    ("earlier", "#1482 the AppImage on a desktop", ["AppImage contents", "AppImage starts"],
     "look at a browser page in the AppImage's window"),
]


def matches(name, key):
    """A key ending in ':' or naming hardware tests ('hw…') matches by prefix."""
    return name.startswith(key) if key.endswith(":") or key.startswith("hw") else name == key


def classify():
    rows = []
    for group, title, keys, person in CHECKLIST:
        covering = [r for r in RESULTS if any(matches(r["check"], k) for k in keys)]
        ran = [r for r in covering if r["status"] in ("PASS", "FAIL")]
        fails = [r for r in covering if r["status"] == "FAIL"]
        if fails:
            state = "AUTO-FAIL"
            note = "; ".join(f"{r['check']}: {oneline(r['detail'], 160)}" for r in fails[:3])
        elif ran and not person and not any(r["status"] == "SKIP" for r in covering):
            state = "AUTO-PASS"
            note = f"{len(ran)} checks passed"
        else:
            state = "NEEDS PERSON"
            done = f"auto: {len([r for r in ran if r['status'] == 'PASS'])} checks passed; " if ran else ""
            skipped = [r["check"] for r in covering if r["status"] == "SKIP"]
            note = done + (person or "the automatic checks didn't run") + \
                (f" (skipped: {', '.join(skipped[:3])})" if skipped else "")
        rows.append({"group": group, "item": title, "state": state, "note": note})
    return rows


# --- Report ----------------------------------------------------------------------------


def report(p, machine, started, rows):
    md = [f"# JStream Linux release pass: {p.version or 'unknown version'}", ""]
    for k, v in machine.items():
        md.append(f"- **{k}**: {v}")
    md.append(f"- **took**: {round((time.time() - started) / 60)} min")
    md += ["", "## The checklist (#1357)", "",
           "AUTO-PASS: the checks below cover it and passed. AUTO-FAIL: a check failed. "
           "NEEDS PERSON: what is left for a person, after what the checks did.", "",
           "| state | item | note |", "|---|---|---|"]
    for r in rows:
        md.append(f"| {r['state']} | {r['item']} | {r['note'].replace('|', '/')} |")
    md += ["", "## Every check", "", "| result | check | detail |", "|---|---|---|"]
    for r in RESULTS:
        md.append(f"| {r['status']} | {r['check']} | {oneline(r['detail'], 600).replace('|', '/')} |")
    if p.screenshots:
        md += ["", "## Screenshots of this desktop", "",
               "These files show this machine's screen and stay out of the zip unless --zip-screenshots "
               "was given. Look at them before sharing them:", ""]
        md += [f"- {s.name}" for s in p.screenshots]
    text = "\n".join(md) + "\n"
    for s in SECRETS:
        text = text.replace(s, "***")
    (p.out / "report.md").write_text(text)
    (p.out / "results.json").write_text(json.dumps({"machine": machine, "checklist": rows, "results": RESULTS},
                                                   indent=2))


def make_zip(p):
    zpath = Path(f"{p.out}.zip")
    shots = {s.resolve() for s in p.screenshots}
    with zipfile.ZipFile(zpath, "w", zipfile.ZIP_DEFLATED) as z:
        for f in sorted(p.out.rglob("*")):
            if not f.is_file() or "token" in f.name:
                continue
            rel = f.relative_to(p.out)
            if rel.parts[0] == "appimage-home" or (rel.parts[0].startswith("daemon-") and "home" in rel.parts):
                continue
            if f.suffix == ".png":
                if f.resolve() in shots and not p.args.zip_screenshots:
                    continue
                z.write(f, str(rel))
            elif f.suffix in (".md", ".json", ".log", ".txt", ".jsonl"):
                text = f.read_text(errors="replace")
                for s in SECRETS:
                    text = text.replace(s, "***")
                z.writestr(str(rel), text)
    return zpath


def list_checks():
    print("The checklist (#1357) and the checks that cover each line:\n")
    for group, title, keys, person in CHECKLIST:
        print(f"[{group}] {title}")
        print(f"    checks: {', '.join(keys) or 'none'}")
        print(f"    a person: {person or 'nothing'}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--deb", help="the release's .deb: checked, then unpacked into the run's work folder")
    ap.add_argument("--appimage", help="the release's AppImage: checked and unpacked")
    ap.add_argument("--installed", action="store_true",
                    help="run the installed package's programs (/usr/lib/jstream) instead of the .deb's")
    ap.add_argument("--root", help="a folder holding jstream-daemon and jstream-cli, instead of a package")
    ap.add_argument("--expect", help="the version the programs should report (default: the package's)")
    ap.add_argument("--checkout", help="a JStream checkout: also run the hardware tests and e2e-stream from it")
    ap.add_argument("--display", action="store_true",
                    help="also capture the display (on Wayland it shows the screen picker: someone must be there)")
    ap.add_argument("--launch", action="store_true", help="start the AppImage for 25 seconds (opens its window)")
    ap.add_argument("--screenshot", action="store_true", help="with --launch, screenshot the desktop while it runs")
    ap.add_argument("--zip-screenshots", action="store_true", help="put the desktop screenshots in the zip")
    ap.add_argument("--keep", action="store_true",
                    help="keep the unpacked .deb and AppImage and the daemons' data folders after the run")
    ap.add_argument("--media", help="a video file for the bench's media scenario (default: one ffmpeg makes)")
    ap.add_argument("--no-bench", action="store_true", help="skip the bench (several minutes)")
    ap.add_argument("--work-dir", help="where runs go (default ~/.local/share/jstream-release-pass)")
    ap.add_argument("--list", action="store_true", help="print the checklist map and exit")
    args = ap.parse_args()
    if args.list:
        list_checks()
        return 0
    if sys.platform != "linux":
        print("the Linux release pass runs on Linux only; on Windows run scripts/release-pass.ps1", file=sys.stderr)
        return 2
    if os.geteuid() == 0:
        print("run it as yourself, not root: the packages' programs refuse root's sandbox", file=sys.stderr)
        return 2

    p = Pass(args)
    started = time.time()
    machine = p.machine()
    section("JStream Linux release pass")
    print(f"Run folder: {p.out}")
    for k, v in machine.items():
        print(f"  {k}: {v}")
    interrupted = True
    try:
        section("The machine")
        p.check_vaapi()
        section("The package")
        lib = None
        if args.root:
            lib = Path(args.root).expanduser().resolve()
        if args.deb:
            deb_lib = p.from_deb(args.deb)
            lib = lib or deb_lib
        inst = p.installed()
        if args.installed or not lib:
            lib = inst or lib
        if args.appimage:
            p.appimage(args.appimage)
        if lib and (lib / "jstream-daemon").is_file():
            p.release = lib
            machine["release"] = str(lib)
            machine["version"] = p.version or "?"
            section("The engine")
            p.versions()
            (p.out / "projects").mkdir(exist_ok=True)
            d = Daemon(lib, p.out, "engine", extra_args=("--collection", str(p.out / "projects" / "start.json")))
            if not d.ws_url:
                add("FAIL", "daemon starts", d.why_not())
                d.stop()
            else:
                add("PASS", "daemon starts", f"listening on {d.ws_url}, with its own data and config folders")
                try:
                    for step in (p.engine, p.snapshots, p.projects, p.webcams, p.bench):
                        try:
                            step(d)
                        except Exception as e:
                            add("FAIL", step.__name__, f"{type(e).__name__}: {e}")
                finally:
                    g = d.stop(graceful=True)
                    if g:
                        check(g[0] and g[1] == 0 and g[2], "graceful stop",
                              f"exited {g[0]}, exit code {g[1]}, printed stopped: {g[2]}")
                    else:
                        add("FAIL", "graceful stop", "the daemon had already exited")
            section("Going live and recording")
            p.outputs()
        else:
            add("FAIL", "release", "no package to run: give --deb, --root, or install the .deb")
        if args.checkout:
            section("From the checkout")
            p.checkout()
        interrupted = False
    except KeyboardInterrupt:
        pass
    finally:
        if interrupted:
            add("WARN", "run", "stopped before every check ran")
        rows = classify()
        report(p, machine, started, rows)
        zpath = make_zip(p)
        count = {s: sum(1 for r in RESULTS if r["status"] == s) for s in COLORS}
        print("\n================ Summary ================")
        print(f"release-pass {p.version or '?'}: {count['PASS']} pass, {count['FAIL']} fail, "
              f"{count['WARN']} warn, {count['SKIP']} skip")
        for r in rows:
            print(f"  {r['state']:<10} {r['item']}")
        print(f"Report: {p.out / 'report.md'}")
        print(f"Zip: {zpath}")
        print(p.clean_up())
        if p.screenshots:
            state = "IN the zip" if args.zip_screenshots else "kept out of the zip"
            print(f"Screenshots of this desktop ({state}): " + ", ".join(str(s) for s in p.screenshots))
        print("=========================================")
    return 1 if any(r["status"] == "FAIL" for r in RESULTS) else 0


if __name__ == "__main__":
    sys.exit(main())
