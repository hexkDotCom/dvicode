#!/usr/bin/env python3
"""Helpers for run.sh: reads the manifest, turns `dvicode-server check` output into JSON,
generates sweep queries, and compares results with the baseline. Standard library only."""

import argparse
import glob
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))

PROBLEM = re.compile(r"^(.*?):(\d+):(\d+): (Error|Warning|Information|Hint): (?:([A-Z][A-Z0-9_]+): )?(.*)$")
STATUS = re.compile(
    r"dvicode check: (\d+) top files, (\d+) files, (\d+) declarations, (\d+) macros, (\d+) packages, "
    r"(\d+) classes, (\d+) design elements; config (\d+) ms, build (\d+) ms")
TOTALS = re.compile(r"dvicode check: (\d+) errors, (\d+) warnings \((\d+) ms for diagnostics\) \[(.*)\]")
WINDOWS_PATH = re.compile(r"^(?:[A-Za-z]:[\\/]|\\\\)")
SIGNALS = {4: "SIGILL", 6: "SIGABRT", 7: "SIGBUS", 8: "SIGFPE", 9: "SIGKILL", 11: "SIGSEGV", 15: "SIGTERM"}


def expand(s):
    os.environ.setdefault("REPO", REPO)
    return os.path.expandvars(s)


def wsl_path(s):
    """A Windows path (D:\\work\\x, D:/work/x, \\\\server\\share) as a Linux path, with wslpath."""
    try:
        return subprocess.run(["wslpath", "-u", s], capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        if s[1:2] == ":":
            return "/mnt/" + s[0].lower() + s[2:].replace("\\", "/")
        sys.exit(f"corpus.py: cannot convert the Windows path {s}")


def local_path(s, base):
    """A path from a manifest: variables expanded, Windows paths converted, relative to base."""
    s = os.path.expanduser(expand(s))
    if WINDOWS_PATH.match(s):
        s = wsl_path(s)
    return os.path.normpath(os.path.join(base, s))


def manifest(path):
    with open(path, encoding="utf-8") as f:
        m = json.load(f)
    # Build files and relative project paths resolve against the manifest's folder.
    m["_dir"] = os.path.dirname(os.path.abspath(path))
    return m


def project_dir(m, name):
    p = m["projects"][name]
    return local_path(p["path"], m["_dir"]) if "path" in p else os.path.join(os.environ["CORPUS"], name)


def configs(m, only):
    rx = re.compile(only) if only else None
    for c in m["configurations"]:
        if rx and not rx.search(c["name"]):
            continue
        p = m["projects"][c["project"]]
        d = project_dir(m, c["project"])
        if "dir" in c:
            d = os.path.join(d, c["dir"])
        b = c.get("build", "auto")
        if b not in ("auto", "project"):
            b = local_path(b, m["_dir"])
        yield c, p, d, b


def cmd_configs(a):
    """name, project, directory, build file, timeout, optional, --env arguments (tab-separated)."""
    m = manifest(a.manifest)
    for c, p, d, b in configs(m, a.only):
        env = [f"{k}={expand(v)}" for k, v in sorted(c.get("env", {}).items())]
        print("\t".join([c["name"], c["project"], d, b, str(c.get("timeout", m.get("timeout", 600))),
                         "1" if p.get("optional") else "0", " ".join(env)]))


def cmd_projects(a):
    """name, url, commit, submodules (comma-separated), for projects that are cloned."""
    m = manifest(a.manifest)
    for n, p in m["projects"].items():
        if "url" in p:
            print("\t".join([n, p["url"], p["commit"], ",".join(p.get("submodules", []))]))


def cmd_locals(a):
    """name, directory, optional, for projects that are used where they are (`path`)."""
    m = manifest(a.manifest)
    for n, p in m["projects"].items():
        if "path" in p:
            print("\t".join([n, project_dir(m, n), "1" if p.get("optional") else "0"]))


def cmd_setup(a):
    """The `// corpus-setup:` lines of the build files used by the selected configurations."""
    m = manifest(a.manifest)
    seen = set()
    for c, p, d, b in configs(m, a.only):
        if b in ("auto", "project") or not os.path.exists(b):
            continue
        with open(b, encoding="utf-8") as f:
            for line in f:
                mm = re.match(r"\s*//\s*corpus-setup:\s*(.*\S)", line)
                if mm and (c["project"], mm.group(1)) not in seen:
                    seen.add((c["project"], mm.group(1)))
                    print(f"{c['project']}\t{mm.group(1)}")


def short(path):
    for base, name in ((os.environ.get("CORPUS", ""), "$CORPUS"), (os.environ.get("UVM_HOME", ""), "$UVM_HOME"),
                       (REPO, "$REPO"), (os.environ.get("HOME", ""), "~")):
        if base and path.startswith(base.rstrip("/") + "/"):
            return name + path[len(base.rstrip("/")):]
    return path


def crash_info(code, stderr):
    """(signal name, panic text) from an exit status and stderr."""
    sig = None
    if code is not None and code > 128 and code - 128 in SIGNALS:
        sig = SIGNALS[code - 128]
    panic = None
    for line in stderr.splitlines():
        if "internal error in thread" in line or "panicked at" in line or "has overflowed its stack" in line:
            panic = line.split("] ", 1)[-1][:500]
            break
    return sig, panic


def cmd_result(a):
    with open(a.stdout, encoding="utf-8", errors="replace") as f:
        out = f.read()
    with open(a.stderr, encoding="utf-8", errors="replace") as f:
        err = f.read()
    r = {"config": a.config, "project": a.project, "commit": a.commit, "exit": a.exit, "wall_s": round(a.wall, 2),
         "timeout": a.timed_out, "signal": None, "panic": None, "completed": False}
    sig, panic = crash_info(a.exit, err)
    r["signal"], r["panic"] = sig, panic
    m = STATUS.search(err)
    if m:
        keys = ["top_files", "files", "declarations", "macros", "packages", "classes", "design_elements", "config_ms", "build_ms"]
        r.update(dict(zip(keys, map(int, m.groups()))))
    m = TOTALS.search(err)
    if m:
        r["completed"] = True
        r["errors"], r["warnings"], r["diag_ms"] = int(m.group(1)), int(m.group(2)), int(m.group(3))
        r["total"] = r["errors"] + r["warnings"]
        r["codes"] = {k: int(v) for k, v in re.findall(r'\("([^"]*)", (\d+)\)', m.group(4))}
    r["build_config_problems"] = len(re.findall(r": build config: ", err))
    per_file = {}
    for line in out.splitlines():
        mm = PROBLEM.match(line)
        if mm:
            k = short(mm.group(1))
            per_file[k] = per_file.get(k, 0) + 1
    r["top_problem_files"] = sorted(per_file.items(), key=lambda kv: (-kv[1], kv[0]))[:20]
    with open(a.output, "w", encoding="utf-8") as f:
        json.dump(r, f, indent=1)
        f.write("\n")
    state = "TIMEOUT" if a.timed_out else sig or ("PANIC" if panic else ("ok" if r["completed"] else f"exit {a.exit}"))
    print(f"{a.config:28} {state:8} {r.get('total', '-'):>6} problems  {r.get('files', '-'):>5} files  "
          f"build {r.get('build_ms', '-')} ms  {dict(list(r.get('codes', {}).items())[:4])}")


# --- sweep -------------------------------------------------------------------------------

KEYWORDS = set("""
begin end module endmodule function endfunction task endtask class endclass package endpackage
interface endinterface if else for foreach while do return input output inout logic bit int
integer string void virtual extends import export typedef enum struct union packed parameter
localparam assign always always_ff always_comb always_latch initial posedge negedge or and not
case endcase default generate endgenerate genvar this super new null static automatic const
local protected rand randc constraint covergroup endgroup coverpoint bins wire reg byte
shortint longint unsigned signed real time property endproperty sequence endsequence assert
assume cover fork join join_any join_none wait disable forever repeat break continue type
""".split())
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_$]*")


def strip_line(line):
    """Blanks out strings and // comments, keeping columns."""
    out, i, n = [], 0, len(line)
    while i < n:
        ch = line[i]
        if ch == '"':
            j = i + 1
            while j < n and line[j] != '"':
                j += 2 if line[j] == "\\" else 1
            out.append(" " * (min(j + 1, n) - i))
            i = j + 1
        elif line.startswith("//", i):
            break
        else:
            out.append(ch)
            i += 1
    return "".join(out)


def cmd_queries(a):
    files = [l.strip() for l in open(a.files, encoding="utf-8") if l.strip() and not l.startswith("==")]
    skip = [s.rstrip("/") + "/" for s in a.skip if s]
    nq = nf = 0
    with open(a.output, "w", encoding="utf-8") as q:
        for path in files:
            if any(path.startswith(s) for s in skip) or not os.path.isfile(path):
                continue
            nf += 1
            q.write(f"--outline {path}\n")
            nq += 1
            try:
                lines = open(path, encoding="utf-8", errors="replace").read().split("\n")
            except OSError:
                continue
            positions = []
            in_block = False
            for ln, text in enumerate(lines, 1):
                # Track block comments roughly, so positions are mostly in code.
                if in_block:
                    if "*/" not in text:
                        continue
                    text = " " * (text.index("*/") + 2) + text[text.index("*/") + 2:]
                    in_block = False
                code = strip_line(text)
                if "/*" in code:
                    k = code.index("/*")
                    if "*/" not in code[k:]:
                        in_block = True
                    code = code[:k]
                if ln % a.every or len(positions) >= a.cap:
                    continue
                for m in IDENT.finditer(code):
                    if m.group(0) in KEYWORDS or (m.start() > 0 and code[m.start() - 1] in "`$'"):
                        continue
                    positions.append((ln, m.start() + 1, m.end() + 1))
                    if len(positions) >= a.cap:
                        break
            for i, (ln, c0, c1) in enumerate(positions):
                q.write(f"--hover {path}:{ln}:{c0}\n--goto {path}:{ln}:{c0}\n--complete {path}:{ln}:{c1}\n")
                nq += 3
                if i % a.refs_every == 0:
                    q.write(f"--refs {path}:{ln}:{c0}\n")
                    nq += 1
    print(f"{nq} queries for {nf} files", file=sys.stderr)


def parse_sweep(stdout):
    """[(query, ms or None, error or None)] from the output of one `check --queries` run."""
    res, cur, err = [], None, None
    for line in stdout.splitlines():
        if line.startswith("== "):
            if cur is not None:  # previous query never finished (should not happen)
                res.append((cur, None, err))
            cur, err = line[3:], None
        elif line.startswith("!! internal error: ") and cur is not None:
            err = line[len("!! internal error: "):]
        elif line.startswith("-- ") and line.endswith(" ms") and cur is not None:
            res.append((cur, int(line[3:-3]), err))
            cur, err = None, None
    return res, cur


def cmd_sweep_part(a):
    """Folds one run into the sweep state; prints the number of queries done (for restarting)."""
    st = json.load(open(a.state)) if os.path.exists(a.state) else {"done": 0, "crashes": [], "times": {}, "slowest": []}
    out = open(a.stdout, encoding="utf-8", errors="replace").read()
    err = open(a.stderr, encoding="utf-8", errors="replace").read()
    res, unfinished = parse_sweep(out)
    for q, ms, e in res:
        kind = q.split(" ", 1)[0]
        t = st["times"].setdefault(kind, {"count": 0, "total_ms": 0, "max_ms": 0})
        t["count"] += 1
        t["total_ms"] += ms or 0
        t["max_ms"] = max(t["max_ms"], ms or 0)
        st["slowest"].append([short_query(q), ms or 0])
        if e is not None:
            st["crashes"].append({"query": short_query(q), "kind": "panic", "message": e})
    st["slowest"] = sorted(st["slowest"], key=lambda x: -x[1])[:15]
    st["done"] += len(res)
    if a.exit not in (0, 1) or a.timed_out:
        sig, panic = crash_info(a.exit, err)
        if "query timeout after" in err or a.timed_out:
            kind = "timeout"
        elif sig:
            kind = sig
        elif panic:
            kind = "panic"
        else:
            kind = f"exit {a.exit}"
        other = [l for l in err.splitlines() if not STATUS.search(l) and not TOTALS.search(l)]
        msg = panic or next((l for l in other if "query timeout" in l), "") or "\n".join(other)[-300:]
        st["crashes"].append({"query": short_query(unfinished) if unfinished else "(project build)", "kind": kind, "message": msg})
        st["done"] += 1  # skip the query that crashed
        if unfinished is None:
            st["build_failed"] = True
    json.dump(st, open(a.state, "w"), indent=1)
    print(st["done"])


def short_query(q):
    if not q:
        return q
    k, _, arg = q.partition(" ")
    return f"{k} {short(arg)}"


def cmd_sweep_result(a):
    st = json.load(open(a.state)) if os.path.exists(a.state) else {"done": 0, "crashes": [], "times": {}, "slowest": []}
    r = {"config": a.config, "queries": a.total, "done": min(st["done"], a.total), "wall_s": round(a.wall, 1),
         "crashes": st["crashes"], "times": st["times"], "slowest": st["slowest"]}
    json.dump(r, open(a.output, "w"), indent=1)
    print(f"{a.config:28} sweep {a.total} queries, {len(st['crashes'])} crashes, {a.wall:.0f} s")
    for c in st["crashes"][:10]:
        print(f"    {c['kind']}: {c['query']}: {c['message'][:160]}")


# --- baseline and compare ------------------------------------------------------------------

def load_results(d, only=""):
    res = {}
    rx = re.compile(only) if only else None
    for p in sorted(glob.glob(os.path.join(d, "*.json"))):
        if p.endswith(".sweep.json") or os.path.basename(p).startswith("_"):
            continue
        r = json.load(open(p))
        if rx and not rx.search(r["config"]):
            continue
        sp = p[:-5] + ".sweep.json"
        if os.path.exists(sp):
            r["sweep"] = json.load(open(sp))
        res[r["config"]] = r
    return res


def failed(r):
    if r.get("timeout"):
        return "timeout"
    if r.get("signal"):
        return r["signal"]
    if r.get("panic"):
        return "panic: " + r["panic"]
    if not r.get("completed"):
        return f"did not complete (exit {r.get('exit')})"
    return None


def cmd_baseline(a):
    res = load_results(a.results, a.only)
    old = json.load(open(a.output)) if os.path.exists(a.output) and a.merge else {"configurations": {}}
    for n, r in res.items():
        if failed(r):
            print(f"warning: {n}: {failed(r)}; recorded anyway", file=sys.stderr)
        old["configurations"][n] = {
            "commit": r.get("commit"), "total": r.get("total"), "errors": r.get("errors"), "warnings": r.get("warnings"),
            "codes": r.get("codes", {}), "files": r.get("files"), "build_ms": r.get("build_ms"), "config_ms": r.get("config_ms"),
        }
    old["configurations"] = dict(sorted(old["configurations"].items()))
    with open(a.output, "w", encoding="utf-8", newline="\n") as f:
        json.dump(old, f, indent=1)
        f.write("\n")
    print(f"{a.output}: {len(res)} configurations recorded")


def cmd_compare(a):
    if not os.path.exists(a.baseline):
        sys.exit(f"compare: no baseline at {a.baseline}; record one with `run.sh baseline`")
    base = json.load(open(a.baseline))["configurations"]
    res = load_results(a.results, a.only)
    if a.only:
        base = {n: b for n, b in base.items() if re.search(a.only, n)}
    bad, notes = [], []
    for n, r in sorted(res.items()):
        why = failed(r)
        if why:
            bad.append(f"{n}: {why}")
        for c in (r.get("sweep") or {}).get("crashes", []):
            bad.append(f"{n}: sweep {c['kind']} in {c['query']}: {c['message'][:200]}")
        b = base.get(n)
        if b is None:
            notes.append(f"{n}: not in the baseline (total {r.get('total')})")
            continue
        if why:
            continue
        new_codes = sorted(set(r.get("codes", {})) - set(b.get("codes", {})))
        if new_codes:
            bad.append(f"{n}: new problem codes {new_codes}")
        if (r.get("total") or 0) > (b.get("total") or 0):
            bad.append(f"{n}: problems grew from {b.get('total')} to {r.get('total')} {diff_codes(b, r)}")
        elif (r.get("total") or 0) < (b.get("total") or 0):
            notes.append(f"{n}: problems fell from {b.get('total')} to {r.get('total')} {diff_codes(b, r)} (update the baseline)")
        bt, rt = (b.get("build_ms") or 0) + (b.get("config_ms") or 0), (r.get("build_ms") or 0) + (r.get("config_ms") or 0)
        if bt and rt > 2 * bt and rt - bt > a.slack_ms:
            bad.append(f"{n}: build time {rt} ms is more than twice the baseline's {bt} ms")
    for n in sorted(set(base) - set(res)):
        notes.append(f"{n}: no result (not run)")
    for x in notes:
        print("note:", x)
    for x in bad:
        print("FAIL:", x)
    print(f"compare: {len(res)} results, {len(bad)} regressions")
    sys.exit(1 if bad else 0)


def diff_codes(b, r):
    bc, rc = b.get("codes", {}), r.get("codes", {})
    d = {k: rc.get(k, 0) - bc.get(k, 0) for k in set(bc) | set(rc) if rc.get(k, 0) != bc.get(k, 0)}
    return "(" + ", ".join(f"{k} {v:+d}" for k, v in sorted(d.items())) + ")" if d else ""


def cmd_table(a):
    res = load_results(a.results)
    print(f"{'configuration':28} {'state':10} {'problems':>8} {'files':>6} {'build ms':>9}  top codes")
    for n, r in sorted(res.items()):
        st = failed(r) or "ok"
        sw = r.get("sweep")
        if sw:
            st += f"/sweep {len(sw['crashes'])}"
        codes = ", ".join(f"{k} {v}" for k, v in list(r.get("codes", {}).items())[:3])
        print(f"{n:28} {st[:10]:10} {r.get('total', '-'):>8} {r.get('files', '-'):>6} {r.get('build_ms', '-'):>9}  {codes}")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("configs"); s.add_argument("manifest"); s.add_argument("--only", default=""); s.set_defaults(f=cmd_configs)
    s = sub.add_parser("projects"); s.add_argument("manifest"); s.set_defaults(f=cmd_projects)
    s = sub.add_parser("locals"); s.add_argument("manifest"); s.set_defaults(f=cmd_locals)
    s = sub.add_parser("setup"); s.add_argument("manifest"); s.add_argument("--only", default=""); s.set_defaults(f=cmd_setup)
    s = sub.add_parser("result")
    for k in ("config", "project", "commit", "stdout", "stderr", "output"):
        s.add_argument("--" + k, default="")
    s.add_argument("--exit", type=int, default=0); s.add_argument("--wall", type=float, default=0)
    s.add_argument("--timed-out", action="store_true"); s.set_defaults(f=cmd_result)
    s = sub.add_parser("queries"); s.add_argument("--files", required=True); s.add_argument("--output", required=True)
    s.add_argument("--every", type=int, default=20); s.add_argument("--cap", type=int, default=30)
    s.add_argument("--refs-every", type=int, default=3); s.add_argument("--skip", action="append", default=[])
    s.set_defaults(f=cmd_queries)
    s = sub.add_parser("sweep-part")
    for k in ("state", "stdout", "stderr"):
        s.add_argument("--" + k, required=True)
    s.add_argument("--exit", type=int, default=0); s.add_argument("--timed-out", action="store_true"); s.set_defaults(f=cmd_sweep_part)
    s = sub.add_parser("sweep-result")
    for k in ("state", "config", "output"):
        s.add_argument("--" + k, required=True)
    s.add_argument("--total", type=int, default=0); s.add_argument("--wall", type=float, default=0); s.set_defaults(f=cmd_sweep_result)
    s = sub.add_parser("baseline"); s.add_argument("results"); s.add_argument("--output", required=True)
    s.add_argument("--merge", action="store_true"); s.add_argument("--only", default=""); s.set_defaults(f=cmd_baseline)
    s = sub.add_parser("compare"); s.add_argument("results"); s.add_argument("--baseline", required=True)
    s.add_argument("--slack-ms", type=int, default=500); s.add_argument("--only", default=""); s.set_defaults(f=cmd_compare)
    s = sub.add_parser("table"); s.add_argument("results"); s.set_defaults(f=cmd_table)
    a = ap.parse_args()
    a.f(a)


if __name__ == "__main__":
    main()
