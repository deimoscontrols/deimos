#!/bin/bash

# Print the fixed stack-frame sizes from an optimized firmware build.
#
# Install the pinned compiler and target with:
#   rustup toolchain install nightly-2026-07-29 --profile minimal \
#       --target thumbv7em-none-eabihf

set -euo pipefail

readonly TOOLCHAIN="nightly-2026-07-29"
readonly TARGET="thumbv7em-none-eabihf"
readonly BINARY="deimos_bare_metal"
readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly FIRMWARE_DIR="${SCRIPT_DIR}/deimos_daq_rev7"
readonly BUILD_DIR="${FIRMWARE_DIR}/target/stack-sizes-build"
readonly OBJECT="${BUILD_DIR}/${BINARY}.o"
readonly REPORT="${FIRMWARE_DIR}/target/stack-sizes.txt"

if ! command -v llvm-readobj >/dev/null 2>&1; then
    echo "llvm-readobj is required but was not found on PATH" >&2
    exit 127
fi

if ! rustup run "${TOOLCHAIN}" rustc --version >/dev/null 2>&1; then
    echo "${TOOLCHAIN} is not installed; see the installation command in $0" >&2
    exit 1
fi

if ! rustup target list --toolchain "${TOOLCHAIN}" --installed \
    | grep -qx "${TARGET}"; then
    echo "${TARGET} is not installed for ${TOOLCHAIN}; see the installation command in $0" >&2
    exit 1
fi

mkdir -p -- "${BUILD_DIR}"
cd -- "${FIRMWARE_DIR}"

# An explicit object output preserves the .stack_sizes section, which the
# linker normally discards from the flashable ELF. The separate target
# directory keeps this diagnostic build away from the image used for flashing.
CARGO_TARGET_DIR="${BUILD_DIR}" cargo "+${TOOLCHAIN}" rustc \
    --release \
    --target "${TARGET}" \
    --bin "${BINARY}" \
    -- \
    -Zemit-stack-sizes \
    --emit="obj=${OBJECT}"

readonly TEMP_REPORT="$(mktemp "${REPORT}.tmp.XXXXXX")"
trap 'rm -f -- "${TEMP_REPORT}"' EXIT

{
    echo "Fixed stack-frame sizes for firmware"
    echo "Compiler: $(rustup run "${TOOLCHAIN}" rustc --version)"
    echo "Profile: release (fat LTO)"
    echo
    printf '%10s  %s\n' "Bytes" "Function"
    printf '%10s  %s\n' "----------" "--------"
    llvm-readobj --demangle --stack-sizes "${OBJECT}" \
        | perl -ne '
            if (/^\s+Functions: \[(.*)\]\s*$/) {
                $function = $1;
            } elsif (/^\s+Size: 0x([[:xdigit:]]+)\s*$/ && defined $function) {
                printf "%10d  %s\n", hex($1), $function;
                undef $function;
            }
        ' \
        | LC_ALL=C sort -k1,1nr -k2,2
    echo
    echo "These are fixed per-function frames, not cumulative call-chain sizes."
    echo "Hardware exception frames and unresolved indirect callees are not included."
} | tee "${TEMP_REPORT}"

mv -- "${TEMP_REPORT}" "${REPORT}"
trap - EXIT
echo "Stack-size report written to ${REPORT}"
