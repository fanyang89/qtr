#!/usr/bin/env bash
set -Eeuo pipefail

readonly libvirt_uri="qemu:///system"

die() {
    printf '::error::%s\n' "$*" >&2
    exit 1
}

notice() {
    printf '::notice::%s\n' "$*"
}

set_output() {
    printf '%s=%s\n' "$1" "$2" >>"$GITHUB_OUTPUT"
}

set_environment() {
    printf '%s=%s\n' "$1" "$2" >>"$GITHUB_ENV"
}

save_state() {
    printf '%s=%s\n' "$1" "$2" >>"$GITHUB_STATE"
}

download_file() {
    local url=$1
    local output=$2

    case "$url" in
        https://*)
            curl \
                --fail \
                --location \
                --proto '=https' \
                --proto-redir '=https' \
                --retry 3 \
                --retry-all-errors \
                --show-error \
                --silent \
                --tlsv1.2 \
                --output "$output" \
                "$url"
            ;;
        file://*)
            cp -- "${url#file://}" "$output"
            ;;
        *)
            die "download URLs must use https:// or file://"
            ;;
    esac
}

validate_archive_entries() {
    local archive=$1
    local expected_root=$2
    local entry
    local mode

    while IFS= read -r entry; do
        [[ -n "$entry" ]] || die "release archive contains an empty path"
        [[ "$entry" == "$expected_root" || "$entry" == "$expected_root/"* ]] \
            || die "release archive entry is outside $expected_root: $entry"
        [[ "/$entry/" != *"/../"* ]] \
            || die "release archive entry contains a parent traversal: $entry"
    done < <(tar -tzf "$archive")

    while read -r mode _; do
        [[ "${mode:0:1}" == "-" || "${mode:0:1}" == "d" ]] \
            || die "release archive contains a non-file entry of type ${mode:0:1}"
    done < <(tar -tvzf "$archive")
}

start_libvirt() {
    if systemctl list-unit-files libvirtd.socket --no-legend 2>/dev/null | grep -q '^libvirtd.socket'; then
        sudo -n systemctl start libvirtd.socket
    elif systemctl list-unit-files virtqemud.socket --no-legend 2>/dev/null | grep -q '^virtqemud.socket'; then
        sudo -n systemctl start virtqemud.socket
    elif systemctl list-unit-files libvirtd.service --no-legend 2>/dev/null | grep -q '^libvirtd.service'; then
        sudo -n systemctl start libvirtd.service
    else
        die "no supported libvirt systemd unit was found"
    fi

    sudo -n virsh --connect "$libvirt_uri" uri >/dev/null \
        || die "libvirt did not accept a qemu:///system connection"
}

configure_default_network() {
    local network_xml

    if ! sudo -n virsh --connect "$libvirt_uri" net-info default >/dev/null 2>&1; then
        for network_xml in \
            /usr/share/libvirt/networks/default.xml \
            /etc/libvirt/qemu/networks/default.xml; do
            if [[ -f "$network_xml" ]]; then
                sudo -n virsh --connect "$libvirt_uri" net-define "$network_xml" >/dev/null
                break
            fi
        done
    fi

    sudo -n virsh --connect "$libvirt_uri" net-info default >/dev/null 2>&1 \
        || die "the libvirt default network is not defined"

    if ! sudo -n virsh --connect "$libvirt_uri" net-list --name | grep -Fxq default; then
        sudo -n virsh --connect "$libvirt_uri" net-start default >/dev/null
    fi
    sudo -n virsh --connect "$libvirt_uri" net-autostart default >/dev/null
}

wait_for_api() {
    local pid=$1
    local url=$2
    local log=$3
    local deadline=$((SECONDS + 30))

    while ((SECONDS < deadline)); do
        if curl \
            --fail \
            --max-time 2 \
            --show-error \
            --silent \
            "$url/health" \
            | jq -e '.ok == true and .libvirtUri == "qemu:///system"' >/dev/null; then
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            printf '::group::qtr service log\n' >&2
            tail -n 100 "$log" >&2 || true
            printf '::endgroup::\n' >&2
            die "qtr exited before its API became ready"
        fi
        sleep 1
    done

    printf '::group::qtr service log\n' >&2
    tail -n 100 "$log" >&2 || true
    printf '::endgroup::\n' >&2
    die "timed out waiting for qtr at $url"
}

[[ "${RUNNER_OS:-}" == "Linux" ]] || die "qtr currently supports only Linux runners"
[[ -n "${RUNNER_TEMP:-}" ]] || die "RUNNER_TEMP is not set"
[[ -n "${GITHUB_OUTPUT:-}" ]] || die "GITHUB_OUTPUT is not set"
[[ -n "${GITHUB_ENV:-}" ]] || die "GITHUB_ENV is not set"
[[ -n "${GITHUB_STATE:-}" ]] || die "GITHUB_STATE is not set"

# shellcheck disable=SC1091
source /etc/os-release
[[ "${ID:-}" == "ubuntu" && "${VERSION_ID:-}" == "24.04" ]] \
    || die "qtr currently supports only Ubuntu 24.04 runners"
[[ "$(uname -m)" == "x86_64" ]] || die "qtr release assets currently support only x86_64"
sudo -n true || die "passwordless sudo is required"

version=${QTR_ACTION_VERSION:-}
version=${version#v}
repository=${QTR_ACTION_REPOSITORY:-fanyang89/qtr}
require_kvm=${QTR_ACTION_REQUIRE_KVM:-true}
api_port=${QTR_ACTION_API_PORT:-8080}

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] \
    || die "version must be an exact semantic version such as 0.1.0"
[[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
    || die "repository must have the owner/name form"
[[ "$require_kvm" == "true" || "$require_kvm" == "false" ]] \
    || die "require-kvm must be true or false"
if [[ ! "$api_port" =~ ^[0-9]+$ ]] || ((api_port < 1 || api_port > 65535)); then
    die "api-port must be an integer between 1 and 65535"
fi

readonly target="x86_64-unknown-linux-gnu"
readonly archive_name="qtr-v${version}-${target}.tar.gz"
readonly archive_root="qtr-v${version}-${target}"
archive_url=${QTR_ACTION_ARCHIVE_URL:-"https://github.com/${repository}/releases/download/v${version}/${archive_name}"}
checksum_url=${QTR_ACTION_CHECKSUM_URL:-"${archive_url}.sha256"}

download_dir=$(mktemp -d "$RUNNER_TEMP/qtr-download.XXXXXX")
install_dir=$(mktemp -d "$RUNNER_TEMP/qtr-install.XXXXXX")
save_state install_dir "$install_dir"
archive_path="$download_dir/$archive_name"
checksum_path="$download_dir/$archive_name.sha256"
trap 'rm -rf -- "$download_dir"' EXIT

download_file "$archive_url" "$archive_path"
download_file "$checksum_url" "$checksum_path"

mapfile -t checksum_lines < <(grep -v '^[[:space:]]*$' "$checksum_path")
[[ ${#checksum_lines[@]} -eq 1 ]] \
    || die "release checksum must contain exactly one non-empty line"
read -r expected_sha expected_name extra <<<"${checksum_lines[0]}"
expected_name=${expected_name#\*}
[[ -z "${extra:-}" && "$expected_sha" =~ ^[0-9A-Fa-f]{64}$ && "$expected_name" == "$archive_name" ]] \
    || die "release checksum must contain exactly: <sha256>  $archive_name"
actual_sha=$(sha256sum "$archive_path" | awk '{print $1}')
[[ "${actual_sha,,}" == "${expected_sha,,}" ]] || die "release archive checksum mismatch"

validate_archive_entries "$archive_path" "$archive_root"
tar -xzf "$archive_path" --directory "$install_dir" --strip-components=1
if find "$install_dir" -type l -print -quit | grep -q .; then
    die "release archive must not contain symbolic links"
fi

qtr_path="$install_dir/bin/qtr"
web_dir="$install_dir/share/qtr/web"
[[ -f "$qtr_path" && -x "$qtr_path" ]] || die "release archive does not contain executable bin/qtr"
[[ -f "$web_dir/index.html" ]] || die "release archive does not contain share/qtr/web/index.html"

notice "Installing QEMU and libvirt packages"
sudo -n apt-get update
sudo -n env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    acl \
    curl \
    dnsmasq-base \
    genisoimage \
    jq \
    libvirt-clients \
    libvirt-daemon-config-network \
    libvirt-daemon-driver-qemu \
    libvirt-daemon-system \
    qemu-system-x86 \
    qemu-utils \
    virtinst

"$qtr_path" --version | grep -Fx "qtr $version" >/dev/null \
    || die "release binary version does not match v$version"

kvm_available=false
if [[ -c /dev/kvm ]]; then
    printf '%s\n' 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
        | sudo -n tee /etc/udev/rules.d/99-kvm4all.rules >/dev/null
    sudo -n udevadm control --reload-rules
    sudo -n udevadm trigger --name-match=kvm
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
        kvm_available=true
    fi
fi
if [[ "$require_kvm" == "true" && "$kvm_available" != "true" ]]; then
    ls -l /dev/kvm >&2 2>/dev/null || true
    die "a readable and writable /dev/kvm is required"
fi

start_libvirt
configure_default_network

runner_user=$(id -un)
libvirt_socket_found=false
for libvirt_socket in /run/libvirt/libvirt-sock /run/libvirt/virtqemud-sock; do
    if [[ -S "$libvirt_socket" ]]; then
        sudo -n setfacl -m "u:${runner_user}:rw-" "$libvirt_socket"
        libvirt_socket_found=true
    fi
done
[[ "$libvirt_socket_found" == "true" ]] || die "libvirt read-write socket was not created"
virsh --connect "$libvirt_uri" uri >/dev/null \
    || die "the runner user cannot access qemu:///system"

service_dir=$(mktemp -d "$RUNNER_TEMP/qtr-service.XXXXXX")
state_dir="$service_dir/state"
image_root="$service_dir/images"
media_root="$service_dir/media"
log_root="$service_dir/logs"
service_log="$service_dir/qtr.log"
token_file="$service_dir/api-token"
save_state service_dir "$service_dir"
save_state service_log "$service_log"
save_state qtr_path "$qtr_path"
mkdir -p "$state_dir" "$image_root" "$media_root" "$log_root"

qemu_user=qemu
if getent passwd libvirt-qemu >/dev/null; then
    qemu_user=libvirt-qemu
fi
sudo -n "$qtr_path" host setup-libvirt-access \
    --user "$runner_user" \
    --qemu-user "$qemu_user" \
    --qemu-rw-dir "$image_root" \
    --qemu-ro-dir "$media_root" \
    --qemu-rw-dir "$log_root"

od -An -N32 -tx1 /dev/urandom | tr -d ' \n' >"$token_file"
printf '\n' >>"$token_file"
chmod 0600 "$token_file"
printf '::add-mask::%s\n' "$(<"$token_file")"

api_url="http://127.0.0.1:${api_port}/api/v1"
nohup env -i \
    "HOME=$HOME" \
    "LANG=${LANG:-C.UTF-8}" \
    "PATH=$PATH" \
    "USER=$runner_user" \
    "$qtr_path" web \
    --listen "127.0.0.1:${api_port}" \
    --connect-uri "$libvirt_uri" \
    --web-dir "$web_dir" \
    --api-token-file "$token_file" \
    --state-dir "$state_dir" \
    --image-root "$image_root" \
    --media-root "$media_root" \
    --log-root "$log_root" \
    >"$service_log" 2>&1 &
qtr_pid=$!

save_state qtr_pid "$qtr_pid"
wait_for_api "$qtr_pid" "$api_url" "$service_log"

printf '%s\n' "$install_dir/bin" >>"$GITHUB_PATH"
set_environment QTR_API_URL "$api_url"
set_environment QTR_API_TOKEN_FILE "$token_file"
set_environment QTR_STATE_DIR "$state_dir"
set_environment QTR_IMAGE_ROOT "$image_root"
set_environment QTR_MEDIA_ROOT "$media_root"
set_environment QTR_LOG_ROOT "$log_root"

set_output qtr-path "$qtr_path"
set_output qtr-version "$version"
set_output api-url "$api_url"
set_output api-token-file "$token_file"
set_output state-dir "$state_dir"
set_output image-root "$image_root"
set_output media-root "$media_root"
set_output log-root "$log_root"
set_output service-log "$service_log"
set_output kvm-available "$kvm_available"
set_output libvirt-uri "$libvirt_uri"

notice "qtr v$version is ready at $api_url"
