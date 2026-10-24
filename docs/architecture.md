# DVICode architecture

DVICode (DV Intelligence) is a SystemVerilog/UVM IDE back end written in Rust
(`dvicode-server`) plus a VS Code extension (`dvicode`). The main use case is a VS Code
client on Windows or macOS with the server running on Linux: a remote machine over SSH, or
WSL2.

```
 VS Code (Windows / macOS)                        Linux (WSL2 or remote host)
+--------------------------------+               +--------------------------------------+
| dvicode extension (UI side)    |   LSP over    | dvicode-server                       |
|  - launcher: local | wsl | ssh | stdio of the  |  project: build file -> compile list |
|    | tcp (attach)              | launched      |  syntax: lexer, preprocessor, parser |
|  - path mapping / dvicode-fs   | process       |  analysis: index, scopes, resolve    |
|  - views: compiled files,      | <-----------> |  lsp: standard + dvicode/* requests  |
|    design hierarchy, macros    |               |  fs:  dvicode/fs/* (remote files)    |
+--------------------------------+               +--------------------------------------+
```

## Transport

The server talks LSP (JSON-RPC with `Content-Length` framing) over **stdio** by default.
The client starts it with a launch command template and uses that process's
stdio as the channel:

| mode    | command the client spawns                                   |
|---------|-------------------------------------------------------------|
| `local` | `<server> --stdio ...` (the engine bundled for this platform) |
| `wsl`   | `wsl.exe -d <distro> -e bash -lc 'exec "<server>" --stdio ...'` |
| `ssh`   | `ssh [opts] -T -o BatchMode=yes <host> "bash -lc 'exec \"<server>\" --stdio ...'"` |
| `custom`| `dvicode.server.launchCommand` with `${SERVER_CMD}` inside  |
| `tcp`   | no spawn; connect to `host:port` of `dvicode-server --listen` (token optional) |

In `tcp` mode the engine authenticates the client only when it was started with a token
(`DVICODE_TOKEN=T`, or `--token T`, which other users can see in the process list). Without
one, it only listens on a loopback address, where any program on the machine can connect and
with it read and write every file the engine's account can: use a token on shared machines.
The token and the files travel unencrypted; across a network, listen on `127.0.0.1` and
tunnel (for example `ssh -L`).

The login shell (`bash -lc`) gives the engine the user's environment (`UVM_HOME`, tool
paths). `ssh` and `wsl.exe` already carry stdio, so the language server needs no port of its
own, and there are no registry files or TLS certificates to manage. The extension can deploy
the bundled Linux x64 engine to `~/.dvicode/server/<version>-<hash>/` on the target
(`dvicode.server.autoDeploy`); other Linux architectures need an engine built there and
`dvicode.server.path`.

## Where the files live

1. **Shared/mapped files.** The workspace folder is visible to both sides (WSL `\\wsl.localhost\<distro>\...`,
   a Samba/NFS/SSHFS mount). The client rewrites every URI that crosses the wire with
   `dvicode.pathMappings` (WSL UNC paths are mapped automatically).
2. **Remote files** (`dvicode://` scheme). The workspace folder exists only on the server.
   The extension registers a `FileSystemProvider` backed by `dvicode/fs/*` requests to the
   server (stat, readDirectory, readFile, writeFile, delete, rename, createDirectory), so
   VS Code edits remote files directly; the server sees plain `file://` paths. Changes made
   on the server by other programs (a generator, another editor) are not watched: run
   **DVICode: Rebuild Project** after them, which reads every file again.

One window works with one engine and one project: the first workspace folder decides where the
engine runs and which build file it uses. Further folders are not configured on their own; their
files are analyzed with the first folder's configuration when opened, and a `dvicode://`
folder of another host cannot be opened in the same window.

## Server crates

| crate | role |
|---|---|
| `dvi-syntax` | lexer, preprocessor (macros with arguments, `include`, conditionals, origin tracking for every token), error-tolerant recursive-descent parser producing a declaration-level AST plus identifier references in bodies |
| `dvi-project` | build configuration (`.dvicode/default.build`): `-f`, `+incdir+`, `+define+`, `-y/-v/+libext+`, `-top`, env vars, `+dvicode_*` directives, see [build-files.md](build-files.md); auto-configuration when absent |
| `dvi-analysis` | compilation of the project in order, global index (packages, modules, classes, macros), scopes and resolution (lexical, class inheritance, imports, `pkg::x`, `a.b.c` member chains), and the IDE features |
| `dvicode-server` | binary: LSP loop, document store, transports (`--stdio`, `--listen`), remote file system |

## Edits of large files

An edit is compiled again with the unit it belongs to, from the file's new text. What keeps
that quick in big files:

- **Lexing:** a file over 32 KB is lexed again only from the line of the edit to the first
  unchanged token after it (`lexer::relex`). The tokens after that are the old ones, moved.
  `DVICODE_CHECK_RELEX=1` also lexes the whole file and logs any difference.
- **Answers while typing:** completion and signature help on a line inside a body are
  answered from the last analysis plus that line, without waiting for the update
  (`Server::line_local`).
- **Name resolution:** the problem check, semantic tokens and the reference searches
  resolve each `a.b.c` chain once and remember lookups for the request.

On a 5 MB generated register model, an update takes about 80 ms. About 55% of that is
preprocessing (macro expansion over the whole unit), and about 30% building the unit
(parsing 15%, indexes).

### Postponed: reusing preprocessing and parsing

Two further steps would make an update depend on the size of the edit rather than of the
unit:

1. **Reuse the preprocessed output.** When an edit's tokens contain no directive and no
   macro use, before and after, the new output is the old one with those tokens replaced,
   and the macro state after the file is unchanged.
   - It needs a map from source tokens to output tokens, kept through includes, macro
     expansions and `` `ifdef `` regions.
   - Every directive rule must be shown local to the edit.
   - The unit's inactive regions, macro uses and problems must be spliced the same way.
   - Expected: about 80 → 40 ms for that file.
2. **Reparse only the enclosing item.** Parse the module, class or member around the edit
   and splice its declarations and references into the unit. Declaration and reference ids
   are indexes into vectors with parent links and indexes on top, so this needs either
   renumbering or stable ids.

Both are postponed until real use shows the need. The files that benefit most are large
generated files, which are rarely typed in. Typing elsewhere already waits less than the
gap between keystrokes. Signs to take them up again:
- reports of lag while editing large hand-written files;
- slow machines where an update takes longer than about 100 ms;
- a profile of such a file showing preprocessing (stage 1) or `make_unit` (stage 2) as the
  cost.

Each step would come with a compare-with-full mode, like `DVICODE_CHECK_RELEX`, and a
corpus fuzz run in that mode.

## Custom requests (`dvicode/*`)

| method | purpose |
|---|---|
| `dvicode/status` | project, file and symbol counts, build-file path, timing |
| `dvicode/compiledFiles` | ordered list of compiled files with include children |
| `dvicode/designHierarchy` | module instance tree from the top (auto-detected or `-top`) |
| `dvicode/tree` | the tree of a view: `{view: "design" \| "verification" \| "files", top?, test?, filter?, target?, reveal?, maxNodes?, objects?, ports?}` → `{revision, roots, matches, incomplete}` (and `test`, `configured`, `tests` for the verification view). `filter` keeps matching nodes and their parents (rules in `ide/filter.rs`); `target` (`{definition, instance}` or `{class}`) keeps the nodes of one design element, instance or class instead; `reveal` (names from a root) returns that node even beyond `maxNodes`. `incomplete` says which limit cut the tree: `nodes`, `matches` or `visits` |
| `dvicode/locate` | the design element, instance and class around a position (`{revision, design, instance, class}`; `class.kind`: `component` or `object`), for the views that follow the editor |
| `dvicode/structure` | the Structure of a file: `{textDocument, filter?, maxNodes?}` → `{revision, version, status, context, others, incomplete, nodes}`; each node has `id`, `name`, `kind` (`group` for a group), `detail`, `extent` (the source it covers), `target` (where a click goes), `inherited_from`, `mark`, `inactive`, `collapsed`, `children` |
| `dvicode/revision` | notification to the client: the analysis changed after edits (the views refresh) |
| `dvicode/expandMacro` | macro text at a position, one level or all levels |
| `dvicode/includingFiles` | files that include the given file |
| `dvicode/rebuild` | re-read the build file and recompile |
| `dvicode/fs/*` | remote file system for the `dvicode://` scheme |
| `dvicode/internalError` | notification to the client: a feature failed (feature, message, source location, backtrace, engine version) |
| `dvicode/debug/fail` | fails on purpose, to test the above |

## Internal errors and crashes

A bug in the engine must cost one answer, not the session:

- Every request, notification, full build, incremental update and problem check runs under
  `crash::catch` (`dvicode-server/src/crash.rs`). A panic there is logged with its location
  and a backtrace (release builds keep function names for this), the request gets an error
  response, and the client is sent `dvicode/internalError` once per feature and place. A
  failed full build falls back to an empty project and the status says so (`build_error`).
- Stack overflows cannot be caught, so the parser limits statement nesting
  (`MAX_STMT_DEPTH`) and the worker threads have 16 MB stacks.
- If the process dies anyway, the client restarts it up to 4 times in 3 minutes, then stops.

The client (`client/src/report.ts`) shows a notice with **Report Issue** and **Show Log** for
each of these, and the status bar shows a failed build. **DVICode: Report Issue** opens VS
Code's issue reporter with the setup, the last internal error and the last 100 log lines
filled in; the user reviews and submits it. Nothing is sent automatically.
