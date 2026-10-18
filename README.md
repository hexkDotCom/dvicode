# DVICode (DV Intelligence)

SystemVerilog/UVM IDE features for VS Code, backed by a Rust language server that runs where
your files and tools are: on this machine, in WSL2, or on a remote Linux host over SSH.

The server is a single 3 MB static binary: nothing to install on the Linux side, and the
extension deploys it for you.

```
 VS Code on Windows / macOS / Linux             Linux (WSL2, remote host, or local)
 ┌──────────────────────────────┐   LSP over    ┌───────────────────────────────────┐
 │ dvicode extension            │  stdio of     │ dvicode-server (Rust, static)     │
 │  launcher: local|wsl|ssh|tcp │  wsl.exe/ssh  │  build file → compile order       │
 │  path mapping, dvicode:// fs │ ────────────▶ │  preprocessor, tolerant parser    │
 │  hierarchy + files views     │               │  index, resolution, IDE features  │
 └──────────────────────────────┘               └───────────────────────────────────┘
```

## Features

| Area | What works |
|---|---|
| Project | Build files in the simulator argument-file syntax (`.dvicode/default.build`): `-f/-F`, `+incdir+`, `+define+`, `-y/-v/+libext+`, `-top`, `-uvm`, plus `+dvicode_init`, `+dvicode_setenv`, `+dvicode_skip_compile` and other `+dvicode_*` directives, env vars, wildcards, several invocations and configurations. Auto-configuration when there is no build file. See [docs/build-files.md](docs/build-files.md). |
| Preprocessor | `` `define `` with arguments/defaults, `` `` `` pasting, `` `" `` stringification, `` `include `` (incdirs, include guards), `` `ifdef/`ifndef/`elsif/`else `` (also 1800-2023 expressions), `` `undef ``, `` `__FILE__/`__LINE__ ``, encrypted `pragma protect` regions skipped. Every token remembers where it came from. |
| Problems | Preprocessing (missing include, undefined macro, macro arguments), syntax (missing `;`, unbalanced `end*`), semantic: `NON_EXISTING_TYPE`, `UNDECLARED_IDENTIFIER`, `NOT_A_MEMBER`, `NON_EXISTING_PACKAGE`, `NON_EXISTING_PORT`, `NON_EXISTING_DESIGN_ELEMENT`. Zero false positives on UVM 1.2, the UBUS example and hexk-sv-lib. |
| Navigation | Go to definition/declaration/type definition/implementation (extern bodies, overrides, subclasses), find references, document highlight, rename, macro definitions, `` `include `` targets, port connections (`.clk(...)`), hierarchical names, `this`/`super`, `pkg::name`, `C#(T)::type_id`, members through variables and function results, inheritance across packages. Call hierarchy (Show Call Hierarchy, incoming and outgoing calls) for functions and tasks, including extern method bodies, `super.f()` and calls from initial blocks. |
| Editing | Hover with signature, container, location and doc comment (and macro expansions), completion (scope, members after `.`/`::`, macros after `` ` ``, module auto-instantiation template, keywords, snippets), signature help (functions, tasks, macros). Quick fixes for `UNDECLARED_IDENTIFIER`/`NON_EXISTING_TYPE`: "Did you mean" (visible names within a small edit distance, inherited members included), "Import package" (packages that declare the name), "Declare variable". |
| Views | Outline, workspace symbols (`class:`, `module:`, `package:`, `macro:`, `extends:`, `file:` filters), semantic highlighting (inactive `` `ifdef `` code shown as comments), folding, type hierarchy, Design Hierarchy view, Verification Hierarchy view, Compiled Files view, Expand Macro, Show Including File. |
| UVM | Verification Hierarchy view: the component tree of the `+UVM_TESTNAME` test (or of a test picked in the view, or of all tests), built from `T::type_id::create("name", this)` and `new("name", this)` in `build_phase` and constructors of each class and its base classes (extern bodies included). Components are labelled test/env/agent/driver/monitor/sequencer/scoreboard by their UVM base class; TLM ports are shown, configuration objects created with a parent are not. Click a component to go to where it is created, or to its class. |
| Remote | Server in WSL (`\\wsl.localhost\...` folders, or Windows drive folders mapped to `/mnt/<d>`), over SSH, through a custom launch command (`bsub`, `qrsh`, ...), or attached over TCP. `dvicode://` folders: files that exist only on the server, edited through the server connection. The bundled Linux server is deployed automatically. |

## Quick start

1. Install the extension: `code --install-extension client/dvicode-0.1.0.vsix`.
2. Open a folder with SystemVerilog sources.
   - **Windows, files in WSL** (`\\wsl.localhost\Ubuntu\home\me\proj`): works out of the box.
   - **Windows, files on a Windows drive**: the server runs in your default WSL distribution
     and sees the folder as `/mnt/<drive>/...`.
   - **Files on a remote Linux host**: run **DVICode: Open Folder on the Server...** and pick
     an SSH host (key or agent authentication), or set `dvicode.server.mode` to `ssh` and
     `dvicode.server.sshHost`, plus `dvicode.pathMappings` if the files are also mounted locally.
   - **Remote-SSH / Remote-WSL windows**: the extension runs on the remote side next to the
     server (no mapping needed).
3. Optional: add `.dvicode/default.build`, see [docs/build-files.md](docs/build-files.md).
   Without one,
   the project is configured automatically.

Try it on [examples/demo-project](examples/demo-project): `.dvicode/errors.build` adds a file
with one deliberate mistake per line.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `dvicode.server.mode` | `auto` | `auto`, `local`, `wsl`, `ssh`, `tcp`, `custom` |
| `dvicode.server.wslDistro` | | WSL distribution (default: the folder's or the default one) |
| `dvicode.server.sshHost`, `.sshArgs`, `.sshCommand` | | SSH destination and options |
| `dvicode.server.launchCommand` | `${SERVER_CMD}` | Wrapper for `custom`, e.g. `ssh farm bsub -Is ${SERVER_CMD}` |
| `dvicode.server.tcpAddress`, `.tcpToken` | `127.0.0.1:7777` | For `dvicode-server --listen HOST:PORT --token T` |
| `dvicode.server.path` | | Server binary on the target (default: bundled, auto-deployed to `~/.dvicode/server/`) |
| `dvicode.server.autoDeploy` | `true` | Copy the bundled server to WSL/SSH targets |
| `dvicode.pathMappings` | `[]` | `[{ "local": "Z:\\proj", "remote": "/home/me/proj" }]` |
| `dvicode.buildFile` | | Build configuration (relative to the folder) |
| `dvicode.env` | `{}` | Variables for `$VAR` expansion in build files |
| `dvicode.uvmHome` | | UVM location for `-uvm` when `$UVM_HOME` is not set |
| `dvicode.maxProblems` | `1000` | Problems published after a full build (open files: all) |

## Repository layout

```
server/                    Rust workspace
  crates/dvi-syntax        lexer, preprocessor, error-tolerant parser (declarations + references)
  crates/dvi-project       build files, auto-configuration
  crates/dvi-analysis      compilation, index, name resolution, IDE features
  crates/dvicode-server    LSP server (stdio/TCP), remote file system, `check` CLI
client/                    VS Code extension (TypeScript)
  src/launcher.ts          where the server runs, deployment
  src/pathmap.ts           URI mapping between VS Code and the server
  src/remotefs.ts          dvicode:// file system over the LSP connection
  test/                    end-to-end tests in a real VS Code
docs/                      architecture and build file reference
examples/demo-project      sample project used by the tests
scripts/package.sh         build the static server and the VSIX
```

## Building and testing

The server builds on Linux or WSL (Rust stable):

```bash
cd server && cargo test                         # unit + integration tests
scripts/package.sh                              # static musl server + client/dvicode-*.vsix
```

`dvicode-server check <dir> [--build FILE]` compiles a project on the command line and
prints its problems; `--goto/--hover/--refs/--complete/--sig/--impl F:LINE:COL`,
`--outline F`, `--hier -`, `--uvm TEST|-` (verification hierarchy), `--calls F:LINE:COL`
(call hierarchy), `--fixes F` (quick fixes of a file) and `--symbols Q` run IDE queries
without an editor.

End-to-end tests start the installed VS Code (isolated profile, other extensions disabled)
on the demo project and check navigation, hover, completion, problems, incremental edits,
the design and verification hierarchies, call hierarchy, quick fixes and macro expansion
through VS Code's own APIs:

```bash
cd client && npm install && node esbuild.mjs
node test/runTest.js 'D:\path\to\examples\demo-project' win-drive         # Windows → WSL, /mnt/d mapping
node test/runTest.js 'file://wsl.localhost/Ubuntu/home/me/demo' wsl-unc  # Windows → WSL folder
node test/runTest.js 'dvicode://wsl+Ubuntu/home/me/demo' remote-fs       # files only in WSL
node test/runTest.js 'dvicode://ssh+me%40host/home/me/demo' ssh          # files and server over SSH
```

## Status and limits

DVICode is an early implementation. Not there yet: VHDL, e,
PSS; elaboration (parameter values, generate conditions), factory overrides and
config-db in the verification hierarchy (component arrays show as one node), factory/config-db
views, diagrams; formatting; quick fixes beyond unknown names; linting rules.
The parser models declarations exactly and treats statements and expressions loosely, which
keeps it fast and robust while typing; checks are deliberately conservative so that valid
code stays clean.

## License

Apache License 2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE).
