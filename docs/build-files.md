# Build configuration files

A build file tells DVICode which files to compile, in which order, and with which include
directories and macros. It uses the argument-file syntax that simulators already understand
(`-f`, `+incdir+`, `+define+`, ...), so the file lists of an existing simulation flow can be
used as they are. DVICode adds a few directives of its own, all starting with `+dvicode_`.

- [Where DVICode looks](#where-dvicode-looks)
- [Syntax](#syntax)
- [Simulator options](#simulator-options)
- [DVICode directives](#dvicode-directives)
- [Paths, wildcards and environment variables](#paths-wildcards-and-environment-variables)
- [Invocations](#invocations)
- [Auto-configuration](#auto-configuration)
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
| `+undefine+NAME` | Remove a macro defined earlier on the command line. |
| `-y DIR` | Library directory: a module that is used but not defined is looked up as `DIR/<module><ext>`. |
| `-v FILE` | Library file: compiled to provide modules that are used but not defined. |
| `+libext+.v+.sv`, `-libext .v,.sv` | Extensions for `-y` lookups (default `.v`, `.sv`). |
| `-top NAME`, `+xmtop+NAME`, `+nctop+NAME` | Design top for the Design Hierarchy view (otherwise every module that is never instantiated is a top). |
| `-uvm`, `-ovm` | Compile the UVM library: adds `+incdir+$UVM/src` and `$UVM/src/uvm_pkg.sv`. `$UVM` is the `-uvmhome` argument, else `$UVM_HOME`, else `$DVICODE_UVM_HOME` (set by the `dvicode.uvmHome` setting). |
| `-uvmhome DIR` | UVM location for `-uvm`. |
| `-setenv NAME=VALUE` | Same as `+dvicode_setenv+NAME=VALUE`. |
| `-makelib LIB ... -endlib` | The files between are compiled; the library name is ignored. |

Options that take a value DVICode does not need (`-l log`, `-work lib`, `-timescale 1ns/1ps`,
`-L lib`, `-sv_lib`, `-cm`, `-access`, `-suppress`, ...) are skipped together with their
value. Other `+options+` and `-options` are skipped.

## DVICode directives

| Directive | Meaning |
|---|---|
| `+dvicode_init[+MODE]` | Start a new [invocation](#invocations): forget the macros, include directories and libraries of the previous one. `MODE` names the compiler being emulated and only changes how relative paths in `-f` files resolve. |
| `+dvicode_init_auto[+MODE]` | Start an invocation that is [auto-configured](#auto-configuration). Files and directories listed after it are added to what auto-configuration finds. |
| `+dvicode_setenv+NAME=VALUE` | Set an environment variable for the rest of the build file (and for nested `-f` files). `VALUE` may use other variables. |
| `+dvicode_setenv_no_override+NAME=VALUE` | Same, unless `NAME` is already set (in the environment, in `dvicode.env`, or earlier in the file). |
| `+dvicode_undefine+NAME` | Same as `+undefine+NAME`. |
| `+dvicode_skip_compile+PATTERN[+PATTERN...]` | Do not compile top files whose path matches a [wildcard](#paths-wildcards-and-environment-variables) pattern. A pattern without a leading `/` matches anywhere in the path: `*/obsolete/*`, `**/*_old.sv`. |
| `+dvicode_compilation_root+DIR` | Directory that relative paths resolve against from here on (default: the project folder). |
| `+dvicode_uvm_home+DIR` | UVM location; also compiles the library like `-uvm`. `+dvicode_uvmhome_override+DIR` is the same. |
| `+dvicode_include_config+FILE` | Read another build file here (its relative paths resolve against its own directory). |
| `+dvicode_test+FILE` | Compile `FILE` like a top file (kept for compatibility with test lists). |

## Paths, wildcards and environment variables

**Relative paths** resolve against the compilation root: the project folder unless
`+dvicode_compilation_root+` changes it. Inside an `-f` file, they resolve against the
directory of that `-f` file when the mode is `default`, a `vcs.*` mode, or when the
file was read with `-F`. In the other modes (`questa.*`, `ius.*`, `xcelium.*`) they resolve
against the compilation root, like those simulators do.

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

An undefined variable in a path is reported and the entry is skipped; in a define value it is
kept as written.

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

Hidden directories and common build output directories (`work`, `INCA_libs`, `xcelium.d`,
`csrc`, `simv.daidir`, `obj_dir`, `node_modules`, `target`, ...) are skipped. Auto-configuration
is a good start; a build file is better for projects with several copies of the same files or
files that must not be compiled together.

## Plusargs

Other `+NAME=VALUE` arguments are kept as simulation plusargs. DVICode uses:

| Plusarg | Use |
|---|---|
| `+UVM_TESTNAME=TEST` | Top of the UVM Verification Hierarchy view. |

## Problems in build files

Missing files and directories, undefined environment variables in paths, unreadable `-f`
files and unknown `+dvicode_` directives are shown as warnings on the line of the build file
(in the Problems view and in the editor). The build continues with the remaining entries.

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
