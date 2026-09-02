#!/usr/bin/env bash
set -euo pipefail

readonly artifact_dir=/artifacts
readonly work_dir=/var/lib/qtr-e2e
readonly vm_name=qtr-e2e
readonly qtr=/workspace/target/release/qtr
readonly manifest=/opt/qtr-e2e/vm.yaml
readonly api_address=127.0.0.1:18080
readonly api_url="http://$api_address/api/v1"
readonly api_token=qtr-e2e-token
readonly base_image_id=jepsen-base.qcow2
readonly clone_image_id=jepsen-node-1.qcow2
readonly seed_id=jepsen-node-1-seed.iso
readonly api_state_dir="$work_dir/server"
readonly api_image_root="$work_dir/images"
readonly api_media_root="$work_dir/media"
readonly api_log_root="$work_dir/logs"

web_pid=

mkdir -p \
    "$artifact_dir" \
    "$work_dir" \
    "$api_state_dir" \
    "$api_image_root" \
    "$api_media_root" \
    "$api_log_root" \
    "$work_dir/web"
exec > >(tee "$artifact_dir/e2e.log") 2>&1

api_request() {
    curl \
        --silent \
        --show-error \
        --fail-with-body \
        --header "Authorization: Bearer $api_token" \
        "$@"
}

cleanup() {
    local status=$?
    trap - EXIT
    set +e

    if [[ -n $web_pid ]]; then
        if kill -0 "$web_pid" >/dev/null 2>&1; then
            api_request --request DELETE "$api_url/images/$clone_image_id" >/dev/null 2>&1
            api_request --request DELETE "$api_url/images/$base_image_id" >/dev/null 2>&1
            api_request --request DELETE "$api_url/media/$seed_id" >/dev/null 2>&1
            kill "$web_pid" >/dev/null 2>&1
        fi
        wait "$web_pid" >/dev/null 2>&1
    fi
    rm -f \
        "$api_image_root/$clone_image_id" \
        "$api_image_root/$base_image_id" \
        "$api_media_root/$seed_id"

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

wait_for_web() {
    local attempt
    for attempt in {1..30}; do
        if curl --silent --show-error --fail "$api_url/health" \
            >"$artifact_dir/api-health.json"; then
            return 0
        fi
        if ! kill -0 "$web_pid" >/dev/null 2>&1; then
            wait "$web_pid" || true
            printf 'qtr web exited before becoming ready\n' >&2
            tail -n 100 "$artifact_dir/qtr-web.log" >&2 || true
            return 1
        fi
        sleep 1
    done

    printf 'timed out waiting for qtr web at %s\n' "$api_url" >&2
    tail -n 100 "$artifact_dir/qtr-web.log" >&2 || true
    return 1
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

"$qtr" web \
    --listen "$api_address" \
    --connect-uri qemu:///system \
    --web-dir "$work_dir/web" \
    --api-token "$api_token" \
    --state-dir "$api_state_dir" \
    --image-root "$api_image_root" \
    --media-root "$api_media_root" \
    --log-root "$api_log_root" \
    >"$artifact_dir/qtr-web.log" 2>&1 &
web_pid=$!
wait_for_web
jq -e '.ok == true and .libvirtUri == "qemu:///system"' "$artifact_dir/api-health.json" \
    >/dev/null

unauthorized_status=$(curl \
    --silent \
    --show-error \
    --output "$artifact_dir/api-unauthorized.json" \
    --write-out '%{http_code}' \
    "$api_url/vms")
test "$unauthorized_status" = 401
jq -e '.status == 401' "$artifact_dir/api-unauthorized.json" >/dev/null

"$qtr" disk create \
    --path "$work_dir/import-base.qcow2" \
    --format qcow2 \
    --size 16M
api_request \
    --request PUT \
    --header 'Content-Type: application/octet-stream' \
    --data-binary "@$work_dir/import-base.qcow2" \
    "$api_url/images/$base_image_id" \
    >"$artifact_dir/api-import-image.json"
jq -e \
    --arg id "$base_image_id" \
    '.id == $id and .format == "qcow2" and .status == "ready" and .backingImageId == null' \
    "$artifact_dir/api-import-image.json" >/dev/null

api_request \
    --request POST \
    --header 'Content-Type: application/json' \
    --data "{\"id\":\"$clone_image_id\"}" \
    "$api_url/images/$base_image_id/clone" \
    >"$artifact_dir/api-clone-image.json"
jq -e \
    --arg id "$clone_image_id" \
    --arg backing "$base_image_id" \
    '.id == $id and .format == "qcow2" and .status == "ready" and .backingImageId == $backing' \
    "$artifact_dir/api-clone-image.json" >/dev/null
qemu-img info --output=json "$api_image_root/$clone_image_id" \
    >"$artifact_dir/clone-image-info.json"
jq -e \
    --arg backing "$api_image_root/$base_image_id" \
    '.format == "qcow2" and .["backing-filename"] == $backing and .["backing-filename-format"] == "qcow2"' \
    "$artifact_dir/clone-image-info.json" >/dev/null

base_delete_status=$(curl \
    --silent \
    --show-error \
    --request DELETE \
    --header "Authorization: Bearer $api_token" \
    --output "$artifact_dir/api-delete-backing-conflict.json" \
    --write-out '%{http_code}' \
    "$api_url/images/$base_image_id")
test "$base_delete_status" = 409
jq -e '.status == 409' "$artifact_dir/api-delete-backing-conflict.json" >/dev/null

cat >"$work_dir/expected-user-data" <<'EOF'
#cloud-config
users:
  - name: qtr
    sudo: ALL=(ALL) NOPASSWD:ALL
EOF
jq -n \
    --arg id "$seed_id" \
    --arg instanceId jepsen-node-1 \
    --arg localHostname n1 \
    --rawfile userData "$work_dir/expected-user-data" \
    '{id: $id, instanceId: $instanceId, localHostname: $localHostname, userData: $userData}' \
    >"$work_dir/cloud-init-request.json"
api_request \
    --request POST \
    --header 'Content-Type: application/json' \
    --data-binary "@$work_dir/cloud-init-request.json" \
    "$api_url/media/cloud-init" \
    >"$artifact_dir/api-cloud-init-seed.json"
jq -e \
    --arg id "$seed_id" \
    '.id == $id and .status == "ready"' \
    "$artifact_dir/api-cloud-init-seed.json" >/dev/null

isoinfo -d -i "$api_media_root/$seed_id" >"$artifact_dir/cloud-init-volume.txt"
isoinfo -R -f -i "$api_media_root/$seed_id" >"$artifact_dir/cloud-init-files.txt"
isoinfo -R -x /user-data -i "$api_media_root/$seed_id" \
    >"$artifact_dir/cloud-init-user-data"
isoinfo -R -x /meta-data -i "$api_media_root/$seed_id" \
    >"$artifact_dir/cloud-init-meta-data"
awk -F ': *' '/Volume id:/{print toupper($2)}' "$artifact_dir/cloud-init-volume.txt" \
    | grep -Fxq CIDATA
grep -Fxq /user-data "$artifact_dir/cloud-init-files.txt"
grep -Fxq /meta-data "$artifact_dir/cloud-init-files.txt"
cmp "$work_dir/expected-user-data" "$artifact_dir/cloud-init-user-data"
grep -Fxq 'instance-id: jepsen-node-1' "$artifact_dir/cloud-init-meta-data"
grep -Fxq 'local-hostname: n1' "$artifact_dir/cloud-init-meta-data"

"$qtr" vm capabilities --machine q35 --json >"$artifact_dir/capabilities.json"
"$qtr" disk create --path "$work_dir/root.qcow2" --format qcow2 --size 64M
"$qtr" disk info --path "$work_dir/root.qcow2" >"$artifact_dir/disk-before.txt"
"$qtr" host fix-vm-perms --file "$manifest" --qemu-user qemu

"$qtr" vm apply --file "$manifest"
"$qtr" vm list >"$artifact_dir/vm-list.txt"
"$qtr" vm dump "$vm_name" --output "$artifact_dir/domain.yaml"
"$qtr" vm dump "$vm_name" --xml >"$artifact_dir/domain.xml"
grep -Fq "<vcpupin vcpu='0' cpuset='0'/>" "$artifact_dir/domain.xml"
grep -Fq "<emulatorpin cpuset='0'/>" "$artifact_dir/domain.xml"
grep -Fq "<iothreadpin iothread='1' cpuset='0'/>" "$artifact_dir/domain.xml"
grep -Fq "<memory mode='strict' nodeset='0'/>" "$artifact_dir/domain.xml"

api_request --request POST "$api_url/vms/$vm_name/start" --output /dev/null
assert_state running
virsh --connect qemu:///system vcpupin "$vm_name" >"$artifact_dir/vcpupin.txt"
virsh --connect qemu:///system numatune "$vm_name" >"$artifact_dir/numatune.txt"

api_request --request POST "$api_url/vms/$vm_name/suspend" --output /dev/null
assert_state paused
api_request --request POST "$api_url/vms/$vm_name/resume" --output /dev/null
assert_state running
api_request --request POST "$api_url/vms/$vm_name/reset" --output /dev/null
assert_state running
printf 'REST reboot intentionally omitted: the empty test disk has no guest to acknowledge reboot.\n'

api_request "$api_url/vms/$vm_name/guest-status" \
    >"$artifact_dir/api-guest-status.json"
jq -e \
    --arg name "$vm_name" \
    '.name == $name and .domainState == "running" and .guestAgentReady == false and .networkInterfacesAvailable == false and (.interfaces | length) == 0' \
    "$artifact_dir/api-guest-status.json" >/dev/null

"$qtr" vm save "$vm_name"
test "$("$qtr" vm saved-state "$vm_name")" = present
"$qtr" vm restore "$vm_name"
assert_state running

"$qtr" vm stop "$vm_name" --force --wait --shutdown-timeout-secs 30
assert_state "shut off"
"$qtr" vm disk-resize "$vm_name" root 128MiB
"$qtr" disk info --path "$work_dir/root.qcow2" >"$artifact_dir/disk-after.txt"

api_request --request DELETE "$api_url/images/$clone_image_id" --output /dev/null
api_request --request DELETE "$api_url/images/$base_image_id" --output /dev/null
api_request --request DELETE "$api_url/media/$seed_id" --output /dev/null

test ! -e "$api_image_root/$clone_image_id"
test ! -e "$api_image_root/$base_image_id"
test ! -e "$api_media_root/$seed_id"

"$qtr" vm rm "$vm_name"
if virsh --connect qemu:///system dominfo "$vm_name" >/dev/null 2>&1; then
    printf 'domain still exists after qtr vm rm: %s\n' "$vm_name" >&2
    exit 1
fi

printf 'qtr libvirt lifecycle and REST automation E2E passed\n'
