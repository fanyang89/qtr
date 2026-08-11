#!/usr/bin/env bash
set -Eeuo pipefail

readonly aml_tool=${1:?AML tool path is required}
readonly probe_tool=${2:?probe tool path is required}
readonly profile=${3:?CPU profile is required}
readonly expected_driver=${4:?expected idle driver is required}
readonly target_kernel=${5:?target kernel release is required}
readonly old_boot_id=${6:?old boot ID is required}
readonly native_vendor=${7:?native host vendor is required}

exec python3 "$probe_tool" \
    "$aml_tool" \
    "$profile" \
    "$expected_driver" \
    "$target_kernel" \
    "$old_boot_id" \
    "$native_vendor"
