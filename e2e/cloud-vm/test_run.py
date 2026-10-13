import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError, URLError

import run


def http_error(code):
    return HTTPError("http://127.0.0.1:8080/api/v1/test", code, "test", {}, None)


class CloudHarnessTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        token = self.root / "token"
        token.write_text("test-secret")
        environment = {
            "QTR_API_URL": "http://127.0.0.1:8080/api/v1",
            "QTR_API_TOKEN_FILE": str(token),
            "QTR_IMAGE_ROOT": str(self.root),
            "QTR_MEDIA_ROOT": str(self.root),
            "QTR_LOG_ROOT": str(self.root),
        }
        self.env = patch.dict(os.environ, environment)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.test = run.CloudTest(self.root, self.root)

    def test_rejects_remote_endpoint_disguised_as_loopback(self):
        with patch.dict(os.environ, {"QTR_API_URL": "http://127.0.0.1:80@example.org/api/v1"}):
            with self.assertRaisesRegex(ValueError, "loopback"):
                run.CloudTest(self.root, self.root)

    def test_cleanup_order_and_idempotence(self):
        self.test.resources = [("images", "base"), ("images", "overlay"), ("media", "seed"), ("vms", "vm")]
        calls = []
        self.test.api = lambda method, route: calls.append((method, route))
        self.test.cleanup()
        self.test.cleanup()
        self.assertEqual(calls, [
            ("POST", "/vms/vm/destroy"), ("DELETE", "/vms/vm"),
            ("DELETE", "/media/seed"), ("DELETE", "/images/overlay"), ("DELETE", "/images/base"),
        ])

    def test_cleanup_keeps_failed_resources_for_retry(self):
        self.test.resources = [("images", "overlay"), ("vms", "vm")]
        def fail(method, route):
            raise http_error(500)
        self.test.api = fail
        with self.assertRaisesRegex(RuntimeError, "cleanup incomplete"):
            self.test.cleanup()
        self.assertEqual(len(self.test.resources), 2)
        self.test.api = lambda method, route: None
        self.test.cleanup()
        self.assertEqual(self.test.resources, [])

    def test_missing_resources_are_cleaned(self):
        self.test.resources = [("vms", "vm")]
        def missing(method, route):
            raise http_error(404)
        self.test.api = missing
        self.test.cleanup()
        self.assertEqual(self.test.resources, [])

    def test_ambiguous_creation_retains_intent(self):
        def lost_response(*args, **kwargs):
            raise URLError("response lost")
        self.test.api = lost_response
        with self.assertRaises(URLError):
            self.test.create(("images", "overlay"), "POST", "/images/base/clone")
        self.assertEqual(self.test.resources, [("images", "overlay")])

    def test_conflict_does_not_claim_existing_resource(self):
        def conflict(*args, **kwargs):
            raise http_error(409)
        self.test.api = conflict
        with self.assertRaises(HTTPError):
            self.test.create(("images", "existing"), "POST", "/images")
        self.assertEqual(self.test.resources, [])

    def test_reboot_requires_new_boot_id(self):
        old = "11111111-1111-1111-1111-111111111111"
        new = "22222222-2222-2222-2222-222222222222"
        self.test.api = lambda *args: {
            "guestAgentReady": True, "networkInterfacesAvailable": True,
            "interfaces": [{"addresses": [{"usable": True, "type": "ipv4", "address": "192.168.122.2"}]}],
        }
        boots = iter([old, new])
        scripts = []
        def ssh(script, timeout=20):
            scripts.append(script)
            return next(boots) if "boot_id" in script else ""
        self.test.ssh = ssh
        with patch.object(run.time, "sleep"):
            self.assertEqual(self.test.wait_ready(previous_boot=old), new)
        self.assertEqual(scripts.count("cat /proc/sys/kernel/random/boot_id"), 2)
        self.assertIn("sudo cloud-init status --wait", scripts)

    def test_readiness_times_out(self):
        with self.assertRaises(TimeoutError):
            self.test.wait_ready(timeout=0)

    def test_complete_flow_cleans_only_run_resources(self):
        source = self.root / "source.qcow2"
        source.write_bytes(b"base-image")
        unrelated = self.root / "unrelated.qcow2"
        unrelated.write_bytes(b"unrelated")
        requests = []
        vms = set()
        def api(method, route, data=None, image=None):
            requests.append((method, route))
            if method == "PUT":
                (self.root / self.test.base).write_bytes(image.read_bytes())
            elif route.endswith("/clone"):
                (self.root / data["id"]).write_bytes(b"overlay")
            elif route == "/media/cloud-init":
                (self.root / data["id"]).write_bytes(b"seed")
            elif method == "POST" and route == "/vms":
                vms.add(data["name"])
            elif method == "DELETE":
                kind, resource_id = route.strip("/").split("/")
                if kind == "vms":
                    vms.remove(resource_id)
                else:
                    (self.root / resource_id).unlink()
            elif method == "GET":
                raise http_error(404)
        self.test.api = api
        self.test.key.with_suffix(".pub").write_text("ssh-ed25519 test")
        with patch.object(run, "command"), patch.object(run, "IMAGE_SHA256", run.digest(source)), \
                patch.object(self.test, "wait_ready", side_effect=["boot-a", "boot-b"]):
            self.test.run(source)
        self.assertEqual(vms, set())
        self.assertEqual(self.test.resources, [])
        self.assertEqual(unrelated.read_bytes(), b"unrelated")
        self.assertIn(("POST", f"/vms/{self.test.name}/reboot"), requests)
        self.assertFalse((self.root / self.test.base).exists())

    def test_bad_download_is_not_published(self):
        with patch.object(run.urllib.request, "urlopen", return_value=io.BytesIO(b"corrupt")):
            with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                run.verified_image(self.root / "cache")
        self.assertEqual(list((self.root / "cache").iterdir()), [])

    def test_verified_cache_skips_download(self):
        path = self.root / f"{run.IMAGE_SHA256}.qcow2"
        path.write_bytes(b"fixture")
        with patch.object(run, "digest", return_value=run.IMAGE_SHA256), patch.object(run.urllib.request, "urlopen") as fetch:
            self.assertEqual(run.verified_image(self.root), path)
        fetch.assert_not_called()

    def test_mirror_cannot_override_checksum(self):
        with patch.dict(os.environ, {"QTR_CLOUD_IMAGE_URL": "http://untrusted/image"}):
            with self.assertRaisesRegex(ValueError, "HTTPS"):
                run.verified_image(self.root / "cache")


if __name__ == "__main__":
    unittest.main()
