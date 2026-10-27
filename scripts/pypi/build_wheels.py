#!/usr/bin/env python3
"""Builds the `dvicode` Python wheels: one per platform, each carrying that platform's engine as
the `dvicode` command (`pip install dvicode`, then `dvicode lint .`).

Usage: build_wheels.py --engines DIR --version X.Y.Z --out DIR [--readme FILE] [--license FILE]
       [--notice FILE] [--notices DIR]

With --notices, DIR/<platform>/ holds that platform's THIRD_PARTY_NOTICES.md and
licenses/rust-std-library.html (scripts/third_party_notices.py --cli), which the wheel carries
with LICENSE and NOTICE.

DIR holds the engines as the release workflow downloads them: DIR/engine-<platform>/dvicode-server
(dvicode-server.exe on Windows), for the platforms below. The wheels need nothing but the
standard library to build, and nothing at all to run: the engine is static (Linux) or
self-contained (Windows, macOS).
"""

import argparse
import base64
import hashlib
import io
import os
import stat
import sys
import zipfile

# Release platform -> wheel platform tag. The Linux engines are static (musl): they run on any
# glibc or musl system of their CPU, which these tags say.
PLATFORMS = {
    "linux-x64": "manylinux_2_17_x86_64.manylinux2014_x86_64.musllinux_1_1_x86_64",
    "linux-arm64": "manylinux_2_17_aarch64.manylinux2014_aarch64.musllinux_1_1_aarch64",
    "win32-x64": "win_amd64",
    "darwin-arm64": "macosx_11_0_arm64",
    "darwin-x64": "macosx_10_12_x86_64",
}

INIT = '''"""DVICode: a SystemVerilog/UVM language server and linter (https://github.com/hexkDotCom/dvicode)."""

__version__ = "{version}"
'''

MAIN = '''"""Runs the DVICode engine this package carries: `dvicode lint .`, `dvicode check .`."""

import os
import subprocess
import sys


def binary():
    name = "dvicode.exe" if os.name == "nt" else "dvicode"
    return os.path.join(os.path.dirname(os.path.abspath(__file__)), "bin", name)


def main():
    exe = binary()
    args = [exe] + sys.argv[1:]
    if os.name == "nt":
        # No exec on Windows: run it and pass its exit status on.
        try:
            sys.exit(subprocess.call(args))
        except KeyboardInterrupt:
            sys.exit(130)
    os.execv(exe, args)


if __name__ == "__main__":
    main()
'''


def metadata(version, readme):
    lines = [
        "Metadata-Version: 2.1",
        "Name: dvicode",
        f"Version: {version}",
        "Summary: SystemVerilog/UVM linter and language server (DVICode): `dvicode lint` for CI",
        "Home-page: https://github.com/hexkDotCom/dvicode",
        "Author: HEXK",
        "License: Apache-2.0",
        "Project-URL: Documentation, https://github.com/hexkDotCom/dvicode/blob/main/docs/lint.md",
        "Project-URL: Rules, https://github.com/hexkDotCom/dvicode/blob/main/docs/lint/README.md",
        "Keywords: systemverilog,uvm,lint,verification,eda",
        "Classifier: License :: OSI Approved :: Apache Software License",
        "Classifier: Topic :: Software Development :: Quality Assurance",
        "Classifier: Topic :: Scientific/Engineering :: Electronic Design Automation (EDA)",
        "Requires-Python: >=3.7",
        "Description-Content-Type: text/markdown",
        "",
        readme,
    ]
    return "\n".join(lines)


def record_line(name, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{name},sha256={digest},{len(data)}"


def build(platform, tag, engine, version, out, readme, license_text, notice_text, notices=None):
    exe = "dvicode.exe" if platform.startswith("win32") else "dvicode"
    dist = f"dvicode-{version}.dist-info"
    files = [
        ("dvicode/__init__.py", INIT.format(version=version).encode(), 0o644),
        ("dvicode/__main__.py", MAIN.encode(), 0o644),
        (f"dvicode/bin/{exe}", open(engine, "rb").read(), 0o755),
        (f"{dist}/METADATA", metadata(version, readme).encode(), 0o644),
        (f"{dist}/WHEEL", f"Wheel-Version: 1.0\nGenerator: dvicode build_wheels.py\nRoot-Is-Purelib: false\nTag: py3-none-{tag}\n".encode(), 0o644),
        (f"{dist}/entry_points.txt", b"[console_scripts]\ndvicode = dvicode.__main__:main\n", 0o644),
        (f"{dist}/licenses/LICENSE", license_text, 0o644),
        (f"{dist}/licenses/NOTICE", notice_text, 0o644),
    ]
    if notices:
        d = os.path.join(notices, platform)
        for rel in ("THIRD_PARTY_NOTICES.md", "licenses/rust-std-library.html"):
            files.append((f"{dist}/licenses/{rel}", open(os.path.join(d, rel), "rb").read(), 0o644))
    record = [record_line(n, d) for n, d, _ in files] + [f"{dist}/RECORD,,"]
    files.append((f"{dist}/RECORD", ("\n".join(record) + "\n").encode(), 0o644))
    path = os.path.join(out, f"dvicode-{version}-py3-none-{tag}.whl")
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data, mode in files:
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.external_attr = (stat.S_IFREG | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, data)
    return path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--engines", required=True)
    ap.add_argument("--version", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--readme", default=None)
    ap.add_argument("--license", default="LICENSE")
    ap.add_argument("--notice", default="NOTICE")
    ap.add_argument("--notices", default=None)
    ap.add_argument("--platforms", default=",".join(PLATFORMS))
    a = ap.parse_args()
    readme = open(a.readme, encoding="utf-8").read() if a.readme else "DVICode Lint: `dvicode lint <project>`."
    license_text = open(a.license, "rb").read()
    notice_text = open(a.notice, "rb").read()
    os.makedirs(a.out, exist_ok=True)
    built = 0
    for platform in a.platforms.split(","):
        d = os.path.join(a.engines, f"engine-{platform}")
        exe = os.path.join(d, "dvicode-server.exe" if platform.startswith("win32") else "dvicode-server")
        if not os.path.exists(exe):
            print(f"build_wheels: no engine for {platform} at {exe}", file=sys.stderr)
            return 1
        print(build(platform, PLATFORMS[platform], exe, a.version, a.out, readme, license_text, notice_text, a.notices))
        built += 1
    return 0 if built else 1


if __name__ == "__main__":
    sys.exit(main())
