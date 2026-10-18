# Changelog

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
