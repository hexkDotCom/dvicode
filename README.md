# DVICode: DV Intelligence for VS Code

**Fast, accurate SystemVerilog and UVM intelligence in VS Code, wherever your code lives.**

DVICode understands your whole design and testbench (macros, includes, packages, class
hierarchies, module ports) and keeps up as you type. Its small Rust engine runs on your
laptop, in WSL, or directly on your Linux build server, so you can edit from Windows or a Mac
at full speed.

## Features

- **Errors as you type**: undeclared names, unknown types, members and ports, missing
  includes and macros, syntax mistakes. Clean code stays clean: no false alarms on UVM and
  other real-world libraries.
- **Go anywhere**: definitions, references, implementations and overrides, through macros,
  packages, class inheritance and port connections.
- **Smart completion**: members after `.`, package items after `::`, macros after `` ` ``,
  and complete module instantiations in one step.
- **Hover documentation**: signatures, doc comments and what a macro expands to.
- **Design Hierarchy**: browse the instance tree from your top module.
- **UVM Verification Hierarchy**: see the component tree your test builds, from
  `+UVM_TESTNAME`.
- **Macro expansion**: see exactly what a `` `uvm_* `` macro turns into, one level or all.
- **Type and call hierarchies**, outline, workspace symbol search, rename and semantic
  highlighting (inactive `` `ifdef `` code is dimmed).
- **Quick fixes**: "did you mean", "import package", "declare variable".
- **Uses your existing file lists**: point it at the simulator's `-f` files, or let it
  configure the project by itself.
- **Remote-ready**: VS Code on Windows or macOS with the engine in WSL or on a Linux host
  over SSH, even for files that exist only on the server. Runs through `bsub`/`qrsh` too.
- **Lightweight**: one 3 MB binary, installed for you; compiles the whole UVM library in
  about 50 ms.

## Getting started

1. **Install** the extension: in VS Code open the Extensions view, choose **...** >
   **Install from VSIX...** and pick the `.vsix` for your platform from the Releases page.
2. **Open your project folder.** DVICode picks where to run:

   | Your setup | What happens |
   |---|---|
   | Files in WSL (`\\wsl.localhost\Ubuntu\home\me\proj`) | Runs in that WSL distribution. |
   | Files on a Windows drive | Runs in your default WSL distribution and sees the folder as `/mnt/<drive>/...`. |
   | Files on a Linux server | Run **DVICode: Open Folder on the Server...** and pick the SSH host. Needs SSH key or agent sign-in. |
   | Remote-SSH or Remote-WSL window | Runs next to your files on the remote side. |
   | Linux desktop | Runs locally. |

3. **Optional: describe the build.** Without a build file, DVICode finds your sources and
   works out the compile order. For full control, create `.dvicode/default.build` in the
   simulator's argument-file syntax:

   ```
   -uvm
   +incdir+tb/include
   +define+SIMULATION
   -f rtl/rtl.f
   tb/tb_pkg.sv
   tb/tb_top.sv
   +UVM_TESTNAME=base_test
   ```

   Everything that works in a build file is described in
   [docs/build-files.md](docs/build-files.md).

Want to try it first? Open [examples/demo-project](examples/demo-project), a small design
with a testbench. Switch to its `errors` build configuration (**DVICode: Set Current Build
Configuration...**) to see problem reporting at work.

## Commands and views

- **DVICode** in the activity bar: Design Hierarchy (pick the top with **Select Design
  Hierarchy Top...**), Verification Hierarchy (pick the test with **Select UVM Test...**) and
  Compiled Files.
- **DVICode: Expand Macro One Level**, **Expand Macro All Levels** and **Show Including
  File**; the first and last are also in the editor's context menu.
- **DVICode: Set Current Build Configuration...** to switch between `.dvicode/*.build` files.
- **DVICode: Show Project Status** (also the status bar item) and **Rebuild Project**.
- **DVICode: Open Folder on the Server...** to work on files that live on a Linux host.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `dvicode.server.mode` | `auto` | Where the engine runs: `auto`, `local`, `wsl`, `ssh`, `tcp`, `custom` |
| `dvicode.server.wslDistro` | | WSL distribution (default: the folder's, else the default one) |
| `dvicode.server.sshHost`, `.sshArgs`, `.sshCommand` | | SSH destination and options |
| `dvicode.server.launchCommand` | `${SERVER_CMD}` | Wrapper for `custom`, e.g. `ssh farm bsub -Is ${SERVER_CMD}` |
| `dvicode.server.tcpAddress`, `.tcpToken` | `127.0.0.1:7777` | Attach to `dvicode-server --listen HOST:PORT --token T` |
| `dvicode.server.path` | | Engine binary on the target (default: bundled, installed to `~/.dvicode/server/`) |
| `dvicode.server.autoDeploy` | `true` | Install the bundled engine on WSL and SSH targets |
| `dvicode.pathMappings` | `[]` | Same folder seen by both sides, e.g. `[{ "local": "Z:\\proj", "remote": "/home/me/proj" }]` |
| `dvicode.buildFile` | | Build configuration (relative to the folder) |
| `dvicode.env` | `{}` | Variables for `$VAR` in build files |
| `dvicode.uvmHome` | | UVM location for `-uvm` when `$UVM_HOME` is not set |
| `dvicode.maxProblems` | `1000` | Problems shown after a full build (open files always get all of theirs) |

## How it works

```
 VS Code on Windows / macOS / Linux             Linux (WSL2, remote host, or local)
 ┌──────────────────────────────┐   LSP over    ┌───────────────────────────────────┐
 │ DVICode extension            │  stdio of     │ dvicode-server (Rust, static)     │
 │  starts the engine where     │  wsl.exe/ssh  │  build file → compile order       │
 │  your files are, maps paths  │ ────────────▶ │  preprocessor, tolerant parser    │
 │  hierarchy and file views    │               │  index, resolution, IDE features  │
 └──────────────────────────────┘               └───────────────────────────────────┘
```

The extension starts the engine through `wsl.exe` or `ssh` and talks the Language Server
Protocol over that connection, so no ports, firewalls or certificates are involved. See
[docs/architecture.md](docs/architecture.md) for the details.

## Status

DVICode is young and moving fast. Not supported yet: VHDL, e and PSS; elaboration of
parameter values and `generate` conditions; UVM factory overrides and config-db views;
diagrams; code formatting; lint rules. Bug reports and real-world code that trips it up are
very welcome.

## Contributing

Building, testing and the code layout are described in
[docs/development.md](docs/development.md).

## License

Apache License 2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE). Third-party components and
their licenses are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
