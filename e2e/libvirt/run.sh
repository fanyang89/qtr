#!/usr/bin/env bash
set -euo pipefail

readonly artifact_dir=/artifacts
readonly work_dir=/var/lib/qtr-e2e
readonly vm_name=qtr-e2e
readonly qtr=/workspace/target/release/qtr
readonly manifest=/opt/qtr-e2e/vm.yaml

mkdir -p "$artifact_dir" "$work_dir"
exec > >(tee "$artifact_dir/e2e.log") 2>&1

cleanup() {
    local status=$?
    trap - EXIT
    set +e

    virsh --connect qemu:///system dumpxml "$vm_name" >"$artifact_dir/domain-final.xml" 2>&1
    virsh --connect qemu:///system destroy "$vm_name" >/dev/null 2>&1
    virsh --connect qemu:///system managedsave-remove "$vm_name" >/dev/null 2>&1
    virsh --connect qemu:///system undefine "$vm_name" >/dev/null 2>&1

    mkdir -p "$artifact_dir/libvirt"
    cp -a /var/log/libvirt/. "$artifact_dir/libvirt/" 2>/dev/null
    cp -a "$work_dir/serial.log" "$artifact_dir/" 2>/dev/null

    if [[ -n ${QTR_E2E_ARTIFACT_UID:-} && -n ${QTR_E2E_ARTIFACT_GID:-} ]]; then
        chown -R "$QTR_E2E_ARTIFACT_UID:$QTR_E2E_ARTIFACT_GID" "$artifact_dir"
    fi

    exit "$status"
}
trap cleanup EXIT

assert_state() {
    local expected=$1
    local actual
    actual=$(virsh --connect qemu:///system domstate "$vm_name")
    if [[ $actual != "$expected" ]]; then
        printf 'expected domain state %q, got %q\n' "$expected" "$actual" >&2
        return 1
    fi
}

test -c /dev/kvm
test -r /dev/kvm
test -w /dev/kvm

virtlogd --daemon
virtlockd --daemon
libvirtd --daemon

for _ in {1..30}; do
    if virsh --connect qemu:///system uri >"$artifact_dir/libvirt-uri.txt" 2>&1; then
        break
    fi
    sleep 1
done
virsh --connect qemu:///system uri

"$qtr" vm capabilities --machine q35 --json >"$artifact_dir/capabilities.json"
"$qtr" disk create --path "$work_dir/root.qcow2" --format qcow2 --size 64M
"$qtr" disk info --path "$work_dir/root.qcow2" >"$artifact_dir/disk-before.txt"
"$qtr" host fix-vm-perms --file "$manifest" --qemu-user qemu

"$qtr" vm apply --file "$manifest"
"$qtr" vm list >"$artifact_dir/vm-list.txt"
"$qtr" vm dump "$vm_name" --output "$artifact_dir/domain.yaml"
"$qtr" vm dump "$vm_name" --xml >"$artifact_dir/domain.xml"

"$qtr" vm start "$vm_name"
assert_state running

"$qtr" vm suspend "$vm_name"
assert_state paused
"$qtr" vm resume "$vm_name"
assert_state running

"$qtr" vm save "$vm_name"
test "$("$qtr" vm saved-state "$vm_name")" = present
"$qtr" vm restore "$vm_name"
assert_state running

"$qtr" vm stop "$vm_name" --force --wait --shutdown-timeout-secs 30
assert_state "shut off"
"$qtr" vm disk-resize "$vm_name" root 128MiB
"$qtr" disk info --path "$work_dir/root.qcow2" >"$artifact_dir/disk-after.txt"

"$qtr" vm rm "$vm_name"
if virsh --connect qemu:///system dominfo "$vm_name" >/dev/null 2>&1; then
    printf 'domain still exists after qtr vm rm: %s\n' "$vm_name" >&2
    exit 1
fi

printf 'qtr libvirt lifecycle E2E passed\n'
