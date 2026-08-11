#!/usr/bin/env bash
set -Eeuo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../.." && pwd)
readonly script_dir repo_root

: "${QTR_BASE_IMAGE:?set QTR_BASE_IMAGE to a prepared local guest image}"
: "${QTR_KERNEL_RPM:?set QTR_KERNEL_RPM to the local target kernel RPM}"
: "${QTR_EXPECTED_DRIVER:?set QTR_EXPECTED_DRIVER to acpi_idle or none}"
: "${QTR_CPU_PROFILE:?set QTR_CPU_PROFILE to hygon-baremetal, hygon-kvm, or native-baremetal}"

case $QTR_EXPECTED_DRIVER in
    acpi_idle | none) ;;
    *) printf 'invalid QTR_EXPECTED_DRIVER: %s\n' "$QTR_EXPECTED_DRIVER" >&2; exit 2 ;;
esac
case $QTR_CPU_PROFILE in
    hygon-baremetal | hygon-kvm | native-baremetal) ;;
    *) printf 'invalid QTR_CPU_PROFILE: %s\n' "$QTR_CPU_PROFILE" >&2; exit 2 ;;
esac
[[ -r $QTR_BASE_IMAGE && -f $QTR_BASE_IMAGE ]] || {
    printf 'QTR_BASE_IMAGE is not a readable regular file: %s\n' "$QTR_BASE_IMAGE" >&2
    exit 2
}
[[ -r $QTR_KERNEL_RPM && -f $QTR_KERNEL_RPM ]] || {
    printf 'QTR_KERNEL_RPM is not a readable regular file: %s\n' "$QTR_KERNEL_RPM" >&2
    exit 2
}

base_image=$(realpath -- "$QTR_BASE_IMAGE")
kernel_rpm=$(realpath -- "$QTR_KERNEL_RPM")
qtr_bin=${QTR_BIN:-$repo_root/target/debug/qtr}
[[ -x $qtr_bin ]] || {
    printf 'qtr binary is not executable: %s (run cargo build first)\n' "$qtr_bin" >&2
    exit 2
}
qtr_bin=$(realpath -- "$qtr_bin")
readonly base_image kernel_rpm qtr_bin

run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$-$RANDOM
readonly run_id
readonly vm_name=qtr-acpi-${QTR_CPU_PROFILE}-${run_id}
readonly work_dir=/var/tmp/$vm_name
readonly overlay_path=$work_dir/root-overlay.qcow2
readonly serial_log=$work_dir/serial.log
artifact_dir=${QTR_ARTIFACT_DIR:-$repo_root/.tmp/artifacts/kernel-acpi-idle/$run_id-$QTR_CPU_PROFILE-$QTR_EXPECTED_DRIVER}
mkdir -p -- "$artifact_dir"
artifact_dir=$(realpath -- "$artifact_dir")
readonly artifact_dir
[[ ! -e $artifact_dir/run.log ]] || {
    printf 'artifact directory already contains run.log: %s\n' "$artifact_dir" >&2
    exit 2
}

exec > >(tee "$artifact_dir/run.log") 2>&1

vm_owned=0
cleanup() {
    local status=$?
    trap - EXIT
    set +e

    if (( vm_owned )); then
        "$qtr_bin" vm dump "$vm_name" --xml >"$artifact_dir/domain-final.xml" 2>&1
        virsh --connect qemu:///system domstate "$vm_name" >"$artifact_dir/domain-state-final.txt" 2>&1
        virsh --connect qemu:///system dumpxml "$vm_name" >"$artifact_dir/domain-live-final.xml" 2>&1
        if (( status != 0 )); then
            "$qtr_bin" vm exec "$vm_name" --timeout-secs 30 -- sh -c \
                'printf "boot_id="; cat /proc/sys/kernel/random/boot_id; uname -a; cat /proc/cpuinfo; printf "current_driver="; cat /sys/devices/system/cpu/cpuidle/current_driver 2>&1 || true' \
                >"$artifact_dir/failure-guest.txt" 2>&1
            "$qtr_bin" vm exec "$vm_name" --timeout-secs 60 -- dmesg >"$artifact_dir/dmesg-failure.txt" 2>&1
            "$qtr_bin" vm exec "$vm_name" --timeout-secs 60 -- journalctl -b --no-pager -n 300 \
                >"$artifact_dir/journal-failure.txt" 2>&1
        fi
        "$qtr_bin" vm stop "$vm_name" --force --wait --shutdown-timeout-secs 30 \
            >"$artifact_dir/cleanup-stop.txt" 2>&1
        "$qtr_bin" vm rm "$vm_name" >"$artifact_dir/cleanup-rm.txt" 2>&1
    fi
    if [[ -f $serial_log ]]; then
        if cp -a -- "$serial_log" "$artifact_dir/serial.log" 2>/dev/null; then
            rm -f -- "$serial_log"
        elif (( EUID != 0 )) && sudo -n cat -- "$serial_log" |
            tee "$artifact_dir/serial.log" >/dev/null &&
            sudo -n rm -f -- "$serial_log"; then
            :
        else
            printf 'failed to archive serial log; retained at %s\n' "$serial_log" >&2
            (( status == 0 )) && status=1
        fi
    fi
    rm -f -- "$overlay_path"
    rmdir -- "$work_dir" 2>/dev/null
    printf 'artifacts: %s\n' "$artifact_dir"
    exit "$status"
}
trap cleanup EXIT

mkdir -p -- "$work_dir"
: >"$serial_log"
chmod 0644 "$serial_log"
printf 'vm_name=%s\nprofile=%s\nexpected_driver=%s\nbase_image=%s\nkernel_rpm=%s\nqtr_bin=%s\n' \
    "$vm_name" "$QTR_CPU_PROFILE" "$QTR_EXPECTED_DRIVER" "$base_image" "$kernel_rpm" "$qtr_bin" \
    >"$artifact_dir/run-manifest.txt"
stat --format='device=%d\ninode=%i\nsize=%s\nmtime=%Y\nmode=%a\n' "$base_image" \
    >"$artifact_dir/base-image-stat.txt"

"$qtr_bin" vm capabilities --machine pc-i440fx-10.2 --json \
    >"$artifact_dir/capabilities.json"
"$qtr_bin" disk info --path "$base_image" >"$artifact_dir/base-image-info.txt"
if ! sudo -n -u qemu test -r "$base_image"; then
    printf 'base image must already be readable by the qemu user: %s\n' "$base_image" >&2
    exit 1
fi
base_format=
while IFS= read -r line; do
    if [[ $line == 'format: '* ]]; then
        base_format=${line#format: }
    fi
done <"$artifact_dir/base-image-info.txt"
case $base_format in
    qcow2 | raw) ;;
    *) printf 'unsupported base image format: %s\n' "$base_format" >&2; exit 1 ;;
esac
"$qtr_bin" disk overlay --path "$overlay_path" --backing-file "$base_image" --backing-format "$base_format"

case $QTR_CPU_PROFILE in
    hygon-baremetal)
        cpu_config='cpu:
  mode: custom
  model: Dhyana
  vcpus: 2
  features:
    hypervisor: disable
    rdseed: disable'
        ;;
    hygon-kvm)
        cpu_config='cpu:
  mode: custom
  model: Dhyana
  vcpus: 2
  features:
    rdseed: disable'
        ;;
    native-baremetal)
        cpu_config='cpu:
  mode: host-passthrough
  vcpus: 2
  features:
    hypervisor: disable'
        ;;
esac
manifest=$(<"$script_dir/vm.yaml.in")
manifest=${manifest//@VM_NAME@/$vm_name}
manifest=${manifest//@CPU_CONFIG@/$cpu_config}
manifest=${manifest//@OVERLAY_PATH@/$overlay_path}
manifest=${manifest//@SERIAL_LOG@/$serial_log}
printf '%s\n' "$manifest" >"$artifact_dir/vm.yaml"

if "$qtr_bin" vm dump "$vm_name" --xml >/dev/null 2>&1; then
    printf 'refusing to use an existing domain: %s\n' "$vm_name" >&2
    exit 1
fi
if (( EUID == 0 )); then
    "$qtr_bin" host fix-vm-perms --file "$artifact_dir/vm.yaml"
else
    sudo -n "$qtr_bin" host fix-vm-perms --file "$artifact_dir/vm.yaml"
fi
vm_owned=1
"$qtr_bin" vm apply --file "$artifact_dir/vm.yaml"
"$qtr_bin" vm dump "$vm_name" --xml >"$artifact_dir/domain.xml"
"$qtr_bin" vm start "$vm_name"
virsh --connect qemu:///system dumpxml "$vm_name" >"$artifact_dir/domain-active.xml"

readonly guest_dir=/var/tmp/qtr-kernel-acpi-idle
"$qtr_bin" vm cp "$vm_name" "$script_dir/aml.py" "guest:$guest_dir/aml.py" --parents --timeout-secs 300
"$qtr_bin" vm cp "$vm_name" "$script_dir/prepare-guest.sh" "guest:$guest_dir/prepare-guest.sh" --parents --timeout-secs 300
"$qtr_bin" vm cp "$vm_name" "$script_dir/probe-guest.py" "guest:$guest_dir/probe-guest.py" --parents --timeout-secs 300
"$qtr_bin" vm cp "$vm_name" "$script_dir/assert-guest.sh" "guest:$guest_dir/assert-guest.sh" --parents --timeout-secs 300
"$qtr_bin" vm cp "$vm_name" "$kernel_rpm" "guest:$guest_dir/kernel.rpm" --parents --timeout-secs 600

old_boot_id=$("$qtr_bin" vm exec "$vm_name" --timeout-secs 300 -- cat /proc/sys/kernel/random/boot_id)
old_boot_id=${old_boot_id//$'\n'/}
readonly old_boot_id
printf '%s\n' "$old_boot_id" >"$artifact_dir/boot-id-before.txt"
"$qtr_bin" vm exec "$vm_name" --timeout-secs 1800 --output "$artifact_dir/guest-prepare.json" -- \
    /bin/bash "$guest_dir/prepare-guest.sh" "$guest_dir/kernel.rpm" "$guest_dir/aml.py"
"$qtr_bin" vm cp "$vm_name" guest:/var/lib/qtr-kernel-acpi-idle/kernel-release \
    "$artifact_dir/target-kernel-release.txt" --timeout-secs 300
"$qtr_bin" vm cp "$vm_name" guest:/var/lib/qtr-kernel-acpi-idle/acpi-expected.json \
    "$artifact_dir/acpi-expected.json" --timeout-secs 300
"$qtr_bin" vm cp "$vm_name" guest:/var/lib/qtr-kernel-acpi-idle/DSDT \
    "$artifact_dir/DSDT.aml" --timeout-secs 300
IFS= read -r target_kernel <"$artifact_dir/target-kernel-release.txt"
[[ -n $target_kernel ]] || {
    printf 'guest returned an empty target kernel release\n' >&2
    exit 1
}
readonly target_kernel

native_vendor=
while IFS= read -r line; do
    if [[ $line == vendor_id*:* ]]; then
        native_vendor=${line#*:}
        native_vendor=${native_vendor#"${native_vendor%%[![:space:]]*}"}
        native_vendor=${native_vendor%"${native_vendor##*[![:space:]]}"}
        break
    fi
done </proc/cpuinfo
[[ -n $native_vendor ]] || {
    printf 'could not determine host CPU vendor_id\n' >&2
    exit 1
}
readonly native_vendor

"$qtr_bin" vm reboot "$vm_name"
new_boot_id=
readonly reboot_deadline=$((SECONDS + 300))
while (( SECONDS < reboot_deadline )); do
    if candidate=$("$qtr_bin" vm exec "$vm_name" --timeout-secs 10 -- cat /proc/sys/kernel/random/boot_id \
        2>>"$artifact_dir/reboot-wait.log"); then
        candidate=${candidate//$'\n'/}
        if [[ -n $candidate && $candidate != "$old_boot_id" ]]; then
            new_boot_id=$candidate
            break
        fi
    fi
    sleep 2
done
[[ -n $new_boot_id ]] || {
    printf 'timed out waiting for a changed boot_id after reboot\n' >&2
    exit 1
}
printf '%s\n' "$new_boot_id" >"$artifact_dir/boot-id-after.txt"

"$qtr_bin" vm exec "$vm_name" --timeout-secs 300 -- \
    /bin/bash "$guest_dir/assert-guest.sh" \
    "$guest_dir/aml.py" "$guest_dir/probe-guest.py" "$QTR_CPU_PROFILE" "$QTR_EXPECTED_DRIVER" \
    "$target_kernel" "$old_boot_id" "$native_vendor" \
    >"$artifact_dir/guest-probe.json"
"$qtr_bin" vm exec "$vm_name" --timeout-secs 300 -- dmesg >"$artifact_dir/dmesg.txt"
stat --format='device=%d\ninode=%i\nsize=%s\nmtime=%Y\nmode=%a\n' "$base_image" \
    >"$artifact_dir/base-image-stat-after.txt"
cmp --silent "$artifact_dir/base-image-stat.txt" "$artifact_dir/base-image-stat-after.txt" || {
    printf 'base image metadata changed during the test\n' >&2
    exit 1
}

printf 'kernel ACPI idle E2E passed: profile=%s expected_driver=%s kernel=%s\n' \
    "$QTR_CPU_PROFILE" "$QTR_EXPECTED_DRIVER" "$target_kernel"
