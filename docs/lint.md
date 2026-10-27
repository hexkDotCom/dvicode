# DVICode Lint

DVICode Lint checks SystemVerilog and UVM code for mistakes the compiler does not report and,
if you ask for it, against the [HEXK SystemVerilog/UVM Style Guide][guide]. It is part of the
DVICode engine: the editor shows its findings with the compile problems, and the `dvicode lint`
command runs the same checks in CI.

- **`recommended`** (on by default): a small set of correctness rules that hold on real code,
  such as a `randomize()` whose result is thrown away, or a UVM registration macro that registers
  another class.
- **`hexk`** (opt-in): the checkable rules of the style guide, house style included. It
  includes `recommended`.

Every rule is listed, with examples, in the [rule reference](lint/README.md).

Lint understands the whole project: build files, compile contexts, packages, the class
hierarchy, the UVM library. It reports a finding only when it can prove it. What it cannot
decide is counted as unknown, not reported. Examples: a receiver whose type does not resolve,
or code a project macro produces. `--stats` shows these counts.

## In the editor

Findings appear in the Problems view with the source `dvicode-lint`, next to the compile
problems of the same file. Each one links to its rule's page. The quick fixes (Ctrl+.) offer:

- the rule's fix, where it has a safe or suggested one;
- **Waive** (only where the finding's statement starts its line): inserts
  `// <ID> waived: <reason>` above the line and selects `<reason>` for you to type;
- **Turn off in this file**: inserts `// dvicode-lint: disable-file=<rule> <reason>`.

Settings:

| Setting | Default | |
|---|---|---|
| `dvicode.lint.enable` | `true` | Show lint findings at all. |
| `dvicode.lint.rulesets` | `["recommended"]` | The rulesets used where no `.dvicode/lint.json` chooses them. |

**DVICode: Show Lint Rules** lists the rules of the running engine.

## The command

The engine binary is also the `dvicode` command. The extension ships it, and it can be
installed on its own (see [Installing the command](#installing-the-command)):

```bash
dvicode lint .
```

```text
dvicode lint <dir> [PATH...] [--build FILE] [--env K=V] [--config FILE]
     [--rulesets r1,r2] [--rule ID=LEVEL]... [--format text|json|sarif|junit|github]
     [--output FILE] [--output-sarif FILE] [--fail-on error|warning|info|never]
     [--baseline FILE] [--write-baseline FILE] [--changed-since REF [--changed-exact]]
     [--all-contexts] [--jobs N] [--allow-incomplete] [--no-fail-on-compile-errors]
     [--show-suppressed] [--stats]
dvicode lint --list-rules [--format text|json|md]
dvicode lint --explain ID
dvicode lint --print-config FILE [<dir>]
```

- `<dir>` is the project. It is loaded the way the editor loads it: its build file
  (`--build FILE`, or `--build auto` to configure from the folder), else
  `.dvicode/default.build`, else auto-configuration.
- `PATH...` limits what is **reported** to these files or folders. The project is still
  compiled whole, so headers are read in the context of their package.
- `--all-contexts` lints each file in every compile context that compiles it. By default, a
  file is linted in its first context.
- `--print-config FILE` shows the configuration of one file: where each value comes from, and
  whether the file is linted at all.

Exit status, the first that applies:

| Status | When |
|---|---|
| 3 | internal failure (also: the output cannot be written) |
| 2 | usage or configuration error, an unreadable build file, nothing to lint |
| 4 | analysis incomplete (syntax errors, encrypted code, a budget) unless `--allow-incomplete` |
| 1 | findings at or above `--fail-on` (default `warning`), or compile errors (unless `--no-fail-on-compile-errors`) |
| 0 | otherwise |

Output formats:

- `text`: `path:line:col: level: message [rule/GUIDE-ID]`, the default;
- `json`: a versioned envelope with the findings, their fingerprints, unknowns and completeness;
- `sarif`: SARIF 2.1.0, for GitHub code scanning and other viewers;
- `junit`: JUnit XML, one test suite per rule and one test case per file, for CI dashboards;
- `github`: workflow annotations, shown on the pull request's changed lines.

## Configuration: `.dvicode/lint.json`

A project's configuration lives in `.dvicode/lint.json`. Write it as strict JSON. Put notes in
`comment` fields: at the top level, in a rule's object form, in an override and in a path's
object form. The extension validates the file and completes rule IDs as you type.

```json
{
  "comment": "Lint settings for the bus VIP; owner: DV team",
  "rulesets": ["hexk"],
  "rules": {
    "LNG-10": "error",
    "line-length": { "level": "info", "max": 140, "comment": "wide tables in sequences" }
  },
  "overrides": [
    {
      "paths": ["tb/legacy"],
      "reason": "ported environment; renaming tracked in JIRA-123",
      "rulesets": ["recommended"]
    }
  ],
  "exclude": [{ "path": "vendor", "reason": "third-party VIP" }, "**/*_reg_block.sv"],
  "include": ["../shared/our_pkg"],
  "examples": ["examples"],
  "baseline": ".dvicode/lint-baseline.json",
  "formatter": "dvicode"
}
```

| Key | |
|---|---|
| `rulesets` | `recommended`, `recommended-preview`, `hexk`. Replaces what the configuration above chose. |
| `rules` | By rule ID or style-guide ID (`LNG-10` sets every rule that checks it). The value is a level (`off`, `hint`, `info`, `warning`, `error`) or `{ "level", "comment", <parameters> }`. A rule's own ID wins over its guide ID. |
| `overrides` | `[{ "paths", "rulesets", "rules", "reason", "comment" }]`: changes for some paths, in order. |
| `owned` | The folders that are linted (default: the project). `exclude` and `include` are applied after it, in order, and the last match wins. |
| `exclude` / `include` | Paths or `{ "path", "reason", "comment" }`. Excluded files are still compiled and analysed; they get no findings and no fixes. |
| `examples` | Files where the style guide's example exceptions apply (FIL-3: classes in a single-module example or unit test). |
| `baseline` | A baseline file (see below). |
| `formatter` | `"dvicode"`: the project formats with DVICode's formatter, and the `hexk` rule `formatter-drift` reports lines it would change. |
| `extends` | `"parent"` (default) or `"none"`: start from the defaults instead of the configuration above. |
| `uvm` | `"1.2"` or `"ieee"`. |

How configurations combine for a file:

1. The defaults: `recommended`, or the `dvicode.lint.rulesets` setting in the editor.
2. The project root's `.dvicode/lint.json`, then each nested one on the way to the file's
   folder. A nested one inherits unless it says `"extends": "none"`.
3. Within one file: `rulesets`, then `rules`, then each matching `overrides` entry in order.
4. The command line's `--rulesets`, then its `--rule ID=LEVEL`.

Paths and globs are relative to the folder that holds the `.dvicode` folder, written with `/`.
They support `*`, `**` and `[...]`. A path that names a folder covers what is in it, and `..`
names the folder above. The UVM library, built-in files and files reached through a link out
of the project are never linted unless `include` names them.

The style guide asks for a reason with every exception. With `hexk`, an override that turns
rules down, or an exclusion, written without a `reason` is noted in `lint.json`. The note is a
warning, and it never fails a run.

> **The style guide covers verification code** (testbenches, UVM, VIP), not RTL. If the
> project keeps RTL or generated netlists under its root, keep `hexk` to the verification
> folders. Use `owned`, or an override that keeps `recommended` for the RTL folders.

## Waivers and suppressions

The style guide records each exception as a comment on the code it covers:

```systemverilog
// LNG-10 waived: values do not matter in this smoke test
void'(item.randomize());
```

- The ID is a style-guide rule ID or a lint rule ID. Several IDs can be separated by commas.
- A waiver covers the statement or declaration right after it: a whole function or task, a
  class's header, a case item. Comments and attributes may stand between.
- A waiver without a reason, or with a placeholder (`<reason>`, `TODO`), is reported
  (`waiver-without-reason`).
- So is one that names no known ID (`waiver-unknown-id`).
- A waiver in code that `` `ifdef `` leaves out does nothing.

For code that cannot take a comment per statement (generated code, for example), the tool's
own suppressions turn rules off for a region. They are not the guide's waiver record, and
`hexk` lists them (`suppression-not-waiver`):

```systemverilog
// dvicode-lint: disable=trailing-whitespace,line-length generated by regtool
...
// dvicode-lint: enable=trailing-whitespace,line-length
// dvicode-lint: disable-file=naming-case vendor naming
```

## Adopting lint on existing code

**A baseline** records today's findings so that only new ones are reported:

```bash
dvicode lint . --write-baseline .dvicode/lint-baseline.json
dvicode lint . --baseline .dvicode/lint-baseline.json
```

Name the file under `"baseline"` in `lint.json` and the editor uses it too: listed findings
show as hints. A finding is recognized by its rule, file, enclosing declaration, text and the
lines around it, so it survives edits elsewhere in the file. When equal findings outnumber the
baseline's count, the extra ones are new. The baseline is a migration aid; waivers are the
guide's lasting record.

**Changed code only.** `--changed-since main` reports the findings on lines changed since the
merge base of `main` and `HEAD`, including staged, unstaged and untracked files.
`--changed-exact` compares with `main` itself. Line filtering cannot show a finding that an
edit causes elsewhere (in a subclass, say): lint the whole project where that matters.

## Installing the command

- **With the extension:** the engine in the extension's `server/<platform>` folder is the same
  command. Copy it as `dvicode`.
- **From a release:** each [GitHub release][releases] has `dvicode-<version>-<platform>.tar.gz`
  (`.zip` on Windows) and `SHA256SUMS`, for Linux x64/arm64 (static: any distribution), macOS
  and Windows.
- **With pip:** `pip install dvicode`. The wheels carry the engine, so nothing else is needed,
  and they work on ordinary EDA Linux hosts without root.
- **As a container:** `docker run --rm -v "$PWD:/src" ghcr.io/hexkdotcom/dvicode lint /src`.

## CI recipes

**GitHub Actions** with the [DVICode Lint action][action]. It shows annotations on the pull
request and uploads SARIF to code scanning:

```yaml
- uses: hexkDotCom/dvicode-lint-action@v1
  with:
    path: .
    args: --rulesets recommended
```

Uploading SARIF needs `security-events: write`. Give the job these permissions:

```yaml
jobs:
  lint:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      security-events: write
    steps:
      - uses: actions/checkout@v4
      - uses: hexkDotCom/dvicode-lint-action@v1
```

On pull requests from forks the token is read-only, so the upload cannot work there; the
annotations still show. Turn the upload off for them:
`sarif: ${{ !github.event.pull_request.head.repo.fork }}`.

Without the action (same permissions):

```yaml
- run: pip install dvicode
- run: dvicode lint . --format github --output-sarif dvicode-lint.sarif
- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: dvicode-lint.sarif
```

The `args` input is split into words as a shell would split it, keeping quoted parts together
(`--build "config files/sim.build"`). Variables and wildcards are not expanded. Annotations
use paths from the repository root, also when `path` is a subfolder.

**GitLab CI**:

```yaml
lint:
  image: python:3.12
  script:
    - pip install dvicode
    - dvicode lint . --format junit --output lint.xml
  artifacts:
    when: always
    reports:
      junit: lint.xml
```

**Jenkins** and other CI: `dvicode lint . --format junit --output lint.xml`, then publish the
JUnit report.

**pre-commit**: the repository has a hook (it needs the `dvicode` command, for example
`pip install dvicode`):

```yaml
repos:
  - repo: https://github.com/hexkDotCom/dvicode
    rev: v0.8.0
    hooks:
      - id: dvicode-lint
```

The hook lints the project and reports only the files being committed.

## Other editors

`dvicode --stdio` is the language server: lint findings, diagnostics, navigation, formatting.
Settings go in the client's initialization options as nested objects: `dvicode.lint.enable`
and `dvicode.lint.rulesets` become

```json
{ "lint": { "enable": true, "rulesets": ["recommended", "hexk"] } }
```

- **Neovim** (0.11+): `vim.lsp.config('dvicode', { cmd = { 'dvicode', '--stdio' }, filetypes = { 'systemverilog', 'verilog' }, root_markers = { '.dvicode', '.git' } })`
  then `vim.lsp.enable('dvicode')`.
- **Emacs** (eglot): `(add-to-list 'eglot-server-programs '((verilog-mode) "dvicode" "--stdio"))`.
- **Sublime Text** (LSP package), in `LSP.sublime-settings`:

  ```json
  { "clients": { "dvicode": { "enabled": true, "command": ["dvicode", "--stdio"], "selector": "source.systemverilog" } } }
  ```

Code actions are plain LSP edits in any editor. After a Waive, replace the `<reason>`
placeholder with the reason; VS Code selects it for you.

## Credits

Rule IDs, levels and the descriptions adapted into the rule pages come from the
[HEXK SystemVerilog/UVM Style Guide][guide] (version 0.6). Copyright 2026 HEXK, licensed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), and adapted here. Several rules
follow the open-source linters Verible, svlint and slang in what they check. Their code is not
used.

[guide]: https://github.com/hexkDotCom/sv-uvm-style-guide
[releases]: https://github.com/hexkDotCom/dvicode/releases
[action]: https://github.com/hexkDotCom/dvicode-lint-action
