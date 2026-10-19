#!/usr/bin/env bash
# Real-world regression harness for the DVICode engine (runs in Linux or WSL).
# See scripts/corpus/README.md. Usage: scripts/corpus/run.sh COMMAND [OPTIONS]
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
export REPO
export CORPUS="${CORPUS:-$HOME/.cache/dvicode-corpus}"
export UVM_HOME="${UVM_HOME:-$HOME/corpus/uvm-1.2}"
MANIFEST="$HERE/manifest.json"
BASELINE="$HERE/baseline.json"
OUT="${OUT:-$CORPUS/results}"
BIN="${DVICODE_BIN:-}"
ONLY=""
EVERY=20
CAP=30
QUERY_TIMEOUT=60
SWEEP_UVM=0
FUZZ_EDITS=60
FUZZ_SEED=1
RENAMES=40
PY=python3

usage() {
    cat <<'EOF'
Usage: scripts/corpus/run.sh COMMAND [OPTIONS]

Commands:
  fetch      clone the projects (or move them) to the pinned commits, generate sources
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
  --bin PATH         engine binary (default: build it; or $DVICODE_BIN)
  --only REGEX       only configurations whose name matches
  --out DIR          results directory (default $CORPUS/results)
  --every N          sweep: query identifiers on every Nth line (default 20)
  --cap N            sweep: at most N positions per file (default 30)
  --query-timeout S  sweep: one query may take S seconds (default 60)
  --sweep-uvm        sweep: also query the UVM library's files
  --edits N          fuzz: edits per configuration (default 60)
  --seed S           fuzz, writes: random seed (default 1)
  --renames N        writes: renames per configuration (default 40)
  --merge            baseline: keep configurations that have no result in OUT

Environment: CORPUS (clones, default ~/.cache/dvicode-corpus), UVM_HOME (default
~/corpus/uvm-1.2), CARGO_TARGET_DIR (for build).
EOF
}

CMD="${1:-}"
[ -n "$CMD" ] && shift
MERGE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --bin) BIN="$2"; shift ;;
        --only) ONLY="$2"; shift ;;
        --out) OUT="$2"; shift ;;
        --every) EVERY="$2"; shift ;;
        --cap) CAP="$2"; shift ;;
        --query-timeout) QUERY_TIMEOUT="$2"; shift ;;
        --sweep-uvm) SWEEP_UVM=1 ;;
        --edits) FUZZ_EDITS="$2"; shift ;;
        --seed) FUZZ_SEED="$2"; shift ;;
        --renames) RENAMES="$2"; shift ;;
        --merge) MERGE=--merge ;;
        -h|--help) usage; exit 0 ;;
        *) echo "run.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

log() { echo "[corpus] $*" >&2; }

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
    for e in $envs; do ARGS+=(--env "$e"); done
}

cmd_check() {
    need_bin
    mkdir -p "$OUT"
    local name project dir build timeout optional envs
    while IFS=$'\t' read -r name project dir build timeout optional envs; do
        if [ ! -d "$dir" ]; then
            [ "$optional" = 1 ] && { log "$name: $dir missing, skipped"; continue; }
            log "$name: $dir missing (run fetch)"; continue
        fi
        engine_args "$dir" "$build" "$envs"
        rm -f "$OUT/$name.sweep.json"  # stale once the configuration is checked again
        run_engine "$timeout" "$OUT/$name.problems.txt" "$OUT/$name.stderr.txt" "${ARGS[@]}" --max 1000000
        $PY "$HERE/corpus.py" result --config "$name" --project "$project" --commit "$(commit_of "$dir")" \
            --stdout "$OUT/$name.problems.txt" --stderr "$OUT/$name.stderr.txt" --exit $RC --wall "$WALL" \
            $TIMED_OUT --output "$OUT/$name.json"
    done < <($PY "$HERE/corpus.py" configs "$MANIFEST" --only "$ONLY")
}

# --- sweep ---------------------------------------------------------------------------------

cmd_sweep() {
    need_bin
    mkdir -p "$OUT"
    local name project dir build timeout optional envs work total done skip t0
    while IFS=$'\t' read -r name project dir build timeout optional envs; do
        [ -d "$dir" ] || { log "$name: $dir missing, skipped"; continue; }
        engine_args "$dir" "$build" "$envs"
        work="$OUT/$name.sweep"
        rm -rf "$work" && mkdir -p "$work"
        t0=$(date +%s)
        run_engine "$timeout" "$work/files.txt" "$work/files.err" "${ARGS[@]}" -q --files -
        skip=()
        [ "$SWEEP_UVM" = 1 ] || skip=(--skip "$(cd "$UVM_HOME" 2>/dev/null && pwd -P)")
        $PY "$HERE/corpus.py" queries --files "$work/files.txt" --output "$work/all.q" --every "$EVERY" --cap "$CAP" "${skip[@]}"
        total=$(wc -l <"$work/all.q")
        done=0
        local part=0
        while [ "$done" -lt "$total" ] && [ $part -lt 50 ]; do
            part=$((part + 1))
            tail -n +$((done + 1)) "$work/all.q" >"$work/part.q"
            # The whole sweep of one configuration gets at most 4 hours.
            run_engine 14400 "$work/out.txt" "$work/err.txt" "${ARGS[@]}" -q \
                --queries "$work/part.q" --query-timeout "$QUERY_TIMEOUT"
            cat "$work/err.txt" >>"$work/stderr.txt"
            local before=$done
            done=$($PY "$HERE/corpus.py" sweep-part --state "$work/state.json" --stdout "$work/out.txt" \
                --stderr "$work/err.txt" --exit $RC $TIMED_OUT)
            [ "$RC" -le 1 ] && [ -z "$TIMED_OUT" ] && break
            grep -q '"build_failed"' "$work/state.json" && break
            [ "$done" -gt "$before" ] || break
            log "$name: restarting the sweep after query $done of $total"
        done
        $PY "$HERE/corpus.py" sweep-result --state "$work/state.json" --config "$name" --total "$total" \
            --wall $(($(date +%s) - t0)) --output "$OUT/$name.sweep.json"
    done < <($PY "$HERE/corpus.py" configs "$MANIFEST" --only "$ONLY")
}

# --- fuzz ----------------------------------------------------------------------------------

# `check --fuzz-edits`: random edits as editor contents (the clones are never written), each
# incremental update compared with a full build of the same texts.
cmd_fuzz() {
    need_bin
    mkdir -p "$OUT"
    local name project dir build timeout optional envs found=0 n line
    while IFS=$'\t' read -r name project dir build timeout optional envs; do
        [ -d "$dir" ] || { log "$name: $dir missing, skipped"; continue; }
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
    done < <($PY "$HERE/corpus.py" configs "$MANIFEST" --only "$ONLY")
    return $found
}

# `check --write-check`: renames and quick fixes applied like the editor does (in memory: the
# clones are never written), each result compiled again and checked.
cmd_writes() {
    need_bin
    mkdir -p "$OUT"
    local name project dir build timeout optional envs found=0 line
    while IFS=$'\t' read -r name project dir build timeout optional envs; do
        [ -d "$dir" ] || { log "$name: $dir missing, skipped"; continue; }
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
    done < <($PY "$HERE/corpus.py" configs "$MANIFEST" --only "$ONLY")
    return $found
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
