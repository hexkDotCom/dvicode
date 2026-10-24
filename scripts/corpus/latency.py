#!/usr/bin/env python3
"""Measures how the engine answers while a file is edited, as an editor would see it.

Starts `dvicode-server --stdio` on a corpus configuration (or any folder), waits for the
first build, opens a file, and then:

  - times hover, completion and semantic tokens on the untouched file;
  - types a line into the body of the file's last module, class or package, one character
    every --interval ms, with a completion request after each character (as an editor sends
    them), and times each answer and the problems of the last version;
  - adds a declaration (which makes the engine check the other files too) and times the
    problems of the file and of the project (until no problems arrive for --settle ms);
  - reads the engine's memory (Linux).

Usage:
  latency.py --server ENGINE --manifest M --only CONFIG --file REL [--json OUT]
  latency.py --server ENGINE --root DIR [--build FILE] --file REL

Standard library only.
"""

import argparse
import json
import os
import queue
import re
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


class Server:
    def __init__(self, exe, root, settings, log):
        args = [exe, "--stdio"] + (["--log", log, "--log-level", os.environ.get("LATENCY_LOG_LEVEL", "info")] if log else [])
        self.p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, cwd=root)
        self.q = queue.Queue()
        self.backlog = []
        self.next_id = 0
        self.settings = settings
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        f = self.p.stdout
        while True:
            headers = {}
            while True:
                line = f.readline()
                if not line:
                    self.q.put((time.monotonic(), None))
                    return
                line = line.decode().strip()
                if not line:
                    break
                k, v = line.split(":", 1)
                headers[k.lower()] = v.strip()
            body = f.read(int(headers["content-length"]))
            m = json.loads(body)
            if os.environ.get("LATENCY_DEBUG"):
                pr = m.get("params") or {}
                what = (f"{pr['uri'].rsplit('/', 1)[-1]} v{pr.get('version')} {len(pr['diagnostics'])}" if m.get("method") == "textDocument/publishDiagnostics"
                        else json.dumps(pr)[:160])
                print("<-", m.get("method") or f"response {m.get('id')}", what, file=sys.stderr)
            self.q.put((time.monotonic(), m))

    def send(self, obj):
        b = json.dumps(obj).encode()
        self.p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
        self.p.stdin.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params):
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        return self.next_id

    def _answer(self, m):
        """Requests of the server: configuration, progress tokens, registrations."""
        if m.get("method") == "workspace/configuration":
            items = m.get("params", {}).get("items", [])
            self.send({"jsonrpc": "2.0", "id": m["id"], "result": [self.settings for _ in items]})
        else:
            self.send({"jsonrpc": "2.0", "id": m["id"], "result": None})

    def wait(self, pred, timeout=1800):
        """The first message `pred` accepts (and when it arrived); others are kept."""
        for i, (t, m) in enumerate(self.backlog):
            if pred(m):
                del self.backlog[i]
                return t, m
        end = time.monotonic() + timeout
        while True:
            left = end - time.monotonic()
            if left <= 0:
                raise TimeoutError("no answer")
            t, m = self.q.get(timeout=left)
            if m is None:
                raise RuntimeError("the engine exited")
            if m.get("method") and "id" in m:
                self._answer(m)
                continue
            if pred(m):
                return t, m
            self.backlog.append((t, m))

    def response(self, rid, timeout=1800):
        return self.wait(lambda m: m.get("id") == rid and "method" not in m, timeout)

    def drain(self):
        """Takes what has arrived so far into the backlog (answering requests)."""
        while True:
            try:
                t, m = self.q.get_nowait()
            except queue.Empty:
                return
            if m is None:
                return
            if m.get("method") and "id" in m:
                self._answer(m)
            else:
                self.backlog.append((t, m))

    def rss_mb(self):
        try:
            for line in open(f"/proc/{self.p.pid}/status"):
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) / 1024
        except OSError:
            return None
        return None


def phase(name):
    if os.environ.get("LATENCY_DEBUG"):
        print("== " + name, file=sys.stderr)


def uri(path):
    return Path(path).resolve().as_uri()


def timed(srv, method, params):
    t0 = time.monotonic()
    t, m = srv.response(srv.request(method, params))
    return (t - t0) * 1000, m.get("result")


def pick_body_insertion(text):
    """Where to type inside a body: a statement before the file's last `endfunction` or
    `endtask`. Returns (line, prefix, typed)."""
    lines = text.split("\n")
    for i in range(len(lines) - 1, -1, -1):
        if re.match(r"^\s*(endfunction|endtask)\b", lines[i]):
            return i, "    ", "probe_missing = 1;"
    raise SystemExit("no endfunction/endtask in the file")


def pick_insertion(text):
    """Where to type: before the last `end...` of a module, class or package, with a line that
    makes sense there and names something undeclared (so that every version of the file has a
    problem, and the engine says when it has checked it). Returns (line, prefix, typed)."""
    lines = text.split("\n")
    for i in range(len(lines) - 1, -1, -1):
        s = lines[i].strip()
        m = re.match(r"^(endmodule|endclass|endpackage|endinterface|endprogram)\b", s)
        if m:
            kind = m.group(1)
            if kind == "endclass":
                return i, "  function void probe_f(); int probe_v = ", "probe_missing; endfunction"
            if kind == "endpackage":
                return i, "  localparam int PROBE_P = ", "probe_missing;"
            return i, "  wire probe_w = ", "probe_missing;"
    raise SystemExit("no endmodule/endclass/endpackage in the file")


def own_problems(srv, doc, version, wait):
    """When the problems of that version of the file arrived (`None`: not within `wait` s)."""
    try:
        t, _ = srv.wait(lambda m: m.get("method") == "textDocument/publishDiagnostics" and m["params"]["uri"] == doc and m["params"].get("version") == version, timeout=wait)
        return t
    except (TimeoutError, queue.Empty):
        return None


def percentile(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))] if xs else None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--server", required=True)
    ap.add_argument("--manifest")
    ap.add_argument("--only", help="the configuration (a regular expression matching one name)")
    ap.add_argument("--root")
    ap.add_argument("--build")
    ap.add_argument("--file", required=True, help="the file to edit, relative to the root (or absolute)")
    ap.add_argument("--interval", type=int, default=100, help="ms between typed characters")
    ap.add_argument("--settle", type=int, default=3000, help="ms without problems that ends a wait")
    ap.add_argument("--wait", type=int, default=120, help="s to wait for the problems of a version")
    ap.add_argument("--where", choices=["scope", "body"], default="scope",
                    help="type a declaration before the last end of a scope, or a statement in the last subroutine")
    ap.add_argument("--json", help="write the measurements here too")
    ap.add_argument("--log", help="the engine's log file")
    ap.add_argument("--perf", help="record a perf profile (Linux) of the typing and the declaration into this file")
    a = ap.parse_args()

    env = {}
    if a.manifest:
        import corpus
        m = corpus.manifest(a.manifest)
        found = list(corpus.configs(m, a.only))
        if len(found) != 1:
            raise SystemExit(f"--only matches {len(found)} configurations")
        c, _, root, build = found[0]
        # As run.sh gives them to `dvicode-server check`.
        env["CORPUS"] = os.environ.get("CORPUS", os.path.expanduser("~/.cache/dvicode-corpus"))
        env["UVM_HOME"] = os.environ.get("UVM_HOME", os.path.expanduser("~/corpus/uvm-1.2"))
        for kv in corpus.config_env(c):
            k, v = kv.split("=", 1)
            env[k] = v
        name = c["name"]
    else:
        root, build, name = a.root, a.build or "auto", Path(a.root).name
    root = os.path.abspath(root)
    settings = {"env": env}
    if build not in ("auto", "project", None):
        settings["buildFile"] = os.path.abspath(build)
    path = a.file if os.path.isabs(a.file) else os.path.join(root, a.file)
    text = open(path, encoding="utf-8", errors="replace").read()
    out = {"config": name, "file": a.file, "bytes": len(text)}

    srv = Server(a.server, root, settings, a.log)
    t0 = time.monotonic()
    srv.request("initialize", {
        "processId": os.getpid(), "rootUri": uri(root),
        "workspaceFolders": [{"uri": uri(root), "name": Path(root).name}],
        "initializationOptions": settings,
        "capabilities": {
            "window": {"workDoneProgress": True},
            "workspace": {"configuration": True, "didChangeWatchedFiles": {"dynamicRegistration": False}},
            "textDocument": {"publishDiagnostics": {"versionSupport": True},
                             "completion": {"completionItem": {"snippetSupport": True}},
                             "synchronization": {"didSave": True}},
        },
    })
    srv.wait(lambda m: m.get("id") == 1)
    srv.notify("initialized", {})
    srv.notify("workspace/didChangeConfiguration", {"settings": {"dvicode": settings}})
    # The first build ends with the end of its progress.
    t, _ = srv.wait(lambda m: m.get("method") == "$/progress" and m["params"]["value"].get("kind") == "end")
    phase("built")
    out["first_build_ms"] = round((t - t0) * 1000)
    out["rss_after_build_mb"] = srv.rss_mb()

    doc = uri(path)
    version = 1
    srv.notify("textDocument/didOpen", {"textDocument": {"uri": doc, "languageId": "systemverilog", "version": version, "text": text}})
    lines = text.split("\n")
    line, prefix, typed = pick_body_insertion(text) if a.where == "body" else pick_insertion(text)
    # A position on a word of the file for hover: the first identifier of the line above.
    hover_line = max(0, line - 1)
    mw = re.search(r"[A-Za-z_]\w*", lines[hover_line])
    hover_pos = {"line": hover_line, "character": mw.start() if mw else 0}
    tdoc = {"uri": doc}
    phase("requests on the untouched file")
    out["hover_ms"], _ = timed(srv, "textDocument/hover", {"textDocument": tdoc, "position": hover_pos})
    out["completion_ms"], res = timed(srv, "textDocument/completion", {"textDocument": tdoc, "position": hover_pos})
    out["tokens_ms"], _ = timed(srv, "textDocument/semanticTokens/full", {"textDocument": tdoc})

    prof = None
    if a.perf:
        prof = subprocess.Popen(["perf", "record", "-q", "-F", "999", "-g", "-p", str(srv.p.pid), "-o", a.perf],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1)  # perf attaches
    # Typing: the line goes in empty, then one character at a time.
    version += 1
    srv.notify("textDocument/didChange", {"textDocument": {"uri": doc, "version": version},
                                          "contentChanges": [{"range": {"start": {"line": line, "character": 0}, "end": {"line": line, "character": 0}}, "text": prefix + "\n"}]})
    col = len(prefix)
    completion = []
    pending = []
    t_type = time.monotonic()
    for ch in typed:
        version += 1
        srv.notify("textDocument/didChange", {"textDocument": {"uri": doc, "version": version},
                                              "contentChanges": [{"range": {"start": {"line": line, "character": col}, "end": {"line": line, "character": col}}, "text": ch}]})
        col += 1
        t_last_key = time.monotonic()
        pending.append((time.monotonic(), srv.request("textDocument/completion", {"textDocument": tdoc, "position": {"line": line, "character": col}})))
        deadline = time.monotonic() + a.interval / 1000
        # Collect the answers that come in while the next character waits.
        while pending and time.monotonic() < deadline:
            sent, rid = pending[0]
            try:
                t, _ = srv.response(rid, timeout=max(0.001, deadline - time.monotonic()))
            except (TimeoutError, queue.Empty):
                break
            completion.append((t - sent) * 1000)
            pending.pop(0)
        left = deadline - time.monotonic()
        if left > 0:
            time.sleep(left)
    for sent, rid in pending:
        t, _ = srv.response(rid)
        completion.append((t - sent) * 1000)
    phase("typed")
    # Problems of the last version of the file.
    t = own_problems(srv, doc, version, a.wait)
    phase("problems of the last version")
    out["typing_chars"] = len(typed)
    out["typing_completion_ms"] = {"p50": percentile(completion, 50), "p95": percentile(completion, 95), "max": max(completion)}
    out["typing_problems_after_last_ms"] = t and round((t - t_last_key) * 1000)

    # A declaration: the project's other files are checked again.
    srv.drain()
    srv.backlog.clear()
    version += 1
    decl_line = line + 1
    t_decl = time.monotonic()
    srv.notify("textDocument/didChange", {"textDocument": {"uri": doc, "version": version},
                                          "contentChanges": [{"range": {"start": {"line": decl_line, "character": 0}, "end": {"line": decl_line, "character": 0}}, "text": "  probe_missing_t probe_decl;\n"}]})
    hover_ms, _ = timed(srv, "textDocument/hover", {"textDocument": tdoc, "position": hover_pos})
    t = own_problems(srv, doc, version, a.wait)
    phase("declaration: own problems")
    out["decl_hover_ms"] = hover_ms
    out["decl_own_problems_ms"] = t and round((t - t_decl) * 1000)
    last, files = t or t_decl, set()
    while True:
        try:
            t, m = srv.wait(lambda m: m.get("method") == "textDocument/publishDiagnostics", timeout=a.settle / 1000)
        except (TimeoutError, queue.Empty):
            break
        last = t
        files.add(m["params"]["uri"])
    phase("declaration: project settled")
    out["decl_project_problems_ms"] = round((last - t_decl) * 1000)
    out["decl_files_published"] = len(files)
    out["rss_end_mb"] = srv.rss_mb()
    if prof:
        prof.send_signal(2)
        prof.wait(timeout=60)

    srv.request("shutdown", None)
    srv.notify("exit", None)
    try:
        srv.p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        srv.p.kill()
    for k in ("hover_ms", "completion_ms", "tokens_ms", "decl_hover_ms"):
        out[k] = round(out[k], 1)
    out["typing_completion_ms"] = {k: round(v, 1) for k, v in out["typing_completion_ms"].items()}
    print(json.dumps(out))
    if a.json:
        with open(a.json, "a", encoding="utf-8") as f:
            f.write(json.dumps(out) + "\n")


if __name__ == "__main__":
    main()
