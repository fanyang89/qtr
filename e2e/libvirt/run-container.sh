#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../.." && pwd)
artifact_dir=${QTR_E2E_ARTIFACT_DIR:-"$repo_root/.tmp/e2e/libvirt"}
image=${QTR_E2E_IMAGE:-qtr-e2e-fedora44}
container=${QTR_E2E_CONTAINER_NAME:-"qtr-e2e-$$"}

if [[ ! -c /dev/kvm ]]; then
    printf '/dev/kvm is required for the qtr libvirt E2E test\n' >&2
    exit 1
fi

mkdir -p "$artifact_dir"
artifact_dir=$(cd -- "$artifact_dir" && pwd)

cleanup() {
    docker rm --force "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker build --file "$script_dir/Dockerfile" --tag "$image" "$repo_root"

status=0
docker run \
    --name "$container" \
    --privileged \
    --cgroupns host \
    --device /dev/kvm \
    --env "QTR_E2E_ARTIFACT_GID=$(id -g)" \
    --env "QTR_E2E_ARTIFACT_UID=$(id -u)" \
    --volume /sys/fs/cgroup:/sys/fs/cgroup:rw \
    --volume "$artifact_dir:/artifacts" \
    "$image" || status=$?

docker logs "$container" >"$artifact_dir/container.log" 2>&1 || true
exit "$status"
