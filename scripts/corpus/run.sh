#!/usr/bin/env bash
# Real-world regression harness for the DVICode engine (runs in Linux or WSL).
# See scripts/corpus/README.md. Usage: scripts/corpus/run.sh COMMAND [OPTIONS]
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
export REPO
export CORPUS="${CORPUS:-$HOME/.cache/dvicode-corpus}"
export UVM_HOME="${UVM_HOME:-$HOME/corpus/uvm-1.2}"
MANIFEST="${DVICODE_CORPUS_MANIFEST:-}"
BASELINE=""
OUT_ARG=""
BIN="${DVICODE_BIN:-}"
ONLY=""
EVERY=20
CAP=30
QUERY_TIMEOUT=60
SWEEP_UVM=0
FUZZ_EDITS=60
FUZZ_SEED=1
RENAMES=40
JOBS=""
CHUNK=2000
PY=python3

usage() {
    cat <<'EOF'
Usage: scripts/corpus/run.sh COMMAND [OPTIONS]

Commands:
  fetch      clone the projects (or move them) to the pinned commits, generate sources;
             local checkouts (`path` projects) are left alone
  build      build the release engine with scripts/wsl-cargo.sh
  check      run `dvicode-server check` for every configuration: OUT/<config>.json
  sweep      run IDE queries on every file of every configuration: OUT/<config>.sweep.json
  fuzz       random edits, each incremental update compared with a full build:
             OUT/<config>.fuzz.txt has a reproducer per difference; exit 1 on any
  writes     renames and quick fixes applied like the editor does, each result checked:
             OUT/<config>.writes.txt lists findings and notes; exit 1 on findings
  compare    compare OUT with baseline.json; exit 1 on regressions
  baseline   write baseline.json from OUT (--merge keeps configurations not in OUT)
  table      print a summary of OUT
  all        fetch, build (unless --bin), check, sweep, compare

Options:
  --manifest PATH    projects and configurations, or a folder with a manifest.json (default
                     $DVICODE_CORPUS_MANIFEST, else scripts/corpus/manifest.json); a Windows
                     path is converted with wslpath
  --baseline PATH    baseline (default baseline.json next to the manifest)
  --bin PATH         engine binary (default: build it; or $DVICODE_BIN)
  --only REGEX       only configurations whose name matches
  --out DIR          results directory (default $CORPUS/results for scripts/corpus/manifest.json,
                     results/ next to any other manifest)
  --every N          sweep: query identifiers on every Nth line (default 20)
  --cap N            sweep: at most N positions per file (default 30)
  --query-timeout S  sweep: one query may take S seconds (default 60)
  --sweep-uvm        sweep: also query the UVM library's files
  --edits N          fuzz: edits per configuration (default 60)
  --seed S           fuzz, writes: random seed (default 1)
  --renames N        writes: renames per configuration (default 40)
  --merge            baseline: keep configurations that have no result in OUT
  --jobs N           check, sweep, fuzz, writes: run N engine processes at a time (default half
                     the CPUs); with N > 1 each configuration's output is printed when it ends
  --chunk N          sweep: at most N queries per engine process (default 2000; 0: one
                     process per configuration)

Environment: CORPUS (clones, default ~/.cache/dvicode-corpus), UVM_HOME (default
~/corpus/uvm-1.2), CARGO_TARGET_DIR (for build), DVICODE_CORPUS_MANIFEST (see --manifest),
OUT (results directory for scripts/corpus/manifest.json only).
EOF
}

CMD="${1:-}"
[ -n "$CMD" ] && shift
MERGE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --manifest) MANIFEST="$2"; shift ;;
        --baseline) BASELINE="$2"; shift ;;
        --bin) BIN="$2"; shift ;;
        --only) ONLY="$2"; shift ;;
        --out) OUT_ARG="$2"; shift ;;
        --every) EVERY="$2"; shift ;;
        --cap) CAP="$2"; shift ;;
        --query-timeout) QUERY_TIMEOUT="$2"; shift ;;
        --sweep-uvm) SWEEP_UVM=1 ;;
        --edits) FUZZ_EDITS="$2"; shift ;;
        --seed) FUZZ_SEED="$2"; shift ;;
        --renames) RENAMES="$2"; shift ;;
        --merge) MERGE=--merge ;;
        --jobs) JOBS="$2"; shift ;;
        --chunk) CHUNK="$2"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "run.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

log() { echo "[corpus] $*" >&2; }

[ -n "$JOBS" ] || JOBS=$(( $(nproc) / 2 ))
case "$JOBS" in ''|*[!0-9]*) echo "run.sh: --jobs needs a number" >&2; exit 2 ;; esac
[ "$JOBS" -ge 1 ] || JOBS=1
case "$CHUNK" in ''|*[!0-9]*) echo "run.sh: --chunk needs a number" >&2; exit 2 ;; esac

# A path given on Windows (D:\work\x, D:/work/x, \\server\share) as a Linux path.
linux_path() {
    case "$1" in
        [A-Za-z]:[\\/]*|\\\\*) wslpath -u "$1" ;;
        *) echo "$1" ;;
    esac
}

# The manifest decides where the baseline and the results live: a private manifest keeps
# everything in its own folder, the public one keeps results in $CORPUS (out of the repository).
PUBLIC="$(realpath -m "$HERE/manifest.json")"
MANIFEST="$(realpath -m "$(linux_path "${MANIFEST:-$PUBLIC}")")"
[ -d "$MANIFEST" ] && MANIFEST="$MANIFEST/manifest.json"
case "$CMD" in
    ""|-h|--help|build|table) ;;
    *) [ -f "$MANIFEST" ] || { echo "run.sh: no manifest at $MANIFEST" >&2; exit 2; } ;;
esac
MDIR="$(dirname "$MANIFEST")"
BASELINE="$(realpath -m "$(linux_path "${BASELINE:-$MDIR/baseline.json}")")"
if [ -n "$OUT_ARG" ]; then
    OUT="$(linux_path "$OUT_ARG")"
elif [ "$MANIFEST" = "$PUBLIC" ]; then
    OUT="${OUT:-$CORPUS/results}"
else
    OUT="$MDIR/results"
fi
[ "$MANIFEST" = "$PUBLIC" ] || log "manifest $MANIFEST, results $OUT"

# --- fetch ---------------------------------------------------------------------------------

fetch_project() {
    local name="$1" url="$2" commit="$3" subs="$4" dir="$CORPUS/$1"
    if [ ! -d "$dir/.git" ]; then
        log "cloning $name"
        git init -q "$dir" && git -C "$dir" remote add origin "$url"
    fi
    if [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" != "$commit" ]; then
        log "$name: checking out $commit"
        git -C "$dir" fetch -q --depth 1 origin "$commit" || git -C "$dir" fetch -q origin || return 1
        git -C "$dir" checkout -q --force --detach "$commit" || return 1
    fi
    local s
    for s in ${subs//,/ }; do
        # Recursive: core/cvfpu has a submodule of its own. A no-op when already checked out.
        git -C "$dir" submodule update -q --init --recursive --depth 1 -- "$s" ||
            git -C "$dir" submodule update -q --init --recursive -- "$s" || return 1
    done
}

venv() {
    local v="$CORPUS/.venv"
    if [ ! -x "$v/bin/python" ] || ! "$v/bin/python" -c 'import hjson, mako, systemrdl, yaml, Crypto' 2>/dev/null; then
        log "creating $v (hjson, mako, ... for OpenTitan's regtool)"
        python3 -m venv "$v" && "$v/bin/pip" install -q hjson mako semantic_version tabulate mistletoe pyyaml \
            systemrdl-compiler peakrdl-systemrdl pycryptodome || return 1
    fi
    echo "$v/bin/python"
}

# `// corpus-setup: regtool OPT NAME HJSON` in a build file: run the project's util/regtool.py
# (OPT -s: UVM register package, -f: CSR assertions) into $CORPUS/.gen/<project>/NAME.
run_setup() {
    local project tool opt name hjson py out file
    while IFS=$'\t' read -r project line; do
        read -r tool opt name hjson <<<"$line"
        if [ "$tool" != regtool ]; then log "unknown corpus-setup: $line"; continue; fi
        out="$CORPUS/.gen/$project/$name"
        if [ "$opt" = -s ]; then file="$out/${name}_ral_pkg.sv"; else file="$out/${name}_csr_assert_fpv.sv"; fi
        [ -s "$file" ] && continue
        py="$(venv)" || return 1
        mkdir -p "$out"
        log "generating $project $name ($opt)"
        (cd "$CORPUS/$project" && "$py" util/regtool.py "$opt" -t "$out" "$hjson") >/dev/null || return 1
    done < <($PY "$HERE/corpus.py" setup "$MANIFEST" --only "$ONLY")
}

cmd_fetch() {
    mkdir -p "$CORPUS"
    local rc=0
    while IFS=$'\t' read -r name url commit subs; do
        fetch_project "$name" "$url" "$commit" "$subs" || { log "$name: fetch failed"; rc=1; }
    done < <($PY "$HERE/corpus.py" projects "$MANIFEST")
    # Local checkouts are used as they are; check records their commit for reference.
    local dir optional c
    while IFS=$'\t' read -r name dir optional; do
        if [ -d "$dir" ]; then
            c="$(commit_of "$dir")"
            log "$name: local checkout $dir${c:+ at ${c:0:12}}"
        elif [ "$optional" = 1 ]; then
            log "$name: $dir missing (optional, skipped)"
        else
            log "$name: $dir missing"; rc=1
        fi
    done < <($PY "$HERE/corpus.py" locals "$MANIFEST")
    run_setup || rc=1
    return $rc
}

# --- build ---------------------------------------------------------------------------------

cmd_build() {
    command -v cargo >/dev/null || source "$HOME/.cargo/env"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/dvicode-target}"
    "$REPO/scripts/wsl-cargo.sh" build --release -q -p dvicode-server || return 1
    BIN="$CARGO_TARGET_DIR/release/dvicode-server"
    log "engine: $BIN"
}

need_bin() {
    if [ -z "$BIN" ]; then cmd_build || exit 1; fi
    [ -x "$BIN" ] || { echo "run.sh: no engine binary at $BIN" >&2; exit 1; }
}

# --- check ---------------------------------------------------------------------------------

commit_of() { git -C "$1" rev-parse HEAD 2>/dev/null || echo ""; }

# Runs the engine with a timeout; sets RC, WALL, TIMED_OUT.
run_engine() {
    local timeout="$1" stdout="$2" stderr="$3"; shift 3
    local t0 ms
    t0=$(date +%s%N)
    # In a subshell with stderr closed, so that a crash does not print "Segmentation fault".
    RC=$({ timeout --kill-after=10 "$timeout" "$BIN" check "$@" >"$stdout" 2>"$stderr"; echo $?; } 2>/dev/null)
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    WALL=$(printf "%d.%03d" $((ms / 1000)) $((ms % 1000)))
    TIMED_OUT=""
    if [ $RC -eq 124 ] && ! grep -q "query timeout after" "$stderr"; then TIMED_OUT=--timed-out; fi
    if [ $RC -eq 137 ]; then TIMED_OUT=--timed-out; fi
}

# Engine arguments for a configuration: ARGS=(DIR [--build FILE] --env ...).
engine_args() {
    local dir="$1" build="$2" envs="$3" e
    ARGS=("$dir")
    [ "$build" = project ] || ARGS+=(--build "$build")
    ARGS+=(--env "CORPUS=$CORPUS" --env "UVM_HOME=$UVM_HOME")
    local list=()
    [ -n "$envs" ] && IFS=$'	' read -r -a list <<<"$envs"
    for e in "${list[@]}"; do ARGS+=(--env "$e"); done
}

# Runs FUNC NAME PROJECT DIR BUILD TIMEOUT OPTIONAL ENVS for each configuration, --jobs at a
# time; returns 1 when one of them did. In parallel, a configuration's output is held back and
# printed when it ends, so that the lines of different configurations do not mix.
each_config() {
    local fn="$1" rc=0 logs n=0 f p pids=() alive
    local name project dir build timeout optional envs
    if [ "$JOBS" -le 1 ]; then
        while IFS=$'\t' read -r name project dir build timeout optional envs; do
            "$fn" "$name" "$project" "$dir" "$build" "$timeout" "$optional" "$envs" || rc=1
        done < <($PY "$HERE/corpus.py" configs "$MANIFEST" --only "$ONLY")
        return $rc
    fi
    logs="$(mktemp -d)"
    # `timeout` puts the engines in process groups of their own: stop the whole trees.
    trap 'stop_jobs; rm -rf "$logs"; exit 130' INT
    trap 'stop_jobs; rm -rf "$logs"; exit 143' TERM
    while IFS=$'\t' read -r name project dir build timeout optional envs; do
        # `wait -n` also returns for the process substitution, so count the workers left.
        while :; do
            alive=()
            for p in "${pids[@]}"; do kill -0 "$p" 2>/dev/null && alive+=("$p"); done
            pids=("${alive[@]}")
            [ ${#pids[@]} -lt "$JOBS" ] && break
            wait -n
        done
        n=$((n + 1))
        (
            # In a subshell of its own, so that a fatal error still records a status.
            ("$fn" "$name" "$project" "$dir" "$build" "$timeout" "$optional" "$envs") >"$logs/$n.out" 2>"$logs/$n.err"
            echo $? >"$logs/$n.rc"
            { flock 9; cat "$logs/$n.err" >&2; cat "$logs/$n.out"; } 9>"$logs/lock"
        ) &
        pids+=($!)
    done < <($PY "$HERE/corpus.py" configs "$MANIFEST" --only "$ONLY")
    wait
    trap - INT TERM
    for f in $(seq 1 $n); do
        [ "$(cat "$logs/$f.rc" 2>/dev/null)" = 0 ] || rc=1
    done
    rm -rf "$logs"
    return $rc
}

# Sends SIGTERM to a process and everything under it, stopping each first so that it starts
# nothing new meanwhile.
kill_tree() {
    local c
    kill -STOP "$1" 2>/dev/null || return 0
    for c in $(pgrep -P "$1"); do kill_tree "$c"; done
    kill -TERM "$1" 2>/dev/null
    kill -CONT "$1" 2>/dev/null
}

stop_jobs() {
    local p
    for p in $(jobs -p); do kill_tree "$p"; done
}

check_one() {
    local name="$1" project="$2" dir="$3" build="$4" timeout="$5" optional="$6" envs="$7"
    if [ ! -d "$dir" ]; then
        # No result rather than an older one; a configuration that is not optional fails.
        rm -f "$OUT/$name.json" "$OUT/$name.sweep.json"
        [ "$optional" = 1 ] && { log "$name: $dir missing, skipped"; return 0; }
        log "$name: $dir missing (run fetch)"; return 1
    fi
    engine_args "$dir" "$build" "$envs"
    rm -f "$OUT/$name.sweep.json"  # stale once the configuration is checked again
    run_engine "$timeout" "$OUT/$name.problems.txt" "$OUT/$name.stderr.txt" "${ARGS[@]}" --max 1000000
    $PY "$HERE/corpus.py" result --config "$name" --project "$project" --commit "$(commit_of "$dir")" \
        --stdout "$OUT/$name.problems.txt" --stderr "$OUT/$name.stderr.txt" --exit $RC --wall "$WALL" \
        $TIMED_OUT --output "$OUT/$name.json"
}

cmd_check() {
    need_bin
    mkdir -p "$OUT"
    local rc=0 slow only="$ONLY" jobs="$JOBS"
    each_config check_one || rc=1
    [ "$JOBS" -gt 1 ] || return $rc
    # Builds side by side slow each other down (each engine uses every CPU): configurations
    # whose build time compare would report are checked again one at a time.
    slow=$($PY "$HERE/corpus.py" slow "$OUT" --baseline "$BASELINE" --only "$ONLY") || return 1
    [ -n "$slow" ] || return $rc
    log "checking again one at a time for their build times: $slow"
    ONLY="$slow" JOBS=1
    each_config check_one || rc=1
    ONLY="$only" JOBS="$jobs"
    return $rc
}

# --- sweep ---------------------------------------------------------------------------------

# The queries of all configurations go to one pool of --jobs engine processes, in chunks of
# --chunk queries (see Sweep in corpus.py).
cmd_sweep() {
    need_bin
    mkdir -p "$OUT"
    local uvm=()
    [ "$SWEEP_UVM" = 1 ] && uvm=(--sweep-uvm)
    $PY "$HERE/corpus.py" sweep "$MANIFEST" --only "$ONLY" --bin "$BIN" --out "$OUT" --jobs "$JOBS" \
        --chunk "$CHUNK" --every "$EVERY" --cap "$CAP" --query-timeout "$QUERY_TIMEOUT" "${uvm[@]}"
}

# --- fuzz ----------------------------------------------------------------------------------

# `check --fuzz-edits`: random edits as editor contents (the clones are never written), each
# incremental update compared with a full build of the same texts.
fuzz_one() {
    local name="$1" project="$2" dir="$3" build="$4" timeout="$5" optional="$6" envs="$7" found=0 n line
    [ -d "$dir" ] || { log "$name: $dir missing, skipped"; return 0; }
    engine_args "$dir" "$build" "$envs"
    # Each edit costs a full build, so the time limit grows with the number of edits.
    run_engine $((timeout * (FUZZ_EDITS / 20 + 1))) "$OUT/$name.fuzz.txt" "$OUT/$name.fuzz.err" "${ARGS[@]}" \
        --fuzz-edits "$FUZZ_EDITS" --seed "$FUZZ_SEED" --fuzz-findings 3
    line=$(grep "dvicode check: fuzz seed" "$OUT/$name.fuzz.err" | sed 's/^dvicode check: //; s/ (.*//')
    n=$(grep -c "^reproducer" "$OUT/$name.fuzz.txt")
    if [ $RC -gt 1 ] || [ -n "$TIMED_OUT" ]; then
        log "$name: engine failed (exit $RC${TIMED_OUT:+, timed out}), see $OUT/$name.fuzz.err"
        found=1
    else
        log "$name: $line"
    fi
    [ "$n" -gt 0 ] && found=1
    return $found
}

cmd_fuzz() {
    need_bin
    mkdir -p "$OUT"
    each_config fuzz_one
}

# `check --write-check`: renames and quick fixes applied like the editor does (in memory: the
# clones are never written), each result compiled again and checked.
writes_one() {
    local name="$1" project="$2" dir="$3" build="$4" timeout="$5" optional="$6" envs="$7" found=0 line
    [ -d "$dir" ] || { log "$name: $dir missing, skipped"; return 0; }
    engine_args "$dir" "$build" "$envs"
    run_engine $((timeout * (RENAMES / 10 + 1))) "$OUT/$name.writes.txt" "$OUT/$name.writes.err" "${ARGS[@]}" \
        --write-check "$RENAMES" --seed "$FUZZ_SEED"
    line=$(grep "dvicode check: write check" "$OUT/$name.writes.err" | sed 's/^dvicode check: //')
    if [ $RC -gt 1 ] || [ -n "$TIMED_OUT" ]; then
        log "$name: engine failed (exit $RC${TIMED_OUT:+, timed out}), see $OUT/$name.writes.err"
        found=1
    else
        log "$name: $line"
    fi
    grep -q "^finding:" "$OUT/$name.writes.txt" && found=1
    return $found
}

cmd_writes() {
    need_bin
    mkdir -p "$OUT"
    each_config writes_one
}

case "$CMD" in
    fetch) cmd_fetch ;;
    build) cmd_build ;;
    check) cmd_check ;;
    sweep) cmd_sweep ;;
    fuzz) cmd_fuzz ;;
    writes) cmd_writes ;;
    compare) $PY "$HERE/corpus.py" compare "$OUT" --baseline "$BASELINE" --only "$ONLY" ;;
    baseline) $PY "$HERE/corpus.py" baseline "$OUT" --output "$BASELINE" --only "$ONLY" $MERGE ;;
    table) $PY "$HERE/corpus.py" table "$OUT" ;;
    all) cmd_fetch && need_bin && cmd_check && cmd_sweep &&
        $PY "$HERE/corpus.py" compare "$OUT" --baseline "$BASELINE" --only "$ONLY" ;;
    ""|-h|--help) usage ;;
    *) echo "run.sh: unknown command $CMD" >&2; usage >&2; exit 2 ;;
esac
