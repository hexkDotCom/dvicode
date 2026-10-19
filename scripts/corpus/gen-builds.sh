#!/usr/bin/env bash
# Regenerates the build files that are derived from the projects' own FuseSoC cores and Bender
# manifests; run it after changing a pinned commit in manifest.json. Needs the corpus venv
# (run.sh fetch creates it) with FuseSoC: $CORPUS/.venv/bin/pip install fusesoc==2.4.5
# (the version OpenTitan's python-requirements.txt pins). The other build files in builds/ are
# written by hand from the projects' simulator file lists.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CORPUS="${CORPUS:-$HOME/.cache/dvicode-corpus}"
B="$HERE/builds"
V="$CORPUS/.venv/bin"

ot() {  # ot NAME CORE TOP [options]
    local name="$1" core="$2" top="$3"; shift 3
    PATH="$V:$PATH" "$V/python" "$HERE/fusesoc_build.py" --root "$CORPUS/opentitan" --core "$core" --top "$top" \
        --var CORPUS/opentitan --gen-var CORPUS/.gen/opentitan "$@" -o "$B/$name.build"
}
# As hw/dv/tools/dvsim/common_sim_cfg.hjson runs FuseSoC and the simulator (TL-UL: 32 bits).
MAP=(--flag=--mapping=lowrisc:prim_generic:all:0.1)
DV=("${MAP[@]}" --uvm --define UVM --define UVM_NO_DEPRECATED --define UVM_REGEX_NO_DPI
    --define UVM_REG_ADDR_WIDTH=32 --define UVM_REG_DATA_WIDTH=32 --define UVM_REG_BYTENABLE_WIDTH=4
    --define SIMULATION --define DUT_HIER=tb.dut)
ot opentitan-uart lowrisc:dv:uart_sim:0.1 tb "${DV[@]}"
ot opentitan-i2c lowrisc:dv:i2c_sim:0.1 tb "${DV[@]}"
# Build mode aes_masked of hw/ip/aes/dv/aes_base_sim_cfg.hjson.
ot opentitan-aes lowrisc:dv:aes_sim:0.1 tb "${DV[@]}" --define EN_MASKING=1 --define SBOX_IMPL=aes_pkg::SBoxImplDom \
    --define EN_GCM=0
ot opentitan-earlgrey-rtl lowrisc:systems:top_earlgrey:0.1 top_earlgrey "${MAP[@]}" --define SIMULATION

"$V/python" "$HERE/bender_flist.py" --base "$CORPUS" --var CORPUS \
    --pkg "tech_cells_generic=$CORPUS/tech_cells_generic" --pkg "common_cells=$CORPUS/common_cells" \
    --pkg "axi=$CORPUS/axi" -o "$B/pulp-axi-rtl.build"
