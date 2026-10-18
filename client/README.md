# DVICode: DV Intelligence for VS Code

**Fast, accurate SystemVerilog and UVM intelligence in VS Code, wherever your code lives.**

DVICode understands your whole design and testbench (macros, includes, packages, class
hierarchies, module ports) and keeps up as you type. Its small Rust engine runs on your
laptop, in WSL, or directly on your Linux build server, so you can edit from Windows or a Mac
at full speed.

## Features

- **Errors as you type**: undeclared names, unknown types, members and ports, missing
  includes and macros, syntax mistakes. Clean code stays clean.
- **Go anywhere**: definitions, references, implementations and overrides, through macros,
  packages, class inheritance and port connections.
- **Smart completion**: members after `.`, package items after `::`, macros after `` ` ``,
  and complete module instantiations in one step.
- **Hover documentation**: signatures, doc comments and what a macro expands to.
- **Design Hierarchy** and **UVM Verification Hierarchy** views.
- **Macro expansion**: see exactly what a `` `uvm_* `` macro turns into.
- **Type and call hierarchies**, outline, symbol search, rename, semantic highlighting.
- **Quick fixes**: "did you mean", "import package", "declare variable".
- **Uses your existing file lists** (`-f` files), or configures the project by itself.
- **Remote-ready**: the engine runs in WSL or on a Linux host over SSH, even for files that
  exist only on the server, or through `bsub`/`qrsh`.
- **Lightweight**: one 3 MB binary, installed for you.

## Getting started

Open a folder with SystemVerilog sources. On Windows the engine runs in WSL; for files on a
Linux server, run **DVICode: Open Folder on the Server...** and pick the SSH host (SSH key or
agent sign-in). To control the compile order, add `.dvicode/default.build` in the simulator's
argument-file syntax (`-f`, `+incdir+`, `+define+`, `-uvm`, ...); without it the project is
configured automatically.

The `dvicode.server.*` settings choose where the engine runs. The project's documentation
describes build files, settings and remote setups in detail.
