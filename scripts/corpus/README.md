# Real-world regression corpus

`run.sh` checks the engine against large open-source SystemVerilog projects: it clones them at
pinned commits, runs `dvicode-server check` on each configuration (a project plus a build
file), runs IDE queries on every file to find crashes, and compares the results with
`baseline.json`. It runs in Linux or WSL (bash, git, python3, coreutils `timeout`, util-linux
`flock`).

Valid code that the projects' simulators accept should produce no problems, so a problem
reported on a configuration with a real build file is a false positive in the engine, or a
build file that does not match the project's flow. Auto-configured runs of whole repositories
also count problems that come from auto-configuration itself (several copies of the same
package, templates, files that need defines from a makefile); they are there to catch crashes,
slowdowns and changes, not to reach zero.

## Quick start

```bash
scripts/corpus/run.sh all            # fetch, build the engine, check, sweep, compare
```

or step by step:

```bash
scripts/corpus/run.sh fetch          # clone or move the projects to the pinned commits
scripts/corpus/run.sh check          # builds the engine (or --bin PATH), writes results
scripts/corpus/run.sh sweep          # IDE queries on every file
scripts/corpus/run.sh compare        # exit 1 on a regression against baseline.json
scripts/corpus/run.sh table          # summary of the results
scripts/corpus/run.sh fuzz           # random edits: incremental updates against full builds
scripts/corpus/run.sh writes         # renames and quick fixes applied, results checked
```

From Windows: `wsl.exe -e bash -c 'cd /mnt/d/.../hexk-dvicode && scripts/corpus/run.sh all'`
(use `-e`, otherwise `wsl.exe` expands `$` variables itself).

Options: `--only REGEX` (configurations whose name matches), `--bin PATH` (an engine binary
instead of building one with `scripts/wsl-cargo.sh`), `--out DIR` (results directory),
`--jobs N` and `--chunk N` (see [Parallel runs](#parallel-runs)),
`--every N`, `--cap N`, `--query-timeout S`, `--sweep-uvm` (see [Sweep](#sweep)), `--edits N`,
`--seed S` (see [Fuzz](#fuzz)), `--renames N` (see [Writes](#writes)), `--manifest PATH` and
`--baseline PATH` (see [Private projects](#private-projects)).

Environment:

| Variable | Default | Use |
|---|---|---|
| `CORPUS` | `~/.cache/dvicode-corpus` | Clones (`$CORPUS/<project>`), generated files (`$CORPUS/.gen`), Python venv (`$CORPUS/.venv`), results (`$CORPUS/results`) |
| `UVM_HOME` | `~/corpus/uvm-1.2` | UVM 1.2 for the `-uvm` build files and auto-configuration (the Accellera 1.2 release) |
| `CARGO_TARGET_DIR` | `~/.cache/dvicode-target` | Where `run.sh build` builds the engine |
| `DVICODE_CORPUS_MANIFEST` | (unset) | A manifest to use instead of `manifest.json` (see [Private projects](#private-projects)) |
| `OUT` | `$CORPUS/results` | Results directory for `manifest.json` (another manifest keeps its results next to it) |

Keep `CORPUS` on the Linux file system: `/mnt/<drive>` is much slower. The clones are never
part of this repository; only the manifest, the build files and the baseline are.

## Parallel runs

`check`, `sweep`, `fuzz` and `writes` run `--jobs N` engine processes at a time (default: half
the CPUs, at least 1). Each configuration keeps its own result files, so the results and
`compare` are the same whatever N is; only the times change. With `--jobs 1` everything runs
one after the other with its output as it comes; with more, each configuration's lines are
printed together when it ends (`sweep` prints the line counting a configuration's queries
when it starts, and its summary when it ends).

`sweep` puts the queries of all configurations in one queue: each configuration's queries are
split into chunks, each run by its own engine process, so that one large configuration does not
set the time of the whole sweep. A chunk has at least `--chunk N` queries (default 2000), and a
configuration has no more chunks than `--jobs`, since every process builds the project again
first; `--chunk 0` runs each configuration in one process, and so does `--jobs 1`. The time in
`<configuration>.sweep.json` adds up the times of its processes.

The engine uses every CPU for a build, so builds side by side slow each other down. After a
parallel `check`, the configurations whose build time `compare` would report as more than
twice the baseline's are checked again one at a time, and their results replaced: a real
slowdown stays, the effect of the other builds does not.

On a 24-thread WSL installation the public corpus takes about 6 s to check and 2 minutes to
sweep with `--jobs 1`, and 2 s and 22 s with the default 12.

## Projects and configurations

[manifest.json](manifest.json) lists the projects (repository and pinned commit, plus
submodules to fetch) and the configurations. A configuration names a project and a build:

- a file in [builds/](builds), which refers to the clones through `$CORPUS` (and to generated
  files through `$CORPUS/.gen`);
- `auto`: auto-configuration of the whole clone (`check --build auto`);
- `project`: what the editor would use, the project's own `.dvicode/default.build` or
  auto-configuration.

Optional `env` values are passed with `--env`; `CORPUS` and `UVM_HOME` always are. `path`
projects are local checkouts used where they are (the demo project and the earlier local
corpus in `~/corpus`): `fetch` leaves them alone and fails when a non-optional one is missing,
missing optional ones are skipped, and the results record their `git rev-parse HEAD` (if any)
for reference. Paths may use `$VARIABLES` and `~`; a Windows path (`D:\work\x`) is converted
with `wslpath`; a relative path, build files included, resolves against the manifest's folder.

| Configuration | Source of the build file |
|---|---|
| `opentitan-uart`, `-i2c`, `-aes` | The FuseSoC file list of the UVM testbench (`lowrisc:dv:<ip>_sim`), generated by [gen-builds.sh](gen-builds.sh) the way OpenTitan's dvsim runs FuseSoC, plus the defines of `hw/dv/tools/dvsim/common_sim_cfg.hjson` (aes: build mode `aes_masked`) |
| `opentitan-earlgrey-rtl` | FuseSoC file list of `lowrisc:systems:top_earlgrey` |
| `ibex-dv` | `dv/uvm/core_ibex/ibex_dv.f` with the arguments of `yaml/rtl_simulation.yaml` and `scripts/compile_tb.py` |
| `cv32e40p-uvm` | core-v-verif `mk/uvmt/uvmt.mk` and `xrun.mk`: `cv32e40p_manifest.flist`, `uvmt_cv32e40p.flist`, `imperas_iss.flist` with the `DV_*` variables; the RTL clone is pinned to the commit `cv32e40p/sim/ExternalRepos.mk` asks for |
| `cva6-rtl` | `core/Flist.cva6` with the Makefile's default target `cv64a6_imafdc_sv39` |
| `pulp-axi-rtl` | The `Bender.yml` sources of tech_cells_generic, common_cells (at the commit axi depends on) and axi, generated by [bender_flist.py](bender_flist.py) |
| `*-auto` | Auto-configuration of the clone |
| `demo`, `demo-errors`, `ubus`, `uvm-only`, `hexk-sv-lib`, `uvm11d-proj` | The earlier local corpus (`demo-errors` must keep its 9 deliberate problems) |

OpenTitan's UVM register packages and CSR assertion modules are generated by its own
`util/regtool.py`: a `// corpus-setup: regtool OPT NAME HJSON` line in a build file makes
`fetch` run it into `$CORPUS/.gen/opentitan/NAME`, in a venv that `fetch` creates
(hjson, mako, ...). Known missing pieces: core-v-verif's svlib (downloaded from Bitbucket by
its makefile) is not fetched, which gives two build-file warnings in `cv32e40p-uvm`.

### Changing a pinned commit

1. Update the commit in `manifest.json` and run `run.sh fetch`.
2. Regenerate the derived build files: `$CORPUS/.venv/bin/pip install fusesoc==2.4.5` (the
   version OpenTitan pins), then `scripts/corpus/gen-builds.sh`. Check the hand-written build
   files against the projects' file lists.
3. Run `check` and `sweep`, look at the changes (`compare`), and update the baseline.

## Results

`$CORPUS/results/<configuration>.json`, one per configuration:

| Field | Meaning |
|---|---|
| `errors`, `warnings`, `total`, `codes` | Problem counts, by problem code |
| `top_files`, `files`, `declarations`, `macros`, `packages`, `classes`, `design_elements` | From the engine's summary |
| `config_ms`, `build_ms`, `diag_ms`, `wall_s` | Times |
| `exit`, `signal`, `panic`, `timeout`, `completed` | How the run ended |
| `top_problem_files` | The 20 files with the most problems |
| `build_config_problems` | Warnings about the build file itself (missing files, ...) |

`<configuration>.problems.txt` has every problem (`file:line:col: Severity: CODE: message`)
and `<configuration>.stderr.txt` the engine's summary and build-file warnings. A configuration
gets `timeout` seconds (manifest, default 600).

## Sweep

`run.sh sweep` asks for the outline of every file and, on every 20th line (`--every`), for
hover, go to definition and completion at each identifier and references at every third one,
at most 30 positions per file (`--cap`). The UVM library's own files are skipped unless
`--sweep-uvm` is given. The queries run in order in engine processes
(`check --queries FILE`, one per chunk: see [Parallel runs](#parallel-runs)), which catch panics
and report them per query, and exit when one query takes longer than `--query-timeout` seconds
(default 60). When a process crashes (a signal such as a stack overflow) or a query hangs, the
query is recorded and its chunk continues after it in a new process; when the project build
itself crashes, the sweep of the configuration ends there. `<configuration>.sweep.json` lists
the crashes in query order, the time per query kind and the slowest queries;
`<configuration>.sweep/` keeps the raw output (`all.q`, the queries; `state.json`; `stderr.txt`;
a folder per chunk).

## Fuzz

`run.sh fuzz` checks incremental updates, which the other commands never exercise:
`check --fuzz-edits N --seed S` makes N random but realistic edits as editor contents
(insert, delete, move and paste lines; rename an identifier in a file; add, change and remove
`` `define ``, `` `include `` and `import`; add class members; break the syntax and undo;
type a line in pieces; open a file outside the build; close files; edit headers that many
files include) and analyses each one the way the server does after `didChange`. After every
edit it compares the result with a full build of the same texts: the problems of every file,
the problems the editor would show (the server republishes only some files after an edit),
the outline of the edited files, and go to definition, hover and references at positions in
the edited files and where names from the edited lines are used. The clones are never
written.

Any difference is a bug in the incremental path. `<configuration>.fuzz.txt` has a reproducer
for each: the editor contents to start from (as changes to the files on disk) and the fewest
last edits that still show the difference, then the differences. A run stops at three
findings. Options: `--edits N` (default 60; each edit costs a full build, so keep it small
for the `*-auto` configurations), `--seed S` (default 1; the same seed makes the same edits).
`fuzz` and `writes` are not part of `all`.

For a single project: `dvicode-server check DIR [--build FILE] --fuzz-edits 200 --seed 7`
(`--fuzz-verbose` prints every edit; `--fuzz-disk` also writes some edits to the files and
reports them like watched-file changes, so use it only on a copy). When a difference needs a
long history, the reducer removes edits from the whole sequence (delta debugging) before
looking for the shortest tail. `--fuzz-save DIR` writes each reproducer as JSON, and
`check DIR --fuzz-replay FILE` replays one and prints the differences it still shows, which is
the quickest way to debug a finding. The server's unit tests run a short seeded session on the
demo project.

## Writes

`run.sh writes` checks the edits the server asks the editor to make:
`check --write-check N --seed S` renames N elements at sampled positions (macro uses,
`` `define `` names and `` `ifdef `` operands, names in macro arguments, names in headers, any
name) and applies the quick fixes for problems it makes on purpose (a misspelled name, a
removed `import`). Each workspace edit is applied as the editor applies it (every document
must carry the version of the open document, or none; ranges must not overlap), and the
result is compiled again from scratch. A finding is a rename that adds problems, edits places
that are not the element's references or misses some, changes anything but the name or the
line endings, or touches a file outside the workspace or a built-in file; a "Did you mean"
that does not restore the text; an "Import package" after which the file's problems differ
from what they were with the import. Refused renames (outside the workspace, a name used
inside a macro body or built by token pasting) and missing quick fixes are notes.
`<configuration>.writes.txt` lists both. `--renames N` (default 40), `--seed S`.

## Latency

`latency.py` measures what an editor waits for, on one configuration and one file: it starts
`dvicode-server --stdio`, waits for the first build, opens the file, times hover, completion
and semantic tokens, then types a line before the file's last `endmodule`/`endclass`/
`endpackage` one character every `--interval` ms (100) with a completion request after each,
and adds a declaration. It prints one JSON line: the first build, each request's time, the
completion times while typing (p50, p95, max), when the problems of the last version arrived,
when the project's problems settled after the declaration (no problems for `--settle` ms), and
the engine's memory (Linux). The typed line names something undeclared, so that every version
has a problem to report. `LATENCY_DEBUG=1` traces the messages; `--log FILE` keeps the
engine's log (`LATENCY_LOG_LEVEL=debug` for more).

```bash
python3 scripts/corpus/latency.py --server ~/.cache/dvicode-target/release/dvicode-server \
  --manifest scripts/corpus/manifest.json --only '^opentitan-auto$' \
  --file hw/ip/i3c/rtl/i3c_reg_top.sv --json latency.jsonl
```

## Baseline and compare

`run.sh compare` compares `$CORPUS/results` with [baseline.json](baseline.json) and exits 1
when a configuration

- crashed, timed out, panicked or did not finish, or a sweep found a crash or a hang;
- reports a problem code that the baseline does not have for it;
- reports more problems than the baseline;
- takes more than twice the baseline's build time (config + build), ignoring differences under
  500 ms (`corpus.py compare --slack-ms`), which are noise.

Fewer problems are reported as a note: after checking that the change is a fix, record it with
`run.sh baseline` (all results) or `run.sh baseline --only REGEX --merge` (keeps the other
configurations). The times in the baseline come from one machine (a 24-thread WSL
installation); on a much slower machine, record a local baseline first.

## Private projects

The same checks can run on projects that must not leave your machine (proprietary code). A
private set is a folder of its own, outside this repository, with a manifest, its build files,
its baseline and its results:

```
~/dvicode-private/            (or %USERPROFILE%\dvicode-private on Windows)
  manifest.json               projects and configurations
  *.build                     build files (paths relative to the project folder)
  baseline.json               written by `run.sh baseline`
  results/                    written by check and sweep
```

[private-example/](private-example) shows the format with this repository's demo project;
copy it to start:

```bash
cp -r scripts/corpus/private-example ~/dvicode-private
```

In `manifest.json`, list the projects as local checkouts with `path` (a Linux path such as
`/home/me/work/projX`, `~/work/projX`, or a Windows path such as `D:/work/projX` or
`D:\\work\\projX` in JSON, which is converted with `wslpath`); `url` + `commit` projects (a private Git server) work too and are
cloned into `$CORPUS`. Give each configuration a build file next to the manifest, `auto`, or
`project` (the checkout's own `.dvicode/default.build`). Checkouts on the Linux file system
are much faster than `/mnt/<drive>`.

Point `run.sh` at the manifest with `--manifest PATH` (the file, or the folder that holds
`manifest.json`) or the `DVICODE_CORPUS_MANIFEST` environment variable. The baseline defaults
to `baseline.json` next to the manifest (`--baseline PATH` to change it) and the results to
`results/` next to it, so nothing is written to the repository or mixed with the public
results:

```bash
export DVICODE_CORPUS_MANIFEST=~/dvicode-private/manifest.json
scripts/corpus/run.sh check          # problems per configuration
scripts/corpus/run.sh sweep          # IDE queries on every file
scripts/corpus/run.sh baseline       # first time: record baseline.json (review it first)
scripts/corpus/run.sh compare        # later runs: exit 1 on a regression
scripts/corpus/run.sh all            # fetch (checks the checkouts), build, check, sweep, compare
```

Before a release, `scripts/prerelease.ps1` runs it as the `corpus-private` step after the
public `corpus` step when `DVICODE_CORPUS_MANIFEST` is set on Windows or `-PrivateCorpus PATH`
is given (a Windows path or a path in WSL):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\prerelease.ps1 -PrivateCorpus $env:USERPROFILE\dvicode-private\manifest.json
```

The step's output on the console names your projects and files; its line in the summary only
gives the number of configurations, regressions and notes.

Keeping it private:

- Never put the private folder inside this repository or any other public one, and do not
  copy its build files, baseline or results into issues or pull requests: problem messages,
  `top_problem_files` and sweep crashes contain file, module and class names.
- When an engine problem shows up only in a private project, reduce it to a small example with
  invented names before reporting it, and review everything you attach (logs included) for
  paths and names from your projects.

## Files

| File | Purpose |
|---|---|
| `run.sh` | The runner |
| `corpus.py` | Manifest reading, result JSON, the sweep, compare and baseline (Python standard library) |
| `manifest.json` | Projects, pinned commits, configurations |
| `builds/` | Build files |
| `baseline.json` | Expected results |
| `private-example/` | The format of a private set of projects, with the demo project |
| `gen-builds.sh`, `fusesoc_build.py`, `bender_flist.py` | Regenerate the build files derived from FuseSoC cores and Bender manifests |
