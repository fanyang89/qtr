import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError

import run


class SmokeLauncherTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        work = self.root / "work"
        work.mkdir()
        self.artifacts = self.root / "artifacts"
        self.artifacts.mkdir()
        token = self.root / "token"
        token.write_text("never-print-this-token")
        env = patch.dict(os.environ, {
            "QTR_API_URL": "http://127.0.0.1:18080/api/v1",
            "QTR_API_TOKEN_FILE": str(token),
            "QTR_IMAGE_ROOT": str(self.root), "QTR_MEDIA_ROOT": str(self.root),
            "QTR_LOG_ROOT": str(self.root), "QTR_CLOUD_MANAGED_RUN": "0",
            "QTR_JEPSEN_NETWORK": "host", "QTR_JEPSEN_M2": str(self.root / "m2"),
        })
        env.start()
        self.addCleanup(env.stop)
        self.test = run.SmokeTest(work, self.artifacts)

    def test_short_distinct_run_ids_fit_adapter_limits(self):
        self.assertEqual(len(set(self.test.run_ids)), 2)
        self.assertTrue(all(len(run_id) <= 24 for run_id in self.test.run_ids))
        self.assertTrue(all(run_id.startswith("smoke-") for run_id in self.test.run_ids))

    def test_cleanup_failure_preserves_base_and_attempts_both_runs(self):
        self.test.runs_attempted = True
        self.test.fixture.api = Mock(side_effect=HTTPError("url", 409, "conflict", {}, None))
        self.test.fixture.cleanup = Mock()
        with patch.object(self.test, "diagnostics"), self.assertRaisesRegex(RuntimeError, "cleanup incomplete"):
            self.test.finish()
        self.assertEqual(self.test.fixture.api.call_count, 2)
        self.test.fixture.cleanup.assert_not_called()

    def test_cleanup_accepts_missing_run_then_deletes_base(self):
        self.test.runs_attempted = True
        self.test.fixture.api = Mock(side_effect=[
            HTTPError("url", 404, "missing", {}, None), {"state": "closed"},
        ])
        self.test.fixture.cleanup = Mock()
        self.test.finish()
        self.test.fixture.cleanup.assert_called_once()

    def test_container_cleanup_requires_an_exact_recorded_id(self):
        for value in ["", "a-container-name", "--all", "abc\n123"]:
            self.test.cidfile.write_text(value)
            with patch.object(run.subprocess, "run") as execute:
                with self.assertRaisesRegex(RuntimeError, "invalid controller cidfile"):
                    self.test.stop_controller()
            execute.assert_not_called()
        self.test.cidfile.write_text("a" * 64)
        with patch.object(run.subprocess, "run") as execute:
            self.test.stop_controller()
        self.assertEqual(execute.call_args.args[0], ["docker", "rm", "--force", "a" * 64])

    def test_controller_uses_private_mounts_pinned_image_and_nonroot_uid(self):
        fixture = self.test.fixture
        fixture.key.with_suffix(".pub").write_text("ssh-ed25519 fake-public-key")
        base = fixture.images / fixture.base
        fixture.create = Mock(side_effect=lambda *args, **kwargs: base.write_bytes(b"base"))
        fixture.api = Mock(return_value={"state": "closed"})
        fixture.cleanup = Mock(side_effect=lambda: base.unlink(missing_ok=True))
        (self.artifacts / "result.json").write_text(json.dumps({"ok": True}))
        with patch.object(run.CLOUD, "command"), patch.object(run.CLOUD, "digest", return_value=run.CLOUD.IMAGE_SHA256), \
                patch.object(run.subprocess, "run") as execute:
            self.test.run(self.root / "input")
        command = execute.call_args.args[0]
        self.assertIn(run.CONTROLLER_IMAGE, command)
        self.assertIn(f"{os.getuid()}:{os.getgid()}", command)
        self.assertIn(f"{fixture.key}:/secrets/id_ed25519:ro", command)
        self.assertIn("never-print-this-token", (self.root / "token").read_text())
        self.assertNotIn("never-print-this-token", " ".join(command))
        self.assertNotIn("never-print-this-token", (self.test.work / "config.json").read_text())
        self.assertFalse(base.exists())
        self.assertEqual(execute.call_args.kwargs["timeout"], 1500)

    def test_controller_removal_failure_retains_base_but_attempts_run_cleanup(self):
        self.test.runs_attempted = True
        self.test.fixture.api = Mock(return_value={"state": "closed"})
        self.test.fixture.cleanup = Mock()
        with patch.object(self.test, "stop_controller", side_effect=OSError("unavailable")):
            with self.assertRaisesRegex(RuntimeError, "controller removal failed"):
                self.test.finish()
        self.assertEqual(self.test.fixture.api.call_count, 2)
        self.test.fixture.cleanup.assert_not_called()

    def test_unreadable_diagnostics_do_not_prevent_cleanup(self):
        serial = self.test.fixture.logs / f"{self.test.run_id}-n1.serial.log"
        serial.write_text("log")
        self.test.fixture.cleanup = Mock()
        with patch.object(run.shutil, "copyfile", side_effect=PermissionError("unreadable")):
            self.test.finish()
        self.test.fixture.cleanup.assert_called_once()
        errors = json.loads((self.artifacts / "diagnostics-errors.json").read_text())
        self.assertEqual(errors[0]["error"], "PermissionError")

    def test_base_checksum_failure_does_not_launch_controller(self):
        fixture = self.test.fixture
        fixture.key.with_suffix(".pub").write_text("ssh-ed25519 key")
        fixture.create = Mock()
        with patch.object(run.CLOUD, "command"), patch.object(run.CLOUD, "digest", return_value="wrong"), \
                patch.object(run.subprocess, "run") as execute:
            with self.assertRaisesRegex(AssertionError, "pinned base"):
                self.test.run(self.root / "input")
        execute.assert_not_called()
        self.assertFalse(self.test.runs_attempted)


if __name__ == "__main__":
    unittest.main()
