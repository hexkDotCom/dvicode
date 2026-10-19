# Changelog

## 0.1.2 (preview)

Tested in VS Code's Remote - WSL and Remote - SSH windows and on macOS, and with a new edit fuzzer
that compares every incremental update with a full build.

**Remote windows**

- Remote - WSL and Remote - SSH windows: the server settings (`dvicode.server.*`,
  `dvicode.pathMappings`) of the local user settings no longer apply there; for example,
  `dvicode.server.mode: wsl` on Windows stopped the engine from starting in a Remote - WSL
  window. These settings are now per machine and no longer synchronized by Settings Sync;
  workspace settings still override them.

**Rename and quick fixes**

- Renaming a port or signal connected with an implicit `.name` makes the connection explicit
  instead of breaking the instance.
- Renaming a macro also changes its uses inside other macros, in macro-call arguments and in
  `` `ifdef `` / `` `ifndef `` / `` `elsif `` / `` `undef `` (include guards too), and can start
  from the `` `define `` line. Go to definition, hover and references work on names written in
  macro arguments.
- Rename refuses changes that would break code (a declaration used inside a macro body, a macro
  called through a pasted name, elements of files outside the workspace such as the UVM library)
  instead of making them.
- "Did you mean" for types in parameter values and macro arguments and for misspelled packages
  before `::`; "Import package" for anything the package declares.

**While editing**

- Problems no longer go stale after editing a header, after opening and closing files outside
  the build, or after moving a `` `define ``; duplicate definitions resolve the same way as after
  a full build; `-y`/`-v` library modules follow the build's own files.

**Remote files (`dvicode://`)**

- Renaming onto an existing folder replaces it, deleting a link to a folder removes the link, and
  writing to a folder reports the right error on Windows.

## 0.1.1 (preview)

Tested on large open-source projects (OpenTitan, Ibex, CV32E40P and core-v-verif, CVA6, PULP
axi and common_cells) and on SVUnit unit tests; most changes remove false problems found there.

**Fewer false problems**

- SVUnit tests: token pasting before punctuation and escaped quotes inside macro strings.
- Typedefs with packed dimensions in port, parameter and argument lists
  (`input addr_t [N-1:0] a`).
- Based numbers with macro digits (``32'h`ADDR``), `3'b0://comment`, `` `define `` argument
  lists over several lines, macro pasting with comments and spaces.
- `coverpoint {a, b};`, `foreach ($root.top.a[i])`, named assertions used hierarchically, `bind`
  (connections, and members of the bound target).
- `import pkg::*;` outside modules reaches the later files of the same compilation.
- C, C++ and VHDL files in simulator file lists are skipped.
- Build files: `+define+MASK=8'hFF` no longer swallows the rest of the file; string continuations
  in files with Windows line endings.

**Projects**

- Auto-configuration skips what the top-level `.gitignore` ignores and finds the base folder of
  `` `include "dir/file.svh" ``.
- Missing `-y`/`-v` libraries, unreadable build files and `-f` files that include themselves are
  reported.
- Much faster on folders that WSL reads from a Windows drive (`/mnt/c`, `/mnt/d`).
- New files are picked up without a rebuild; **Rebuild Project** also re-reads files changed
  outside VS Code.

**Robustness and security**

- Fixed engine crashes on a class that extends its own nested type and on very deeply nested
  code.
- In Restricted Mode (untrusted workspaces) the workspace cannot choose which program runs.
- Remote folders: files of another host are never written through the current connection; the
  engine deployed on SSH hosts is private to your account.
- TCP mode: stricter token handshake; `dvicode.env` values are kept out of the log.
- Path mapping fixes for `\\wsl$`, root mappings, and SSH projects with files only on the
  server.
- Quick fixes keep the file's line endings.

## 0.1.0 (preview)

First public preview.

**Language features**

- Problems as you type: undeclared names, unknown types, members and ports, missing
  includes and macros, syntax errors, with quick fixes ("did you mean", "import package",
  "declare variable").
- Go to definition, references, implementations and overrides, through macros, packages,
  class inheritance and port connections.
- Completion of members, package items, macros and module instantiations; signature help.
- Hover with signatures, doc comments and macro expansions.
- Type and call hierarchies, outline, workspace symbols, rename and semantic highlighting
  (inactive `` `ifdef `` code is dimmed).

**Views and commands**

- Design Hierarchy, UVM Verification Hierarchy (from `+UVM_TESTNAME`) and Compiled Files.
- Expand Macro One Level / All Levels, Show Including File.
- Build configurations in `.dvicode/*.build` (simulator argument-file syntax, `-f` files,
  `+dvicode_` directives), or automatic project configuration.
- Report Issue: opens VS Code's issue reporter with your setup and the last error for you to
  review.

**Where it runs**

- Natively on Windows, Linux and macOS; in WSL; on a Linux host over SSH, including
  folders that exist only on the server; through a custom launch command (`bsub`, `qrsh`)
  or a TCP connection.
- The engine is installed for you on WSL and SSH hosts (Linux x64).
