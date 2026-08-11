#!/usr/bin/env python3
"""Assert the post-reboot ACPI idle and CPU profile state."""

import importlib.util
import json
import os
import re
import sys
from pathlib import Path


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def load_aml(path):
    spec = importlib.util.spec_from_file_location("qtr_acpi_aml", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def read_cpuinfo():
    cpus = []
    for block in Path("/proc/cpuinfo").read_text().strip().split("\n\n"):
        fields = {}
        for line in block.splitlines():
            if ":" in line:
                key, value = line.split(":", 1)
                fields[key.strip()] = value.strip()
        if "processor" in fields:
            cpus.append(fields)
    require(cpus, "/proc/cpuinfo contains no processors")
    return cpus


def online_cpu_paths():
    paths = []
    for path in Path("/sys/devices/system/cpu").glob("cpu[0-9]*"):
        if not re.fullmatch(r"cpu[0-9]+", path.name):
            continue
        online = path / "online"
        if not online.exists() or online.read_text().strip() == "1":
            paths.append(path)
    return sorted(paths, key=lambda path: int(path.name[3:]))


def read_idle_states(cpu_path):
    states = []
    for state_path in sorted((cpu_path / "cpuidle").glob("state*")):
        states.append(
            {
                "name": (state_path / "name").read_text().strip(),
                "desc": (state_path / "desc").read_text().strip(),
            }
        )
    return states


def probe(
    aml_path, profile, expected_driver, target_kernel, old_boot_id, native_vendor
):
    require(
        profile in {"hygon-baremetal", "hygon-kvm", "native-baremetal"},
        "invalid CPU profile",
    )
    require(expected_driver in {"acpi_idle", "none"}, "invalid expected driver")
    boot_id = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    require(boot_id != old_boot_id, "boot_id did not change across qtr vm reboot")
    running_kernel = os.uname().release
    require(
        running_kernel == target_kernel,
        f"uname -r is {running_kernel}, expected {target_kernel}",
    )

    aml = load_aml(aml_path)
    acpi = aml.verify_tables(
        "/sys/firmware/acpi/tables/DSDT",
        "/sys/firmware/acpi/tables/FACP",
        "/var/lib/qtr-kernel-acpi-idle/acpi-expected.json",
    )

    cpus = read_cpuinfo()
    require(
        acpi["processor_count"] == len(cpus),
        f"DSDT has {acpi['processor_count']} processors but /proc/cpuinfo has {len(cpus)}",
    )
    vendors = sorted({cpu.get("vendor_id", "") for cpu in cpus})
    require(len(vendors) == 1 and vendors[0], f"inconsistent CPU vendors: {vendors}")
    flags = [set(cpu.get("flags", "").split()) for cpu in cpus]
    if profile.startswith("hygon-"):
        require(vendors == ["HygonGenuine"], f"Hygon profile exposed vendor {vendors}")
        require(
            all("rdseed" not in cpu_flags for cpu_flags in flags),
            "Hygon profile exposed disabled rdseed",
        )
    else:
        require(
            vendors == [native_vendor],
            f"native profile exposed {vendors}, host is {native_vendor}",
        )
    if profile == "hygon-kvm":
        require(
            all("hypervisor" in cpu_flags for cpu_flags in flags),
            "hygon-kvm lacks hypervisor flag",
        )
    else:
        require(
            all("hypervisor" not in cpu_flags for cpu_flags in flags),
            f"{profile} exposed hypervisor flag",
        )

    driver_path = Path("/sys/devices/system/cpu/cpuidle/current_driver")
    try:
        driver = driver_path.read_text().strip()
    except (FileNotFoundError, PermissionError, OSError):
        driver = None
    if expected_driver == "acpi_idle":
        require(
            driver == "acpi_idle", f"current_driver is {driver!r}, expected acpi_idle"
        )
    else:
        require(
            driver in {None, "none"},
            f"current_driver is {driver!r}, expected none or unreadable",
        )

    idle_states = {}
    if expected_driver == "acpi_idle":
        cpu_paths = online_cpu_paths()
        require(cpu_paths, "no online CPU sysfs directories found")
        for cpu_path in cpu_paths:
            states = read_idle_states(cpu_path)
            require(
                any(
                    state["name"] == "C1" and state["desc"] == "ACPI HLT"
                    for state in states
                ),
                f"{cpu_path.name} lacks C1 / ACPI HLT",
            )
            require(
                all(state["name"] not in {"C2", "C3"} for state in states),
                f"{cpu_path.name} unexpectedly exposes C2/C3",
            )
            idle_states[cpu_path.name] = states

    return {
        "status": "passed",
        "boot_id": boot_id,
        "old_boot_id": old_boot_id,
        "kernel_release": running_kernel,
        "profile": profile,
        "cpu_vendor": vendors[0],
        "hypervisor_flag": all("hypervisor" in cpu_flags for cpu_flags in flags),
        "rdseed_flag": all("rdseed" in cpu_flags for cpu_flags in flags),
        "current_driver": driver,
        "expected_driver": expected_driver,
        "idle_states": idle_states,
        "acpi": acpi,
    }


def main():
    if len(sys.argv) != 7:
        raise SystemExit(
            "usage: probe-guest.py AML PROFILE EXPECTED_DRIVER TARGET_KERNEL OLD_BOOT_ID NATIVE_VENDOR"
        )
    try:
        result = probe(*sys.argv[1:])
    except Exception as error:
        print(
            json.dumps(
                {"status": "failed", "error": str(error)}, indent=2, sort_keys=True
            )
        )
        print(f"guest assertion failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
