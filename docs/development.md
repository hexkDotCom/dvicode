# Developing DVICode

## Repository layout

```
server/                    Rust workspace
  crates/dvi-syntax        lexer, preprocessor, error-tolerant parser (declarations + references)
  crates/dvi-project       build files, auto-configuration
  crates/dvi-analysis      compilation, index, name resolution, IDE features
  crates/dvicode-server    LSP server (stdio/TCP), remote file system, `check` CLI
client/                    VS Code extension (TypeScript)
  src/launcher.ts          where the server runs, deployment
  src/pathmap.ts           URI mapping between VS Code and the server
  src/remotefs.ts          dvicode:// file system over the LSP connection
  test/                    end-to-end tests in a real VS Code
docs/                      architecture, build file reference, this file
examples/demo-project      sample project used by the tests
scripts/package.sh         build the static server, notices and the VSIX
scripts/third_party_notices.py   regenerate THIRD_PARTY_NOTICES.md
```

## Building

The server builds on Linux or WSL with stable Rust. Commands start in the repository root:

```bash
(cd server && cargo test)                       # unit + integration tests
scripts/package.sh                              # static musl server, notices, client/dvicode-*.vsix
scripts/package.sh --server                     # only the server, copied into client/server/linux-x64
```

`scripts/package.sh` also regenerates `THIRD_PARTY_NOTICES.md` from the bundled npm packages
and the linked Rust crates, and copies `LICENSE`, `NOTICE` and the Rust standard library
notices into the extension. Commit the regenerated `THIRD_PARTY_NOTICES.md` when dependencies
change.

## Testing on the command line

`dvicode-server check <dir> [--build FILE]` compiles a project and prints its problems.
IDE queries run without an editor; `LINE` and `COL` count from 1, like in the editor:

| Option | Query |
|---|---|
| `--goto F:LINE:COL` | go to definition |
| `--hover F:LINE:COL` | hover text |
| `--refs F:LINE:COL` | references |
| `--impl F:LINE:COL` | implementations |
| `--complete F:LINE:COL` | completion items |
| `--sig F:LINE:COL` | signature help |
| `--calls F:LINE:COL` | call hierarchy |
| `--expand F:LINE:COL` | macro expansion |
| `--outline F` | document symbols |
| `--fixes F` | quick fixes of a file |
| `--hier TOP` or `--hier -` | design hierarchy |
| `--uvm TEST` or `--uvm -` | verification hierarchy |
| `--symbols QUERY` | workspace symbols |

A good regression check is that clean libraries (UVM, your own verified code) report zero
problems, and that `examples/demo-project` with `.dvicode/errors.build` reports exactly the
mistakes marked in `rtl/broken.sv`.

## Performance

The figure in the README (UVM 1.2 in about 50 ms) is the `build` time that
`dvicode-server check` prints for a project containing only UVM 1.2 (`uvm_pkg.sv`: 142
files, 75,000 lines, 2.5 MB):

```bash
dvicode-server check <project> --env UVM_HOME=<uvm-1.2> -q
```

It covers preprocessing, parsing and indexing, not problem checking (printed separately).
Measured with a release build on Linux (WSL2) on an Intel Core Ultra 9 285K (24 cores),
files already in the operating system's cache: 46 to 56 ms over three runs.

## Internal errors

`dvicode/debug/fail` makes the engine fail on purpose, which exercises the whole path: the
log entry with a backtrace, the error response, the `dvicode/internalError` notice and the
issue text (see the last end-to-end test). Code that may panic in a new place should run
under `crash::catch` like its neighbours.

## End-to-end tests

The end-to-end tests start the installed VS Code (isolated profile, other extensions
disabled) on the demo project and check navigation, hover, completion, problems,
incremental edits, the design and verification hierarchies, call hierarchy, quick fixes and
macro expansion through VS Code's own APIs:

```bash
cd client && npm install && node esbuild.mjs
node test/runTest.js 'D:\path\to\examples\demo-project' win-drive         # Windows -> WSL, /mnt/d mapping
node test/runTest.js 'file://wsl.localhost/Ubuntu/home/me/demo' wsl-unc  # Windows -> WSL folder
node test/runTest.js 'dvicode://wsl+Ubuntu/home/me/demo' remote-fs       # files only in WSL
node test/runTest.js 'dvicode://ssh+me%40host/home/me/demo' ssh          # files and server over SSH
```

For the SSH variant, pass the SSH options through the environment, for example
`DVICODE_E2E_SETTINGS='{"dvicode.server.sshArgs": ["-p", "2222", "-i", "/path/key"]}'`.
Without `DVICODE_E2E_EXTENSION` the extension runs from `client/` and uses the engines in
`client/server/<platform>/`; set it to the `extension` folder of an unpacked `.vsix` to test a
package instead.

## Before a release

`scripts/prerelease.ps1` runs every check on a Windows machine with WSL and prints a summary;
it exits with 1 when a step fails:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\prerelease.ps1
powershell -ExecutionPolicy Bypass -File scripts\prerelease.ps1 -Vsix C:\Downloads\dvicode-0.1.1.vsix -Skip build,test
```

| Step | What it does |
|---|---|
| `build` | Native Windows engine (`cargo`), static Linux engine (`scripts/package.sh --server` in WSL), extension bundle |
| `test` | Server tests natively on Windows and in WSL |
| `unit` | The extension's unit tests (`npm run unit`) |
| `corpus` | The real-world corpus regression (`scripts/corpus`), compared with its baseline |
| `corpus-private` | The same with a private set of projects outside the repository, when `-PrivateCorpus PATH` or `DVICODE_CORPUS_MANIFEST` names its manifest (see [scripts/corpus/README.md](../scripts/corpus/README.md#private-projects)); the summary shows only counts |
| `e2e-native` | End-to-end tests with the engine on Windows |
| `e2e-wsl` | End-to-end tests in WSL: a Windows-drive folder, a `\\wsl.localhost` folder and a WSL-only `dvicode://` folder |
| `e2e-ssh` | End-to-end tests over SSH, with deployment of the engine, against a second WSL distribution |

Without `-Vsix` the end-to-end tests run the extension from the sources with the engines just
built; with `-Vsix` they test a package, typically the universal `.vsix` of a Release workflow
run (which also contains the release builds of the engines).

One-time setup on the Windows machine:

- Rust with the MSVC toolchain (`rustup-init.exe` from <https://rustup.rs>) and the Visual Studio
  Build Tools with the C++ workload; Node.js; VS Code; Git.
- A WSL distribution (default `Ubuntu`, option `-Distro`) with Rust (`rustup target add
  x86_64-unknown-linux-musl`), Python 3 and `rsync`.
- For `e2e-ssh`: a second WSL distribution (default `dvicode-remote`, option `-SshDistro`) that
  plays the remote Linux host. Install OpenSSH server there, create the user (default `dv`), and
  start a separate `sshd` on its own port with key login only, from `/etc/ssh/sshd_dvicode.conf`:

  ```
  Port 2222
  ListenAddress 0.0.0.0
  HostKey /etc/ssh/ssh_host_ed25519_key
  PasswordAuthentication no
  PubkeyAuthentication yes
  AuthorizedKeysFile .ssh/authorized_keys
  ```

  Create a key pair for the test (`ssh-keygen -t ed25519 -N "" -f
  %LOCALAPPDATA%\dvicode-test\e2e_key`), put the public key into `~dv/.ssh/authorized_keys` in that
  distribution, and allow only your user to read the private key (`icacls ... /inheritance:r
  /grant:r %USERNAME%:R`). The script starts that `sshd` itself. A distribution of another Linux
  family than the first one (for example a RHEL clone next to Ubuntu) also tests the engine on a
  second distribution.

## Releasing

See [releasing.md](releasing.md) for the platform packages, GitHub releases and publishing to the
VS Code Marketplace and Open VSX.
