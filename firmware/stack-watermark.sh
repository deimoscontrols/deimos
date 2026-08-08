#!/bin/bash

# Read a stack-watermark test image's painted DTCM stack and report observed MSP use.
#
# First flash firmware built with `--features stack-watermark`, run the desired
# on-target workload, then invoke this script while the probe remains attached.
# The startup painter writes from the linker's _stack_end through the
# then-current MSP, so the first non-painted word gives the maximum downward
# excursion since startup.

set -euo pipefail

readonly CHIP="STM32H743ZITx"
readonly DEFAULT_PROBE="0483:3754:0031003E3033510735393935"
readonly PROBE="${1:-${DEFAULT_PROBE}}"
readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly ELF="${SCRIPT_DIR}/deimos_daq_rev7/target/thumbv7em-none-eabihf/release/deimos_bare_metal"
readonly PAINT="cccccccc"

if ! command -v probe-rs >/dev/null 2>&1; then
    echo "probe-rs is required but was not found on PATH" >&2
    exit 127
fi

if ! command -v nm >/dev/null 2>&1 || [[ ! -f "${ELF}" ]]; then
    echo "Instrumented firmware ELF or nm was not found" >&2
    exit 1
fi

readonly STACK_START="$(nm -n "${ELF}" | awk '$3 == "_stack_start" { print "0x" $1 }')"
readonly STACK_END="$(nm -n "${ELF}" | awk '$3 == "_stack_end" { print "0x" $1 }')"
if [[ -z "${STACK_START}" || -z "${STACK_END}" ]]; then
    echo "Instrumented firmware ELF has no _stack_start or _stack_end symbol" >&2
    exit 1
fi
readonly STACK_BYTES=$((STACK_START - STACK_END))
readonly STACK_WORDS=$((STACK_BYTES / 4))

probe-rs read b32 "${STACK_END}" "${STACK_WORDS}" \
    --chip "${CHIP}" \
    --probe "${PROBE}" \
    | awk -v stack_bytes="${STACK_BYTES}" -v paint="${PAINT}" '
        BEGIN {
            scanning = 1
            painted_bytes = 0
        }
        {
            for (field = 1; field <= NF; field += 1) {
                if (scanning && tolower($field) == paint) {
                    painted_bytes += 4
                } else {
                    scanning = 0
                }
            }
        }
        END {
            if (painted_bytes == 0) {
                print "No painted stack prefix found; flash with --features stack-watermark first." > "/dev/stderr"
                exit 1
            }
            printf "stack_reserved_bytes=%d\n", stack_bytes
            printf "stack_high_water_bytes=%d\n", stack_bytes - painted_bytes
            printf "stack_minimum_free_bytes=%d\n", painted_bytes
        }
    '
