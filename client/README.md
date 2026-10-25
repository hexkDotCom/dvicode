# DVICode: DV Intelligence for VS Code

**Free, open-source SystemVerilog and UVM navigation for VS Code.**

Follow definitions across packages and class hierarchies, see what a macro expands to, and
explore your design and UVM testbench without starting a simulator. DVICode runs on your
desktop, in WSL, or beside your sources on a Linux server over SSH. Currently in preview.

![Hovering a UVM macro shows its expansion, right-click Go to Definition opens uvm_env in the UVM 1.2 library, a misspelled type is underlined as you type and fixed from the lightbulb, a deleted semicolon is flagged at once, and clicking components in the Verification Hierarchy jumps to their code](https://raw.githubusercontent.com/hexkDotCom/dvicode/main/docs/images/dvicode-demo.gif)

## Features

- **Errors as you type**: undeclared names, unknown types, members and ports, missing
  includes and macros, syntax mistakes. No errors on UVM 1.2, OpenTitan, Ibex, core-v-verif,
  CVA6 and PULP AXI with their own file lists.
- **Go anywhere**: definitions, references, implementations and overrides, through macros,
  packages, class inheritance and port connections.
- **Smart completion**: members after `.`, package items after `::`, macros after `` ` ``,
  and complete module instantiations in one step.
- **Hover documentation**: signatures, doc comments and what a macro expands to.
- **Inlay hints**: port, parameter and argument names on positional connections and calls.
- **Override and implement**: complete an inherited method's name in a class to override it,
  or implement every pure virtual method left with one code action.
- **Design Hierarchy** and **UVM Verification Hierarchy** views, worked out from the source:
  filter them by name or path, and let them follow the editor (jump with F12 and the view
  selects where you landed).
- **Formatter**: Format Document and Format Selection fix indentation and trailing blanks (and,
  when asked, runs of blank lines), nothing else: every result is checked to change whitespace
  only. Indentation follows the constructs (blocks, `case` items, one-statement bodies,
  headers, continuation lines, UVM `*_begin`/`*_end` macros); lines aligned by hand under the
  line above are kept. `` `define `` definitions, block comments, strings and protected
  regions are never touched. Settings under `dvicode.format`, or a `.dvicode/format.json` in
  the project. Alignment of declarations and connections is not done yet.
- **Parameters per instance**: the Design Hierarchy shows the parameter values each instance
  gets (overrides, `defparam`s, defaults, localparams and constant functions evaluated), the
  generate branches that exist in it and one block per generate loop copy (`g[0]`, `g[1]`);
  hover a parameter or a signal for its value or width in each instance. What cannot be
  evaluated is said, never guessed: branches whose condition is unknown are shown marked
  *undetermined*.
- **Register Model**: the maps, registers, addresses and fields a UVM register block's
  `build()` creates, read from its construction code, with search by address.
- **Assertions and Coverage**: right-click a signal or field to see the assertions, covers and
  coverpoints that use it (as the checked value, a clock or a reset), through named
  properties, port connections and `bind`s, and covergroup `sample()` arguments.
- **uvm_config_db**: hover a `get` to see the `set` calls that can provide it (field, type
  and scope matched as UVM 1.2 matches them, through the test's component hierarchy), and a
  `set` for the `get` calls it reaches; go to definition from a field name to its sets.
- **Instance repair**: complete port and parameter names in an instance's lists, connect the
  missing ports, expand `.*`, turn ordered connections into named ones, and see ports
  connected twice or ordered and named connections mixed.
- **Factory override impact**: hover a `set_*_override*` call to see which components it
  changes in the test's modeled build, which it may change, and why others are not changed.
- **Drivers and Loads**: right-click a signal or port and choose Show Drivers or Show Loads to
  see where it is written (assignments, `force`, output arguments, gate outputs) or read,
  following port connections down into instances and up to where its module is instantiated.
  The places are found from the source, not by elaborating the design: interfaces, `alias`,
  `bind` instances and connections through expressions are not followed.
- **Macro expansion**: see exactly what a `` `uvm_* `` macro turns into.
- **Type and call hierarchies**, outline, symbol search, rename, semantic highlighting.
- **Quick fixes**: "did you mean", "import package", "declare variable".
- **Uses your existing file lists** (`-f` files), or configures the project by itself.
- **Works where your sources live**: natively on Windows, Linux and macOS, in WSL, or on a
  Linux host over SSH, even for files that exist only on the server.
- **Lightweight**: a single engine of about 5 MB, installed for you. No telemetry.

| Macro hover | Design and UVM hierarchies | Problems and quick fixes |
|---|---|---|
| ![Hover on `uvm_component_utils showing the UVM definition and its expansion](https://raw.githubusercontent.com/hexkDotCom/dvicode/main/docs/images/hover.png) | ![The Design Hierarchy and the UVM Verification Hierarchy of demo_full_test next to the environment class](https://raw.githubusercontent.com/hexkDotCom/dvicode/main/docs/images/hierarchy.png) | ![Deliberate mistakes in the Problems view, with the quick fix "Did you mean 'state_e'?"](https://raw.githubusercontent.com/hexkDotCom/dvicode/main/docs/images/problems.png) |

## Getting started

Open a folder with SystemVerilog sources; the engine runs on your computer. For files in WSL
it runs in that WSL distribution; for files on a Linux server, run **DVICode: Open Folder on
the Server...** and pick the SSH host (SSH key or agent sign-in). To control the compile
order, add `.dvicode/default.build` in the simulator's argument-file syntax (`-f`,
`+incdir+`, `+define+`, `-uvm`, ...); without it the project is configured automatically.

## Documentation

- [Overview, settings and FAQ](https://github.com/hexkDotCom/dvicode#readme)
- [Build files](https://github.com/hexkDotCom/dvicode/blob/main/docs/build-files.md): every
  option and `+dvicode_` directive
- [Architecture](https://github.com/hexkDotCom/dvicode/blob/main/docs/architecture.md):
  remote setups, path mapping, launch modes
- Something wrong? Run **DVICode: Report Issue**: it fills in your setup and the log for you
  to review, or [open an issue](https://github.com/hexkDotCom/dvicode/issues).
