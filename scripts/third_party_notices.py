#!/usr/bin/env python3
"""Generates THIRD_PARTY_NOTICES.md for what DVICode distributes.

Covers:
  - npm packages bundled into the VS Code extension (runtime dependencies in
    client/package-lock.json, which esbuild inlines into dist/extension.js);
  - Rust crates linked into the static Linux server (normal dependencies of the
    workspace for the x86_64-unknown-linux-musl target);
  - the Rust standard library and the C runtime it links statically (musl, libunwind),
    whose notices Rust publishes as COPYRIGHT-library.html (copied next to the output).

Run where cargo works:
  python3 scripts/third_party_notices.py [--targets T1,T2] [--out FILE] [--rust-std-copy FILE]

--targets lists the Rust targets of the binaries shipped together (crates are combined);
the default is the static Linux server only.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGET = "x86_64-unknown-linux-musl"
LICENSE_PREFIXES = ("license", "licence", "copying", "notice", "copyright", "unlicense")
# License files of code a package bundles, below its top level: libmimalloc-sys compiles
# Microsoft's mimalloc (the default version, v2) into the x86_64 Linux engine.
BUNDLED = {"libmimalloc-sys": ["c_src/mimalloc/v2/LICENSE"]}


def license_files(directory: Path):
    """License and notice files at the top of a package directory, sorted."""
    out = []
    try:
        for p in sorted(directory.iterdir()):
            if p.is_file() and p.name.lower().startswith(LICENSE_PREFIXES):
                out.append(p)
    except FileNotFoundError:
        pass
    return out


def npm_packages():
    lock = json.loads((ROOT / "client" / "package-lock.json").read_text(encoding="utf-8"))
    pkgs = []
    for path, info in lock.get("packages", {}).items():
        if not path or info.get("dev"):
            continue
        name = path.split("node_modules/")[-1]
        pkgs.append(
            {
                "name": name,
                "version": info.get("version", "?"),
                "license": info.get("license", "UNKNOWN"),
                "dir": ROOT / "client" / path,
                "url": f"https://www.npmjs.com/package/{name}",
            }
        )
    return sorted(pkgs, key=lambda p: p["name"])


def rust_packages(targets):
    pkgs = {}
    for target in targets:
        for p in rust_packages_for(target):
            pkgs[(p["name"], p["version"])] = p
    return [pkgs[k] for k in sorted(pkgs)]


def rust_packages_for(target):
    meta = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
            cwd=ROOT / "server",
        )
    )
    by_id = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    members = set(meta["workspace_members"])
    seen, stack = set(), list(members)
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            if any(k["kind"] in (None, "normal") for k in d["dep_kinds"]):
                stack.append(d["pkg"])
    pkgs = []
    for i in seen - members:
        p = by_id[i]
        pkgs.append(
            {
                "name": p["name"],
                "version": p["version"],
                "license": p.get("license") or "UNKNOWN",
                "dir": Path(p["manifest_path"]).parent,
                "url": p.get("repository") or f"https://crates.io/crates/{p['name']}",
            }
        )
    return sorted(pkgs, key=lambda p: (p["name"], p["version"]))


def section(title, intro, pkgs, problems):
    """Package table, then each distinct license text once with the packages using it."""
    lines = [f"## {title}", "", intro, "", "| Package | Version | License |", "|---|---|---|"]
    for p in pkgs:
        lines.append(f"| [{p['name']}]({p['url']}) | {p['version']} | {p['license']} |")
    lines.append("")
    texts = {}
    order = []
    for p in pkgs:
        files = license_files(p["dir"]) + [p["dir"] / b for b in BUNDLED.get(p["name"], []) if (p["dir"] / b).is_file()]
        if not files:
            problems.append(f"{p['name']} {p['version']}: no license file in {p['dir']}")
            continue
        for f in files:
            text = f.read_text(encoding="utf-8", errors="replace").strip()
            key = hashlib.sha256(text.encode()).hexdigest()
            if key not in texts:
                texts[key] = (text, [])
                order.append(key)
            texts[key][1].append(f"{p['name']} {p['version']} ({f.relative_to(p['dir']).as_posix()})")
    for key in order:
        text, users = texts[key]
        lines.append("<details>")
        lines.append(f"<summary>{', '.join(users)}</summary>")
        lines.append("")
        lines.append("```text")
        lines.append(text.replace("```", "'''"))
        lines.append("```")
        lines.append("")
        lines.append("</details>")
        lines.append("")
    return lines


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--targets", default=TARGET, help="comma-separated Rust targets")
    ap.add_argument("--out", default=str(ROOT / "THIRD_PARTY_NOTICES.md"))
    ap.add_argument("--rust-std-copy", default=str(ROOT / "client" / "licenses" / "rust-std-library.html"))
    args = ap.parse_args()

    problems = []
    npm = npm_packages()
    targets = [t for t in args.targets.split(",") if t]
    rust = rust_packages(targets)
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    rustc_version = subprocess.check_output(["rustc", "--version"], text=True).strip()
    std_notice = Path(sysroot) / "share" / "doc" / "rust" / "COPYRIGHT-library.html"
    if std_notice.is_file():
        dest = Path(args.rust_std_copy)
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(std_notice.read_bytes())
    else:
        problems.append(f"Rust standard library notices not found at {std_notice}")

    out = [
        "# Third-party notices",
        "",
        "DVICode is licensed under the Apache License 2.0 (see LICENSE and NOTICE). It includes the",
        "third-party software listed below, each under its own license. This file is generated by",
        "`scripts/third_party_notices.py`; do not edit it by hand.",
        "",
    ]
    out += section(
        "VS Code extension (bundled into dist/extension.js)",
        "These npm packages are compiled into the extension bundle.",
        npm,
        problems,
    )
    out += section(
        "Language server (server/*/dvicode-server)",
        "These Rust crates are statically linked into the server binaries (targets "
        + ", ".join(f"`{t}`" for t in targets) + ").",
        rust,
        problems,
    )
    out += [
        "## Rust standard library and C runtime",
        "",
        f"The server binaries statically link the Rust standard library ({rustc_version}) and,",
        "for the Linux (musl) targets, the musl C library and libunwind. Their copyright and license",
        "notices are published by the Rust project as COPYRIGHT-library.html; a copy ships",
        "with the extension as `licenses/rust-std-library.html` and is also available at",
        "https://github.com/rust-lang/rust/blob/master/COPYRIGHT.",
        "",
    ]
    Path(args.out).write_text("\n".join(out), encoding="utf-8", newline="\n")
    print(f"wrote {args.out}: {len(npm)} npm packages, {len(rust)} Rust crates")
    for p in problems:
        print(f"WARNING: {p}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
