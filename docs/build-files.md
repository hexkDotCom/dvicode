# Build configuration files

A build file tells DVICode which files to compile, in which order, and with which include
directories and macros. It uses the argument-file syntax that simulators already understand
(`-f`, `+incdir+`, `+define+`, ...), so the file lists of an existing simulation flow can
usually be used as they are: the options below are understood, and other options are
skipped. DVICode adds a few directives of its own, all starting with `+dvicode_`.

- [Where DVICode looks](#where-dvicode-looks)
- [Syntax](#syntax)
- [Simulator options](#simulator-options)
- [DVICode directives](#dvicode-directives)
- [Paths, wildcards and environment variables](#paths-wildcards-and-environment-variables)
- [Jinja-templated argument files](#jinja-templated-argument-files)
- [Invocations](#invocations)
- [Auto-configuration](#auto-configuration)
- [From FuseSoC and Bender](#from-fusesoc-and-bender)
- [Plusargs](#plusargs)
- [Problems in build files](#problems-in-build-files)
- [Examples](#examples)

## Where DVICode looks

1. The file set in the `dvicode.buildFile` setting (relative to the workspace folder, or
   absolute on the server). The value `auto` skips build files and always
   [auto-configures](#auto-configuration).
2. `.dvicode/default.build`
3. Nothing found: the project is [auto-configured](#auto-configuration).

Every `*.build` file in `.dvicode/` is a build configuration you can switch to
with **DVICode: Set Current Build Configuration...**, for example `rtl.build` for the design
only and `tb.build` for the full testbench. Editing and saving the active build file starts a
new full build.

## Syntax

```
// Line comment            # also a line comment            -- at the start of a line too
/* Block comment, may span lines */

+incdir+rtl/include+common/include       +directive+arg1+arg2
-f    tb/tb.f                            -option argument
rtl/top.sv                               anything else is a source file
"dir with spaces/file.sv"                quotes group text into one word
```

- Words are separated by spaces, tabs and newlines; a directive does not have to start a line.
- `'...'` and `"..."` group text into one word. Quotes around a whole path are removed;
  quotes inside a define value are kept (`+define+MSG="hello world"`).
- Unknown simulator options are ignored. Unknown `+dvicode_` directives are reported.

## Simulator options

| Option | Meaning |
|---|---|
| `-f FILE`, `-F FILE`, `-file FILE` | Read more arguments from `FILE` (nested files are allowed, up to 32 levels). See [paths](#paths-wildcards-and-environment-variables) for how relative paths inside it resolve. |
| `+incdir+DIR[+DIR...]`, `-incdir DIR` | Include directories for `` `include ``, searched in order after the directory of the including file. |
| `+define+NAME[=VALUE][+NAME2...]`, `-define NAME[=VALUE]`, `-D NAME[=VALUE]` | Macros defined before the first file. A `+` starts a new define only when an identifier follows, so `+define+EXPR=A+1` keeps `A+1`. |
| `+undefine+NAME` | Remove a macro defined on the command line. Undefines apply after all the defines of the invocation, wherever they appear. |
| `-y DIR` | Library directory: a module that is used but not defined is looked up as `DIR/<module><ext>`. |
| `-v FILE` | Library file: compiled to provide modules that are used but not defined. |
| `+libext+.v+.sv`, `-libext .v,.sv` | Extensions for `-y` lookups (default `.v`, `.sv`). |
| `-top NAME`, `+xmtop+NAME`, `+nctop+NAME` | Design top for the Design Hierarchy view (otherwise every module that is never instantiated is a top). |
| `-uvm`, `-ovm` | Compile the UVM library: adds `+incdir+$UVM/src` and `$UVM/src/uvm_pkg.sv` where the first of these options appears. `$UVM` is the `-uvmhome` argument, else `$UVM_HOME`, else `$DVICODE_UVM_HOME` (set by the `dvicode.uvmHome` setting). |
| `-uvmhome DIR` | UVM location, used for the whole [invocation](#invocations) wherever it appears in it. Also compiles the library, like `-uvm`; using both compiles it once. |
| `-setenv NAME=VALUE` | Same as `+dvicode_setenv+NAME=VALUE`. |
| `-makelib LIB ... -endlib` | The files between are compiled; the library name is ignored. |

Options that take a value DVICode does not need (`-l log`, `-work lib`, `-timescale 1ns/1ps`,
`-L lib`, `-sv_lib`, `-cm`, `-access`, `-suppress`, ...) are skipped together with their
value. Other `+options+` and `-options` are skipped.

## DVICode directives

| Directive | Meaning |
|---|---|
| `+dvicode_init[+MODE]` | Start a new [invocation](#invocations): forget the macros, include directories and libraries of the previous one. `MODE` names the compiler being emulated and only changes how relative paths in `-f` files resolve. |
| `+dvicode_init_auto[+MODE]` | Start an invocation that is [auto-configured](#auto-configuration). Files (and wildcards) listed after it are added to what auto-configuration finds. |
| `+dvicode_init_from_simlog[+MODE]+LOG` | Start an invocation with what a VCS or vlogan compile log shows (one invocation per compile command in the log, each with its own defines; lines after this one add to the last): the files of its `Parsing design file` lines in order, its `Parsing library file` lines as library files, the `+define+` and `+incdir+` of its command line (`Command: vcs ...`), and the folders of its `Parsing included file` lines as include directories. A Questa log or transcript (`vlog` output, or a `vsim` transcript whose lines start with `# `) does not list the files it parses: its `vlog ...` command lines are read like an argument file instead (files, `-f` files, `+incdir+`, `+define+`, `-y`/`-v`). Relative paths are from the log's folder. Use one line per log (one per `vlogan` run, for example). Hover over the log path to see what was read. Xcelium logs are not read yet. |
| `+dvicode_setenv+NAME=VALUE` | Set an environment variable for the rest of the build file (and for nested `-f` files). `VALUE` may use other variables. |
| `+dvicode_setenv_no_override+NAME=VALUE` | Same, unless `NAME` is already set (in the environment, in `dvicode.env`, or earlier in the file). |
| `+dvicode_undefine+NAME` | Same as `+undefine+NAME`. |
| `+dvicode_skip_compile+PATTERN[+PATTERN...]` | Do not compile top files whose path matches a [wildcard](#paths-wildcards-and-environment-variables) pattern. A pattern without a leading `/` matches anywhere in the path: `*/obsolete/*`, `**/*_old.sv`. A skipped file is still analyzed on its own while it is open in the editor. |
| `+dvicode_compilation_root+DIR` | Directory that relative paths resolve against for the rest of this file and in `-f` files read from here on in the `questa.*`, `ius.*` and `xcelium.*` modes (default: the project folder). Lasts until the next `+dvicode_init`. |
| `+dvicode_uvm_home+DIR` | Same as `-uvmhome DIR`. `+dvicode_uvmhome_override+DIR` is the same. |
| `+dvicode_include_config+FILE` | Read another build file here (its relative paths resolve against its own directory). |
| `+dvicode_test+FILE` | Compile `FILE` like a top file (kept for compatibility with test lists). |
| `+dvicode_switches+NAME[+NAME...]` | The switches that [Jinja conditions](#jinja-templated-argument-files) in argument files read after it test (`{% if "NAME" in fl_switches %}`). Each use replaces the list. |
| `+dvicode_path_map+FROM=TO` | From this line on, read paths in the folder `FROM` from the folder `TO` instead: for build files, argument files and compile logs written on another machine (a compute farm, a CI runner). See [Paths written on another machine](#paths-written-on-another-machine). |

## Paths, wildcards and environment variables

**Relative paths** in the build file resolve against the project folder. Inside an `-f`
file, they resolve against the directory of that `-f` file when the mode is `default`, a
`vcs.*` mode, or when the file was read with `-F`. In the other modes (`questa.*`, `ius.*`,
`xcelium.*`) they resolve against the compilation root, like those simulators do: the project
folder, unless `+dvicode_compilation_root+` changed it. That directive also changes the base
for the rest of the file it is in.

**Wildcards** work in source files and include directories:

| Pattern | Matches |
|---|---|
| `?` | one character in a path component |
| `*` | any characters within one path component |
| `**` | any number of directories |

`rtl/**/*.sv` compiles every `.sv` file under `rtl/`, in name order.

**Environment variables** are written `$NAME`, `${NAME}`, `$(NAME)` or `%NAME%`. They come
from, in order of priority: `+dvicode_setenv+` earlier in the build, the `dvicode.env`
setting, then the environment of the server process. In the WSL and SSH modes the server is
started through a login shell, so variables from `~/.profile`/`~/.bash_profile` are
available. These are always set:

| Variable | Value |
|---|---|
| `DVICODE_PROJECT_LOC` | the project folder (as the server sees it) |
| `DVICODE_PROJECT_PARENT_LOC` | its parent folder |

An undefined `$NAME`, `${NAME}` or `$(NAME)` in a path is reported and the entry is skipped; an
undefined `%NAME%` is kept as written. In a define value an undefined variable is kept as
written.

### Paths written on another machine

Build files, `-f` files and compile logs made on a compute farm often hold absolute paths that do
not exist where DVICode runs. `+dvicode_path_map+FROM=TO` reads every path in the folder `FROM`
from the folder `TO`:

```
+dvicode_path_map+/nfs/proj=/home/me/proj
-f /nfs/proj/sim/files.f
+dvicode_init_from_simlog+/nfs/proj/sim/compile.log
```

- `TO` is a folder on the machine where the DVICode engine runs: a Linux path when it runs in WSL
  or over SSH. Variables are expanded in both; a relative `TO` is from the base of the file the
  line is in.
- It applies to every path read after the line: source files, `+incdir+`, `-y`/`-v`, `-f`/`-F`
  and the files in them, the UVM location, and the files, include directories and libraries of
  `+dvicode_init_from_simlog`; after `$VAR` expansion.
- Whole folders only: `/nfs/proj` maps `/nfs/proj/a.sv`, not `/nfs/proj_old/a.sv`. When several
  maps match, the longest `FROM` wins. On Windows the comparison ignores case.
- Hover over a mapped path to see how it is read.

This is different from the `dvicode.pathMappings` setting, which maps the folders the editor
shows to the ones the engine sees (a mapped drive and its Linux path); `+dvicode_path_map+` maps
paths written in the build inputs.

## Jinja-templated argument files

Some run flows render their `-f` files with Jinja before calling the simulator, choosing
files with switches of the model being run:

```
common/top.sv
{% if "gate_sim" in fl_switches %}
  netlist/top_gate.v
{% elif ("fpga" in fl_switches) or ("emu" in fl_switches) %}
  rtl/top_fpga.sv
{% else %}
  rtl/top.sv
{% endif %}
```

DVICode evaluates these conditions itself, with the switches given in the build file before
the `-f` file is read:

```
+dvicode_switches+sim+gate_sim
-f verif/run/default.f
```

- Supported: `{% if %}`, `{% elif %}`, `{% else %}`, `{% endif %}` (nested too), with
  `"NAME" in LIST`, `"NAME" not in LIST`, `not`, `and`, `or`, parentheses, `true` and `false`.
  A plain variable name (`fl_switches`, or any other) stands for the switch list. `{# comments #}`
  are skipped. Without `+dvicode_switches+`, the list is empty.
- Not supported, and reported: `{{ expressions }}` (left out of the text, so a path built
  with one is usually not found), other tags such as `{% for %}` or `{% set %}` (ignored), and
  conditions that test something other than the switch list, such as
  `"x" in fl_switches[0]['config']` (taken as false).
- Hovering a condition in the editor shows whether it is true.

## Invocations

A build file can describe several compiler invocations, each starting with
`+dvicode_init`. Macros do not carry over from one invocation to the next, the same as
separate `vlog`/`vcs` runs. Within an invocation, files are compiled in the order listed and
macros defined in one file are visible in the files after it (single compilation unit).

Without any `+dvicode_init`, the whole file is one invocation.

`MODE` values: `default`, `vcs.vlogan`, `vcs.vhdlan`, `questa.vlog`, `questa.vcom`,
`questa.qrun`, `ius.irun`, `xcelium.xrun`, `ius.perspec`, `gcc`. They only affect how `-f`
paths resolve (see above).

## Auto-configuration

With no build file, or in a `+dvicode_init_auto` invocation, DVICode scans the project
folder and:

- compiles every `.sv`, `.v`, `.svp`, `.vp` and `.sva` file that no other file includes
  (`.svh`, `.vh` and `.svi` files are only compiled through `` `include ``);
- puts files that only define macros first, then orders the rest so that a package is
  compiled before the files that use it;
- adds every directory that contains sources as an include directory;
- compiles UVM first when the sources import `uvm_pkg` or include `uvm_macros.svh` and the
  project does not contain UVM itself (needs `$UVM_HOME` or `dvicode.uvmHome`).

Hidden directories, common build output directories (`work`, `INCA_libs`, `xcelium.d`,
`csrc`, `simv.daidir`, `obj_dir`, `node_modules`, `target`, ...) and whatever the project's
top-level `.gitignore` ignores are skipped. Its patterns may use `*`, `?`, `**`, character
classes (`[abc]`, `[a-z]`, `[!abc]`) and `!` lines that re-include what an earlier line
ignored (the last matching line wins, and, as in Git, nothing is re-included from a folder
that is itself ignored). UVM from `$UVM_HOME` (or
`dvicode.uvmHome` when `$UVM_HOME` is not set) is still used when it lives in an ignored folder.
Symbolic links to directories are not followed (list such sources in a build file), and the scan
stops after 50,000 files.

A folder often holds several packages or modules of the same name: one copy per top or per
generated configuration, a vendored copy next to the original, an old copy kept in a
`deprecated` folder. A simulator compiles only one of them; auto-configuration compiles them all,
so a name used in the code resolves to the nearest copy in the folder tree that fits the use:
the package that declares the member used (`pkg::name`, `import pkg::name`), the module that has
the ports and parameters an instance connects by name. Go to definition follows the same choice.
With a build file the first copy compiled is used, as in a simulator.

Auto-configuration is a good start; a build file is better for projects with several copies of
the same files or files that must not be compiled together.

Auto-configuration cannot know the defines your simulator is given (`+define+UVM`,
`-define FULL_ENV`). When code removed by `` `ifdef `` in a package or class may declare names
used elsewhere, those uses are not reported as errors. Instead, one notice (SKIPPED_CODE) on the
`` `ifdef NAME `` line says how many uses depend on it, for example:

```
`FULL_ENV` is not defined, so this code is skipped. 12 uses in the project may need names
declared here and are not checked. ... add +define+FULL_ENV to a build file.
```

The count follows your edits. Adding the define to a build file compiles the code and
checks those uses again.

The same goes for a macro that no file in the folder defines: it usually comes from the
simulator's command line (`+define+RNG_BUS_WIDTH=4`) or from a library outside the folder.
Instead of an error on each use, each file that uses it gets one notice (UNDEFINED_MACRO, as
information) on the first use, saying how many uses there are not expanded. A macro that is
defined in the folder, but only after its use in the compile order, is still an error. With a
build file every use of an undefined macro is an error.

### Seeing what was read

With `dvicode.server.logLevel` set to `debug`, the DVICode output channel shows each build
and argument file as it is read (with the folder its relative paths start from), the argument
files, wildcards and how many files they matched, defines, `+dvicode_setenv+` variables, the UVM
library, a summary of each invocation, and every build-file problem. Without a build file it
shows the folders auto-configuration skipped and why (hidden, build output, `.gitignore`,
symbolic link), the `.gitignore` patterns it applies, how many sources it found, headers that no
file includes, included files that are not in the folder, packages defined more than once, and
whether UVM was added. `trace` adds every file and the compile order. From a terminal:
`dvicode-server check <folder> [--build FILE] --log-level debug`.

## From FuseSoC and Bender

Projects described as [FuseSoC](https://github.com/olofk/fusesoc) cores or
[Bender](https://github.com/pulp-platform/bender) manifests (OpenTitan, PULP) can give DVICode
the file list their own tool produces; reference it from `.dvicode/default.build` with `-F`
(relative paths from the list's own folder). Regenerate the list when the core or manifest
changes.

**FuseSoC.** `--setup` writes the simulator's files without running anything, including what
the core's generators produce (register packages, for example). From the project folder:

```bash
fusesoc --cores-root hw run --no-export --target=sim --setup lowrisc:dv:uart_sim:0.1
```

OpenTitan also needs `--mapping=lowrisc:prim_generic:all:0.1`, as its `dvsim` flow passes it.
The list is `build/<core>/sim-<tool>/<core>.scr` (or `.f`, depending on the tool), with
`+incdir+`, `+define+` and the files in compile order. The build file adds what the flow gives
the simulator on its command line:

```
// .dvicode/default.build
-uvm
+define+UVM
+define+SIMULATION
-F build/lowrisc_dv_uart_sim_0.1/sim-vcs/lowrisc_dv_uart_sim_0.1.scr
-top tb
```

**Bender.** `bender script flist` prints the files of the manifest and its dependencies;
`bender script flist-plus` adds the `+incdir+` and `+define+` lines. Pick the targets the
simulation uses with `-t`:

```bash
bender script flist-plus -t simulation -t test > bender.f
```

```
// .dvicode/default.build
-F bender.f
```

DVICode's own test corpus builds OpenTitan and PULP projects this way
(`scripts/corpus/fusesoc_build.py`, `scripts/corpus/bender_flist.py`).

## Plusargs

Other `+NAME=VALUE` arguments are kept as simulation plusargs. DVICode uses:

| Plusarg | Use |
|---|---|
| `+UVM_TESTNAME=TEST` | Top of the UVM Verification Hierarchy view. |

## Problems in build files

A build file is checked as you type, before it is saved. Warnings are underlined at the word
that is wrong and listed in the Problems view. The build skips the wrong entry and continues
with the rest. The checks:

- Files, directories, `-y`/`-v` libraries and `-f` files that do not exist, and wildcards
  that match nothing.
- Environment variables that are not set; the warning names the variable.
- Options with no value (`-incdir` at the end of a line, `+incdir+` with nothing after it,
  `+dvicode_setenv+NAME` with no `=`).
- Misspelt options and directives, with what was probably meant: `-incdr` (did you mean
  `-incdir`?), `+incdri+`, `+dvicode_skip_compil`, `+dvicod_init`.
- `-f` files that include themselves, unreadable files, and `-uvm` with no UVM library.

Other simulator options (`-sv`, `-timescale`, `+acc`, ...) are accepted and ignored.

Hover over a word to see what DVICode makes of it: the full path and whether it exists, what
a variable expands to, how many files a wildcard matches (and the first few), and which
options DVICode does not use. Ctrl+click (Cmd+click on macOS) a file name to open the file.

An open `-f` file is checked the way the current build reads it: relative paths start from
its folder, and it sees the variables set before it. An `-f` file that the build does not read
gets hover and links, but no warnings, because its paths may be meant for another folder.

## Examples

A testbench with UVM, a design file list and a separate test package:

```
// .dvicode/default.build
-uvm
+dvicode_setenv+IP=$DVICODE_PROJECT_LOC/ip
+incdir+$IP/rtl/include
+define+SIMULATION
-f $IP/rtl/rtl.f
+incdir+tb
tb/tb_pkg.sv
tb/tb_top.sv
-top tb_top
+UVM_TESTNAME=base_test
```

Two invocations: a Verilog library compiled on its own, then the testbench; generated
files are skipped:

```
+dvicode_init+questa.vlog
-f lib/lib.f

+dvicode_init+questa.vlog
-uvm
+dvicode_skip_compile+*/generated/*
tb/**/*.sv
```

The simulator's own file list, plus what it gets from the Makefile:

```
+define+WIDTH=32
-f sim/compile.f
```

The demo project in [examples/demo-project](../examples/demo-project) has
`.dvicode/default.build` and `.dvicode/errors.build` (the same plus a file with deliberate
mistakes).
