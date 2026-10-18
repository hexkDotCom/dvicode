# DVICode - DV Intelligence

SystemVerilog/UVM support for VS Code with a fast language server that can run locally, in
WSL2 or on a remote Linux host.

- Problems while you type: syntax, missing includes and macros, undeclared identifiers,
  unknown types, members and ports.
- Go to definition, find references, rename, hover with documentation, completion with module
  auto-instantiation, signature help, outline, workspace symbols, type and call hierarchy.
- Quick fixes: "Did you mean", "Import package", "Declare variable".
- Design Hierarchy, UVM Verification Hierarchy (`+UVM_TESTNAME` or a picked test) and
  Compiled Files views, Expand Macro, Show Including File.
- Build files in the simulator argument-file syntax (`.dvicode/default.build`, `-f` file lists,
  `+dvicode_*` directives), or automatic configuration.
- Remote: `\\wsl.localhost` folders, Windows folders through WSL, SSH hosts, `dvicode://`
  folders that live only on the server, custom launch commands (bsub/qrsh), TCP attach.

See **DVICode: Open Folder on the Server...** to work on files on a Linux host, and the
`dvicode.server.*` settings to choose where the server runs.
