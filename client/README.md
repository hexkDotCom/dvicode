# DVICode: DV Intelligence for VS Code

**Free, open-source SystemVerilog and UVM navigation for VS Code.**

Follow definitions across packages and class hierarchies, see what a macro expands to, and
explore your design and UVM testbench without starting a simulator. DVICode runs on your
desktop, in WSL, or beside your sources on a Linux server over SSH. Currently in preview.

## Features

- **Errors as you type**: undeclared names, unknown types, members and ports, missing
  includes and macros, syntax mistakes. No errors on UVM 1.2, OpenTitan, Ibex, core-v-verif,
  CVA6 and PULP AXI with their own file lists.
- **Go anywhere**: definitions, references, implementations and overrides, through macros,
  packages, class inheritance and port connections.
- **Smart completion**: members after `.`, package items after `::`, macros after `` ` ``,
  and complete module instantiations in one step.
- **Hover documentation**: signatures, doc comments and what a macro expands to.
- **Design Hierarchy** and **UVM Verification Hierarchy** views, worked out from the source.
- **Macro expansion**: see exactly what a `` `uvm_* `` macro turns into.
- **Type and call hierarchies**, outline, symbol search, rename, semantic highlighting.
- **Quick fixes**: "did you mean", "import package", "declare variable".
- **Uses your existing file lists** (`-f` files), or configures the project by itself.
- **Works where your sources live**: natively on Windows, Linux and macOS, in WSL, or on a
  Linux host over SSH, even for files that exist only on the server.
- **Lightweight**: a single engine of about 5 MB, installed for you. No telemetry.

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
