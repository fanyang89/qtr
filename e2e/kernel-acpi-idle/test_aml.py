#!/usr/bin/env python3
"""Unit tests for the ACPI table patcher."""

import importlib.util
import struct
import tempfile
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("qtr_acpi_aml", SCRIPT_DIR / "aml.py")
AML = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AML)


def acpi_table(signature, body, oem_revision=1):
    table = bytearray(36 + len(body))
    table[:4] = signature
    struct.pack_into("<I", table, 4, len(table))
    table[8] = 2
    table[10:16] = b"QTR   "
    table[16:24] = b"ACPIE2E "
    struct.pack_into("<I", table, 24, oem_revision)
    table[28:32] = b"QTR "
    struct.pack_into("<I", table, 32, 1)
    table[36:] = body
    table[9] = (-sum(table)) & 0xFF
    return bytes(table)


def processor(name, processor_id, address=0, length=0):
    fields = (
        name.encode("ascii")
        + bytes([processor_id])
        + struct.pack("<I", address)
        + bytes([length])
    )
    return AML.PROCESSOR_OP + bytes([len(fields) + 1]) + fields


def fadt(c2_latency=4095, c3_latency=4095):
    body = bytearray(80)
    struct.pack_into("<HH", body, 60, c2_latency, c3_latency)
    return acpi_table(b"FACP", body)


class AmlTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.dsdt = self.root / "DSDT"
        self.facp = self.root / "FACP"
        self.output = self.root / "patched-DSDT"
        self.metadata = self.root / "metadata.json"

    def tearDown(self):
        self.temp.cleanup()

    def write_tables(self, body, facp=None):
        self.dsdt.write_bytes(acpi_table(b"DSDT", body))
        self.facp.write_bytes(facp or fadt())

    def test_patches_and_verifies_processor_pblk(self):
        self.write_tables(processor("C000", 0) + processor("C001", 1))

        result = AML.patch_dsdt(self.dsdt, self.facp, self.output, self.metadata)
        verified = AML.verify_tables(self.output, self.facp, self.metadata)

        self.assertEqual(result["patched_oem_revision"], 2)
        self.assertEqual(verified["processor_count"], 2)
        self.assertTrue(verified["override_active"])
        for entry in verified["processors"]:
            self.assertEqual(entry["pblk_address"], AML.PLACEHOLDER_PBLK)
            self.assertEqual(entry["pblk_length"], 6)

    def test_rejects_nonzero_original_pblk(self):
        self.write_tables(processor("C000", 0, address=0x1000, length=6))

        with self.assertRaisesRegex(ValueError, "expected 0/0"):
            AML.patch_dsdt(self.dsdt, self.facp, self.output, self.metadata)

    def test_rejects_cst_and_valid_deep_idle_latency(self):
        self.write_tables(processor("C000", 0) + b"_CST")
        with self.assertRaisesRegex(ValueError, "contains _CST"):
            AML.patch_dsdt(self.dsdt, self.facp, self.output, self.metadata)

        self.write_tables(processor("C000", 0), fadt(c2_latency=100))
        with self.assertRaisesRegex(ValueError, "would permit P_BLK C2/C3 access"):
            AML.patch_dsdt(self.dsdt, self.facp, self.output, self.metadata)


if __name__ == "__main__":
    unittest.main()
