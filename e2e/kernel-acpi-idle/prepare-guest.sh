#!/usr/bin/env bash
set -Eeuo pipefail

readonly rpm_path=${1:?kernel RPM path is required}
readonly aml_tool=${2:?AML tool path is required}
readonly state_dir=/var/lib/qtr-kernel-acpi-idle
readonly work_dir=/var/tmp/qtr-kernel-acpi-idle-prepare

if (( EUID != 0 )); then
    printf 'guest preparation must run as root\n' >&2
    exit 1
fi
for command in rpm dracut grubby python3 cpio; do
    command -v "$command" >/dev/null || {
        printf 'missing guest command: %s\n' "$command" >&2
        exit 1
    }
done
[[ -r $rpm_path ]] || {
    printf 'kernel RPM is not readable: %s\n' "$rpm_path" >&2
    exit 1
}
[[ -r $aml_tool ]] || {
    printf 'AML tool is not readable: %s\n' "$aml_tool" >&2
    exit 1
}
[[ -r /sys/firmware/acpi/tables/DSDT && -r /sys/firmware/acpi/tables/FACP ]] || {
    printf 'guest ACPI DSDT/FACP tables are not readable\n' >&2
    exit 1
}

rm -rf -- "$work_dir"
mkdir -p "$work_dir/early/kernel/firmware/acpi" "$state_dir"
cleanup() {
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

package_name=$(rpm -qp --queryformat '%{NAME}' "$rpm_path")
kernel_release=$(rpm -qp --queryformat '%{VERSION}-%{RELEASE}.%{ARCH}' "$rpm_path")
readonly package_name kernel_release
[[ $package_name == kernel* ]] || {
    printf 'RPM package name must start with kernel, got: %s\n' "$package_name" >&2
    exit 1
}

rpm -ivh --replacepkgs "$rpm_path"
[[ -d /lib/modules/$kernel_release ]] || {
    printf 'RPM did not install /lib/modules/%s\n' "$kernel_release" >&2
    exit 1
}
[[ -r /boot/vmlinuz-$kernel_release ]] || {
    printf 'RPM did not install /boot/vmlinuz-%s\n' "$kernel_release" >&2
    exit 1
}

config_path=/boot/config-$kernel_release
if [[ ! -r $config_path ]]; then
    config_path=/lib/modules/$kernel_release/config
fi
[[ -r $config_path ]] || {
    printf 'kernel config is unavailable for %s\n' "$kernel_release" >&2
    exit 1
}
if ! grep -qx 'CONFIG_ACPI_TABLE_UPGRADE=y' "$config_path"; then
    printf 'CONFIG_ACPI_TABLE_UPGRADE=y is required in %s\n' "$config_path" >&2
    exit 1
fi

python3 "$aml_tool" patch \
    --dsdt /sys/firmware/acpi/tables/DSDT \
    --fadt /sys/firmware/acpi/tables/FACP \
    --output "$state_dir/DSDT" \
    --metadata "$state_dir/acpi-expected.json"
install -m 0644 "$state_dir/DSDT" "$work_dir/early/kernel/firmware/acpi/DSDT"
touch -d @0 \
    "$work_dir/early/kernel" \
    "$work_dir/early/kernel/firmware" \
    "$work_dir/early/kernel/firmware/acpi" \
    "$work_dir/early/kernel/firmware/acpi/DSDT"

(
    cd "$work_dir/early"
    printf '%s\n' kernel kernel/firmware kernel/firmware/acpi kernel/firmware/acpi/DSDT | \
        cpio --quiet --create --format=newc --owner=0:0 --reproducible >"$work_dir/early.cpio"
)
dracut --force --kver "$kernel_release" "$work_dir/main-initramfs.img"
cat "$work_dir/early.cpio" "$work_dir/main-initramfs.img" >"/boot/initramfs-$kernel_release.img"
chmod 0600 "/boot/initramfs-$kernel_release.img"

python3 - "/boot/initramfs-$kernel_release.img" <<'PY'
import sys

with open(sys.argv[1], "rb") as stream:
    if stream.read(6) != b"070701":
        raise SystemExit("target initramfs does not start with an uncompressed newc archive")
PY

if grubby --info="/boot/vmlinuz-$kernel_release" >/dev/null 2>&1; then
    grubby --remove-kernel="/boot/vmlinuz-$kernel_release"
fi
grubby \
    --add-kernel="/boot/vmlinuz-$kernel_release" \
    --initrd="/boot/initramfs-$kernel_release.img" \
    --title="qtr ACPI idle E2E ($kernel_release)" \
    --copy-default \
    --make-default
[[ $(grubby --default-kernel) == "/boot/vmlinuz-$kernel_release" ]] || {
    printf 'grubby did not select target kernel as default\n' >&2
    exit 1
}

printf '%s\n' "$kernel_release" >"$state_dir/kernel-release"
printf 'package=%s\nkernel_release=%s\ninitramfs=/boot/initramfs-%s.img\n' \
    "$package_name" "$kernel_release" "$kernel_release"
