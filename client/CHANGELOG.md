# Changelog

## 0.4.0 (preview)

**Typing in large projects**

- Completion and signature help no longer wait for a large file to be compiled again when
  you type on one line inside a function, task or block: they are answered at once from the
  last analysis with the line as it is now. Typing in a 5 MB register model went from about
  100 ms to under 10 ms per keystroke. Names declared on that same line show up a keystroke
  later.
- Projects without a build file no longer check every file again after each edit to count
  the uses that `ifdef`-removed code hides: only the files an edit can affect are checked
  (typing in opentitan: 72 → 10 ms per keystroke).
- `-y`/`-v` library files are looked for again only when an edit changes which design
  elements are defined or instantiated, or the macros they see; editing a library file no
  longer searches the library folders on every keystroke.
- Faster updates of big files (lexing, parsing, indexing), member lookups that do not
  allocate, and semantic tokens of a 5 MB file in 150 ms instead of 400 ms.

**Fixes**

- Edits that change what other files see through an instance (the ports a statement
  connects, when a design element is defined twice without a build file), a `bind` target,
  a named block's variables, or which scope `ifdef`-removed code is in, now update the
  problems of the other files too.
- A request about a file waits for edits of files compiled before it in the same build,
  whose macros it is compiled with.

**Release packages**

- The packages of a release carry build provenance attestations, which say that the release
  workflow built them from that release's tag. Check one with
  `gh attestation verify dvicode-0.4.0.vsix -R hexkDotCom/dvicode --signer-workflow
  hexkDotCom/dvicode/.github/workflows/release.yml --source-ref refs/tags/v0.4.0`. Open VSX
  receives the very packages of the GitHub release, checked against those attestations.
- The README explains the remote TCP engine's token, tunnelling it over SSH, and what
  Restricted Mode allows for `dvicode://` folders.

## 0.3.1 (preview)

**Responsiveness**

- Project updates run off the message loop: hover, completion and navigation answer while a
  large project is being rebuilt, and wait only for the edits they need. Cancelled requests
  (`$/cancelRequest`) stop early, and problem checks skip files a newer check owns.
- Editing a header included by many files preprocesses each includer once, in order: a header
  with 20,000 includers updates in about a second.
- The Linux x86_64 engine uses mimalloc and the Windows engine the segment heap: full builds
  are up to 3x faster on Linux, with memory returned at once.
- Faster cursor-scope lookups, renames across many files, library discovery, macro
  highlights, and completion in large designs.

**Also**

- The files a build reads (build file, argument files, simulator logs) are watched whatever
  their names; a new build file is announced when its build is in use.
- Opening a header that an `` `include `` did not find compiles the files that include it
  again; completion in a file included inside a module or class sees that scope.
- Where case does not matter (Windows, macOS), one file is one file under every spelling of
  its name.

**Fixes**

- Deeply nested constraints and structs, and include cycles, no longer crash completion or
  the parser; saving then closing a file no longer leaves its old text in use.
- Bracketed characters in build paths (`[abc]`) are read as a class only when they match.
- The client: Restart works while a start hangs; `\\wsl.localhost` paths, remote TCP engines
  with IPv6 addresses, custom launch commands with quotes, any typed design top, and `case` /
  `endcase` indentation.

**Security**

- An engine started with `--listen` and no token accepts only loopback addresses; token
  handshakes no longer hold up other clients.
- In Restricted Mode, a `dvicode://` folder may only name the SSH host or TCP address your own
  settings name.
- Build files cannot make variables or `**` patterns grow without bound; `.gitignore` is read
  only as a regular file of sensible size; variable values are no longer logged.
- Rename does not change a file linked into the workspace from outside it.

## 0.3.0 (preview)

**UVM Verification Hierarchy: as UVM builds it**

- The hierarchy follows what runs: each constructor (with `super.new()`), then the most derived
  `build_phase` with `super.build_phase()` where it is called (and `build()` through the
  library's), helper methods at every call with the literal names they are given; children are
  built in name order, as UVM's build phase does. A derived `build_phase` that skips
  `super.build_phase()` no longer shows its base class's components.
- Factory overrides are applied: type and instance overrides set on a component, on the factory
  or through `T::type_id`, by type or by registered name, in the order they are registered, with
  `replace`; an instance override wins, a loop stops where it closes. The node shows the class
  built and its tooltip the overrides; one set under a condition is shown as "may be
  overridden" instead of applied.
- Components made under an `if` or `case` are marked "conditional" with the condition; the two
  branches of an `if (host) drv = A...; else drv = B...;` are one node "A · or B". Components
  made in a loop show the number of iterations (`×4`, `×≤4` when it can stop early, `×?`).
- **Show UVM Objects** (in the view's ... menu): configuration objects (with their
  `uvm_config_db` sets), the register model (the block itself), adapters, and the sequences
  each component starts in its run-time phases, with the sequencer each starts on (a tree path
  when the handles lead to a component), the sequences they start in turn (`uvm_do` and its
  variants, `start`), and default sequences under their sequencer.
- **Show TLM Ports** (same menu): ports, exports and imps are hidden by default; a component's
  tooltip counts them.
- Following the editor finds a component from the class it was built from or the one it was
  requested as, and says when the class is an object to show with the switch.
- The hierarchy is faster on large testbenches: files are read once per request, and class
  bases are resolved once per lookup (a large processor testbench went from minutes to
  milliseconds).

**Structure view**

- A new view, Structure: the active file's includes, defines, modules, packages and classes,
  grouped (parameters, ports, signals, instances, processes, generate blocks, functions,
  assertions; fields, constraints, methods with the UVM phases marked), with what each base
  class gives a class and which methods override or hide. Code `` `ifdef `` leaves out is listed
  by lines. It filters by name and follows the cursor.

**Also**

- `+dvicode_init_from_simlog` reads Questa compile logs and transcripts too: the `vlog`
  command lines, with their files, `-f` files, include directories and defines (Questa does not
  list the files it parses). Part of #9.
- When no UVM test is found, the Verification Hierarchy says why if it can: classes extend
  `uvm_test` (or other UVM base classes) but `uvm_pkg` is not imported where they are declared, or
  the UVM library is not compiled. Without a build file it points to the skipped-code notices,
  which name the define to add.
- Snippets for the UVM report macros (`uvm_info` with a choice of verbosity, `uvm_warning`,
  `uvm_error`, `uvm_fatal`), `uvm_config_db` set and get (with the usual fatal when the get
  fails), and analysis ports and imps (#11).
- A build file with `-top+NAME` is told to write `-top NAME` (or `+xmtop+NAME`); NAME is still
  the design top (#10).
- **Show Type Hierarchy** works on classes (the engine had it; editors were not told).
- Pressing Enter after a class, module or method header indents the next line; `// #region`
  folds (the language configuration's patterns were not read as written).

**Fixes**

- Renaming a port, or the signal a `.*` connection takes, is refused with the instance that
  connects it (the `.*` would have been left behind). Renaming a function argument renames
  its named bindings (`f(.value(1))`); renaming a macro renames its use in an `` `include ``.
- A named connection to something that is not a port (`.internal_signal(1)`), or an
  override of something that is not a parameter, is reported.
- `for (int i = 0, j = 0; ...)` declares `j`; `nettype T name with f;` declares `name`.
- `-y` libraries are followed through hierarchies deeper than eight levels.
- Auto-configuration no longer leaves out a file because another file of the same name is
  included by its path.
- Build files: `+define+ONES='1+ZERO='0` gives two defines; Questa command lines keep the
  order of their defines and a quoted `+incdir+` folder with spaces.
- Crashes and hangs on unusual input: `typedef T::X T;`, a long declaration header with a
  non-ASCII character at the cut, deeply nested `` `ifdef `` expressions or generate blocks,
  macros or includes that double at every level (stopped with an error after 5 million
  tokens), an argument file that is a FIFO or a device, symbolic links between sibling
  folders under `**`.
- Faster on large generated files: `` `__LINE__ `` (UVM report macros), non-ANSI port lists,
  packages of many inline typedefs, long lines after a non-ASCII character, renaming a port
  of a module instantiated thousands of times.
- The hierarchy marks a component that builds its own class as cut instead of stopping
  there silently, and stays bounded on generated constants, helpers and overrides.
- Settings: `dvicode.server.logLevel` is checked (it reaches the launch command) and ignored
  in untrusted workspaces; a setting of the wrong type no longer resets the others. In a
  `dvicode://` workspace with path mappings, results open the workspace's own documents.
- `dvicode-server check` exits 2 when its build file cannot be read.

## 0.2.0 (preview)

**Filter and follow the editor in the DVICode views** (#4, #5)

- Each of the three views (Design Hierarchy, Verification Hierarchy, Compiled Files) has a
  **Filter...** button: it shows only the nodes whose name, module or class, or path matches,
  with their parents. Plain text matches anywhere, ignoring case; `*` matches within one level
  of a path, `**` across levels. The engine searches the whole design, not only the part shown;
  on very large designs the view says when it stopped early.
- **Follow the Editor**, per view: the view selects the instance, component or file the cursor
  is in when you move, switch files, **F12** or **Ctrl+click**, without taking the focus. "1 of
  N" in the title when a module or class is used in several places, with **Next** and
  **Previous**. A node beyond the size limit of the tree is fetched when needed.
- Views refresh after edits (when typing pauses), not only after a full build, and remember
  their filter and following per build configuration.

**Other editors**

- The end-to-end tests pass in Cursor 3.26 and VSCodium 1.140, with the engine on the same
  machine, in WSL and over SSH.

- UVM Verification Hierarchy: a base test that creates `T_ENV env` shows the environment class a
  derived test gives `T_ENV` in `extends base_test#(...)` (by name or position, through a
  typedef, or the default), with its whole tree, instead of `T_ENV [unresolved]`.
- UVM Verification Hierarchy: components made by a factory object are shown
  (`env = factory.get_env(._parent(this), ...)`), when the factory method creates one
  component with the parent it is given; the name is the one given, else the variable's (#7).
- `+dvicode_path_map+FROM=TO` in a build file reads paths written on another machine (a compute
  farm, a CI runner) from a folder on this one: in the build file, its `-f` files and compile
  logs.
- Compile logs with several compile commands (`vlogan` for each library, then `vcs`) give one
  invocation per command with its own defines, instead of one merged compile (#9).
- Compile logs: quoted values (`+incdir+"a b"`), command lines continued with `\`, and the
  include directory of `` `include "dir/file.svh" `` are read correctly.
- Fixed: a crash (stack overflow) on two packages that `export` each other; a macro used inside
  its own `` `" `` string took exponential time; `` `"dir/`ID(x)/f.svh`" `` lost the macro's
  arguments; in auto mode, `M a(...), b(...);` could pick a copy of `M` that only fits `a`;
  `foreach (a[i].b[j].c[k])` stopped member checks after `a[i]`; `.gitignore` lines ending in
  `/**` hid exceptions inside the folder.
- Verification Hierarchy: helper methods in `` `ifdef `` code that is off are not followed; a
  non-virtual helper is the one its own class sees; creations are shown in the order they run.

## 0.1.5 (preview)

- `+dvicode_init_from_simlog+MODE+LOG` in a build file compiles what a VCS or vlogan compile log
  shows: its parsed design and library files in order, the defines and include directories of
  its command line, and the folders of the files it included. Before, it fell back to
  auto-configuration.
- The UVM Verification Hierarchy follows the helper methods `build_phase` calls
  (`build_envs()`, `this.build_agents()`), so tests of large testbenches that build their
  environment in helpers expand instead of showing no children.
- Snippets for UVM sequence items, sequences, drivers, monitors, agents, environments, tests and
  scoreboards (type `uvm_` in a SystemVerilog file).
- Auto-configuration applies `!` lines and `[...]` character classes of the project's
  `.gitignore`, as Git does.
- docs/build-files.md: how to use the file lists FuseSoC and Bender produce.

## 0.1.4 (preview)

**Opening a project without a build file**

- When a folder holds several copies of a package or module (one per top, generated variants,
  a vendored or deprecated copy), a name now means the nearest copy in the folder that fits the
  use: the package that declares the member used, the module that has the ports and parameters
  an instance connects. Go to definition follows the same choice. An OpenTitan checkout showed
  2,873 problems before, 585 now; with a build file nothing changes.
- A macro that no file in the folder defines (from the simulator's command line, or a library
  outside the folder) gets one notice in each file that uses it, instead of an error on every
  use.

**Finding out why a project is read wrong**

- With `dvicode.server.logLevel` set to `debug`, the DVICode output shows each build and
  argument file as it is read, what wildcards matched, defines, variables and the UVM library;
  without a build file, the folders skipped and why, headers no file includes, included files
  missing from the folder and packages defined more than once. `trace` lists every file in
  compile order. `dvicode-server check <folder> --log-level debug` prints the same.

**Fewer false errors** (found on encrypted vendor verification IP)

- Macros inside `` `" `` strings are expanded, so include paths built as
  `` `"dir/`VENDOR/name`" `` resolve.
- Modports take subroutine prototypes with arguments (`modport m(export task t(int x))`).
- A module header cut by an encrypted region ends there, and the ports and parameters of its
  instances are not reported.
- `foreach` over an array reached through several selects declares its loop variable.
- `{name: value}` without the apostrophe is an assignment pattern, as in VCS.

**Install**

- DVICode is on the VS Code Marketplace and Open VSX: search for **DVICode** in the Extensions
  view.

## 0.1.3 (preview)

Checked on 68 configurations of large closed-source UVM testbenches, besides the public corpus,
and reviewed in three rounds.

**Build files**

- Build files (`.dvicode/*.build`) and `-f` files are checked as you type, before saving.
  Warnings are underlined at the wrong word. They cover missing files and folders, wildcards
  that match nothing, variables that are not set (by name), options with no value, and misspelt
  options and directives (with a "did you mean" hint). See "Problems in build files" in
  docs/build-files.md.
- Hover over a word in a build file to see its full path and whether it exists, what a
  variable expands to, or what a wildcard matches. Ctrl+click a file name to open it.
- The build notification shows how far the build is (files scanned or compiled, percentage), and
  the DVICode log shows progress and timings. A new build request cancels the running one, and
  DVICode says when it starts using another build file.
- Filelists written as Jinja templates (`{% if "x" in fl_switches %} ... {% endif %}`) are read,
  with the switches given by `+dvicode_switches+a+b` in the build file.

**Fewer false errors** (found on large closed-source testbenches)

- Undeclared names connected to primitive and UDP instances are implicit nets (standard-cell
  libraries); unnamed DPI import arguments; `` `M(x) `` where `M` is a macro argument; macros whose
  body defines another macro; `foreach` in `randomize() with` through a macro; `solve ... hard`.
- Names in modules with encrypted (`pragma protect`) parts are not reported as undeclared.
- Without a build file, uses of names that `` `ifdef `` code may declare (a package body inside
  `` `ifdef UVM ``) are no longer errors; one notice on the `` `ifdef `` line names the macro, how
  many uses depend on it, and the `+define+` to add to a build file.
- `dvicode-server check` on a folder that does not exist is an error.

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
