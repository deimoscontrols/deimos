#!/bin/bash

# Report the minimum DAQ cycle margin recorded by a timing-watermark test image.
#
# Flash with `--features timing-watermark`, run the desired workload, then call
# this script before resetting the board. The relaxed recorder is test-only and
# updates only when a cycle establishes a new minimum.

set -euo pipefail

readonly CHIP="STM32H743ZITx"
readonly DEFAULT_PROBE="0483:3754:0031003E3033510735393935"
readonly PROBE="${1:-${DEFAULT_PROBE}}"
readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly ELF="${SCRIPT_DIR}/deimos_daq_rev7/target/thumbv7em-none-eabihf/release/deimos_bare_metal"
readonly SYMBOLS=(
    "MIN_PUBLISHING_CYCLE_MARGIN_NS"
    "MIN_SAMPLE_ONLY_MARGIN_NS"
    "MIN_SAMPLE_COMM_MARGIN_NS"
)

if ! command -v probe-rs >/dev/null 2>&1 || ! command -v nm >/dev/null 2>&1; then
    echo "probe-rs and nm are required" >&2
    exit 127
fi
if [[ ! -f "${ELF}" ]]; then
    echo "Instrumented firmware ELF was not found" >&2
    exit 1
fi

for symbol in "${SYMBOLS[@]}"; do
    address="$(nm -n "${ELF}" | awk -v symbol="${symbol}" '$3 == symbol { print "0x" $1 }')"
    if [[ -z "${address}" ]]; then
        echo "${symbol} was not found; flash with --features timing-watermark first" >&2
        exit 1
    fi

    word="$(probe-rs read b32 "${address}" 1 --chip "${CHIP}" --probe "${PROBE}" | awk 'NR == 1 { print $1 }')"
    value=$((16#${word}))
    if ((value >= 2147483648)); then
        value=$((value - 4294967296))
    fi
    printf '%s=%d\n' "${symbol}" "${value}"
done
