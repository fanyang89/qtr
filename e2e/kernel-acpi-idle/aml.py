#!/usr/bin/env python3
"""Strictly patch and verify QEMU ProcessorOp P_BLK fields."""

import argparse
import hashlib
import json
import struct
from pathlib import Path

ACPI_HEADER_LENGTH = 36
PROCESSOR_OP = b"\x5b\x83"
PLACEHOLDER_PBLK = 0xDEAD0000


def fail(message):
    raise ValueError(message)


def read_table(path, signature):
    data = bytearray(Path(path).read_bytes())
    if len(data) < ACPI_HEADER_LENGTH:
        fail(f"{path}: truncated ACPI header")
    if bytes(data[:4]) != signature:
        fail(f"{path}: expected signature {signature!r}, got {bytes(data[:4])!r}")
    declared_length = struct.unpack_from("<I", data, 4)[0]
    if declared_length != len(data):
        fail(f"{path}: declared length {declared_length} != file length {len(data)}")
    if sum(data) & 0xFF:
        fail(f"{path}: invalid ACPI checksum")
    return data


def decode_pkg_length(data, offset, limit):
    if offset >= limit:
        fail("ProcessorOp has no PkgLength")
    lead = data[offset]
    follow = lead >> 6
    encoded_length = follow + 1
    if offset + encoded_length > limit:
        fail("ProcessorOp has truncated PkgLength")
    if follow == 0:
        return lead & 0x3F, encoded_length
    if lead & 0x30:
        fail("ProcessorOp PkgLength uses reserved lead-byte bits")
    value = lead & 0x0F
    for index in range(follow):
        value |= data[offset + index + 1] << (4 + index * 8)
    return value, encoded_length


def parse_nameseg(data, offset, limit):
    if offset + 4 > limit:
        fail("ProcessorOp has truncated NameSeg")
    raw = bytes(data[offset : offset + 4])
    first = raw[0]
    if not (first == ord("_") or ord("A") <= first <= ord("Z")):
        fail(f"ProcessorOp has invalid NameSeg {raw!r}")
    for char in raw[1:]:
        if not (
            char == ord("_")
            or ord("A") <= char <= ord("Z")
            or ord("0") <= char <= ord("9")
        ):
            fail(f"ProcessorOp has invalid NameSeg {raw!r}")
    return raw.decode("ascii"), offset + 4


def parse_name_string(data, offset, limit):
    prefixes = ""
    if offset < limit and data[offset] == 0x5C:
        prefixes = "\\"
        offset += 1
    else:
        while offset < limit and data[offset] == 0x5E:
            prefixes += "^"
            offset += 1
    if offset >= limit or data[offset] == 0x00:
        fail("ProcessorOp must have a non-null NameString")
    if data[offset] == 0x2E:
        count = 2
        offset += 1
    elif data[offset] == 0x2F:
        if offset + 1 >= limit:
            fail("ProcessorOp has truncated MultiNamePrefix")
        count = data[offset + 1]
        offset += 2
        if count == 0:
            fail("ProcessorOp MultiNamePrefix has zero segments")
    else:
        count = 1
    segments = []
    for _ in range(count):
        segment, offset = parse_nameseg(data, offset, limit)
        segments.append(segment)
    return prefixes + ".".join(segments), offset


def parse_processors(dsdt):
    body = dsdt[ACPI_HEADER_LENGTH:]
    if b"_CST" in body:
        fail("DSDT contains _CST; this harness only accepts FADT/P_BLK idle tables")

    processors = []
    cursor = ACPI_HEADER_LENGTH
    while True:
        opcode = dsdt.find(PROCESSOR_OP, cursor)
        if opcode < 0:
            break
        package_start = opcode + len(PROCESSOR_OP)
        package_length, encoded_length = decode_pkg_length(
            dsdt, package_start, len(dsdt)
        )
        package_end = package_start + package_length
        fields = package_start + encoded_length
        if package_end > len(dsdt):
            fail(f"ProcessorOp at 0x{opcode:x} extends beyond DSDT")
        name, fields = parse_name_string(dsdt, fields, package_end)
        if fields + 6 > package_end:
            fail(f"ProcessorOp {name} has truncated processor fields")
        processor_id = dsdt[fields]
        address_offset = fields + 1
        pblk_address = struct.unpack_from("<I", dsdt, address_offset)[0]
        length_offset = address_offset + 4
        pblk_length = dsdt[length_offset]
        processors.append(
            {
                "name": name,
                "processor_id": processor_id,
                "pblk_address": pblk_address,
                "pblk_length": pblk_length,
                "opcode_offset": opcode,
                "package_end": package_end,
                "address_offset": address_offset,
                "length_offset": length_offset,
            }
        )
        cursor = opcode + 2

    if not processors:
        fail("DSDT contains no AML ProcessorOp")
    names = [processor["name"] for processor in processors]
    if len(names) != len(set(names)):
        fail("DSDT contains duplicate ProcessorOp names")
    ordered = sorted(processors, key=lambda processor: processor["opcode_offset"])
    for left, right in zip(ordered, ordered[1:]):
        if right["opcode_offset"] < left["package_end"]:
            fail(
                f"nested or overlapping ProcessorOp packages: {left['name']} and {right['name']}"
            )
    return processors


def fadt_idle_info(path):
    fadt = read_table(path, b"FACP")
    if len(fadt) < 100:
        fail("FADT is too short to contain C2/C3 latency fields")
    c2_latency, c3_latency = struct.unpack_from("<HH", fadt, 96)
    if c2_latency <= 100 or c3_latency <= 1000:
        fail(
            "FADT would permit P_BLK C2/C3 access: "
            f"C2={c2_latency} (must be >100), C3={c3_latency} (must be >1000)"
        )
    return {
        "c2_latency": c2_latency,
        "c3_latency": c3_latency,
        "idle_states_valid": False,
    }


def public_processor(processor):
    return {
        "name": processor["name"],
        "processor_id": processor["processor_id"],
        "pblk_address": processor["pblk_address"],
        "pblk_length": processor["pblk_length"],
    }


def patch_dsdt(dsdt_path, fadt_path, output_path, metadata_path):
    fadt = fadt_idle_info(fadt_path)
    dsdt = read_table(dsdt_path, b"DSDT")
    processors = parse_processors(dsdt)
    for processor in processors:
        if processor["pblk_address"] != 0 or processor["pblk_length"] != 0:
            fail(
                f"ProcessorOp {processor['name']} starts with P_BLK "
                f"address=0x{processor['pblk_address']:x}, length={processor['pblk_length']}; expected 0/0"
            )

    original_revision = struct.unpack_from("<I", dsdt, 24)[0]
    if original_revision == 0xFFFFFFFF:
        fail("DSDT OEM revision cannot be incremented")
    patched_revision = original_revision + 1
    for processor in processors:
        struct.pack_into("<I", dsdt, processor["address_offset"], PLACEHOLDER_PBLK)
        dsdt[processor["length_offset"]] = 6
    struct.pack_into("<I", dsdt, 24, patched_revision)
    dsdt[9] = 0
    dsdt[9] = (-sum(dsdt)) & 0xFF
    if sum(dsdt) & 0xFF:
        fail("internal error while recalculating DSDT checksum")

    patched_processors = parse_processors(dsdt)
    metadata = {
        "original_oem_revision": original_revision,
        "patched_oem_revision": patched_revision,
        "patched_sha256": hashlib.sha256(dsdt).hexdigest(),
        "placeholder_pblk": PLACEHOLDER_PBLK,
        "processors": [public_processor(processor) for processor in patched_processors],
        "fadt": fadt,
    }
    Path(output_path).write_bytes(dsdt)
    Path(metadata_path).write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n"
    )
    return metadata


def verify_tables(dsdt_path, fadt_path, metadata_path):
    expected = json.loads(Path(metadata_path).read_text())
    fadt = fadt_idle_info(fadt_path)
    dsdt = read_table(dsdt_path, b"DSDT")
    processors = parse_processors(dsdt)
    revision = struct.unpack_from("<I", dsdt, 24)[0]
    digest = hashlib.sha256(dsdt).hexdigest()
    actual_processors = [public_processor(processor) for processor in processors]
    if digest != expected["patched_sha256"]:
        fail(f"running DSDT SHA-256 {digest} does not match early override")
    if revision != expected["patched_oem_revision"]:
        fail(f"running DSDT OEM revision {revision} does not match patched revision")
    if actual_processors != expected["processors"]:
        fail("running DSDT ProcessorOp fields do not match the patched table")
    for processor in processors:
        if processor["pblk_address"] == 0 or processor["pblk_length"] != 6:
            fail(
                f"ProcessorOp {processor['name']} has P_BLK "
                f"address=0x{processor['pblk_address']:x}, length={processor['pblk_length']}"
            )
    return {
        "sha256": digest,
        "oem_revision": revision,
        "processor_count": len(processors),
        "processors": actual_processors,
        "fadt": fadt,
        "override_active": True,
    }


def main():
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    patch = subparsers.add_parser("patch")
    patch.add_argument("--dsdt", required=True)
    patch.add_argument("--fadt", required=True)
    patch.add_argument("--output", required=True)
    patch.add_argument("--metadata", required=True)
    verify = subparsers.add_parser("verify")
    verify.add_argument("--dsdt", required=True)
    verify.add_argument("--fadt", required=True)
    verify.add_argument("--metadata", required=True)
    args = parser.parse_args()

    if args.command == "patch":
        result = patch_dsdt(args.dsdt, args.fadt, args.output, args.metadata)
    else:
        result = verify_tables(args.dsdt, args.fadt, args.metadata)
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
