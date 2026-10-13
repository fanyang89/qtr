#!/usr/bin/env python3
"""Exercise a real cloud guest against a dedicated local qtr server."""

import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


IMAGE_NAME = "ubuntu-24.04-server-cloudimg-amd64.img"
IMAGE_URL = (
    "https://cloud-images.ubuntu.com/releases/noble/release-20260926/" + IMAGE_NAME
)
# Published by Ubuntu in the same release directory's SHA256SUMS.
IMAGE_SHA256 = "6a81c37564db9b1ee84e141922625e1d7c5b389b99bb3c572e0243607d5bb4d2"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def command(args, timeout=20):
    result = subprocess.run(
        args, check=True, capture_output=True, text=True, timeout=timeout
    )
    return result.stdout.strip()


def verified_image(cache):
    cache.mkdir(parents=True, exist_ok=True)
    path = cache / f"{IMAGE_SHA256}.qcow2"
    if path.exists() and digest(path) == IMAGE_SHA256:
        return path
    url = os.environ.get("QTR_CLOUD_IMAGE_URL", IMAGE_URL)
    if not url.startswith("https://"):
        raise ValueError("QTR_CLOUD_IMAGE_URL must use HTTPS")
    # An override is only a mirror; it cannot change the pinned checksum.
    fd, temporary = tempfile.mkstemp(prefix="download-", dir=cache)
    try:
        with os.fdopen(fd, "wb") as output, urllib.request.urlopen(url, timeout=30) as response:
            shutil.copyfileobj(response, output)
        if digest(Path(temporary)) != IMAGE_SHA256:
            raise ValueError("cloud image SHA-256 mismatch")
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)
    return path


class CloudTest:
    def __init__(self, work, artifacts):
        self.work = work
        self.artifacts = artifacts
        self.api_url = os.environ["QTR_API_URL"].rstrip("/")
        endpoint = urllib.parse.urlsplit(self.api_url)
        if (
            endpoint.scheme != "http"
            or endpoint.hostname != "127.0.0.1"
            or endpoint.username is not None
            or endpoint.password is not None
            or endpoint.path != "/api/v1"
            or endpoint.query
            or endpoint.fragment
        ):
            raise ValueError("this test requires a dedicated loopback qtr server")
        self.token = Path(os.environ["QTR_API_TOKEN_FILE"]).read_text().strip()
        self.images = Path(os.environ["QTR_IMAGE_ROOT"])
        self.media = Path(os.environ["QTR_MEDIA_ROOT"])
        self.logs = Path(os.environ["QTR_LOG_ROOT"])
        self.name = "qtr-cloud-" + secrets.token_hex(8)
        self.base = self.name + "-base.qcow2"
        self.overlay = self.name + ".qcow2"
        self.seed = self.name + ".iso"
        self.resources = []
        self.managed_run = os.environ.get("QTR_CLOUD_MANAGED_RUN", "0") == "1"
        self.run_started = False
        self.address = None
        self.key = work / "id_ed25519"
        # Restrict trust-on-first-use to this disposable VM and this run only.
        self.ssh_options = [
            "ssh", "-F", "/dev/null", "-i", str(self.key), "-o", "BatchMode=yes",
            "-o", "IdentitiesOnly=yes", "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", f"UserKnownHostsFile={work / 'known_hosts'}",
            "-o", "GlobalKnownHostsFile=/dev/null",
        ]

    def api(self, method, route, data=None, image=None):
        headers = {"Authorization": "Bearer " + self.token}
        if image is not None:
            headers["Content-Type"] = "application/octet-stream"
            headers["Content-Length"] = str(image.stat().st_size)
            with image.open("rb") as stream:
                request = urllib.request.Request(
                    self.api_url + route, data=stream, headers=headers, method=method
                )
                with urllib.request.urlopen(request, timeout=180) as response:
                    return json.load(response)
        if data is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(data).encode()
        request = urllib.request.Request(
            self.api_url + route, data=data, headers=headers, method=method
        )
        with urllib.request.urlopen(request, timeout=10) as response:
            body = response.read()
            return json.loads(body) if body else None

    def create(self, resource, method, route, **kwargs):
        if self.managed_run and resource != ("images", self.base):
            kinds = {"images": "imageClone", "media": "cloudInit", "vms": "vm"}
            request = {"kind": kinds[resource[0]], "request": kwargs["data"]}
            if resource[0] == "images":
                request["sourceImageId"] = self.base
            return self.api("POST", f"/runs/{self.name}/resources", data=request)
        # Record intent before transport: a response can be lost after creation.
        self.resources.append(resource)
        try:
            return self.api(method, route, **kwargs)
        except urllib.error.HTTPError as error:
            if error.code < 500:
                self.resources.remove(resource)
            raise

    def ssh(self, script, timeout=20):
        return command(self.ssh_options + [f"qtr@{self.address}", script], timeout)

    def wait_ready(self, previous_boot=None, timeout=420):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                status = self.api("GET", f"/vms/{self.name}/guest-status")
                (self.artifacts / "guest-status.json").write_text(json.dumps(status, indent=2))
                if status["guestAgentReady"] and status["networkInterfacesAvailable"]:
                    addresses = [
                        a["address"] for interface in status["interfaces"]
                        for a in interface["addresses"] if a["usable"] and a["type"] == "ipv4"
                    ]
                    for address in addresses:
                        self.address = address
                        boot = self.ssh("cat /proc/sys/kernel/random/boot_id")
                        if re.fullmatch(r"[0-9a-f-]{36}", boot) and boot != previous_boot:
                            self.ssh("sudo cloud-init status --wait", timeout=30)
                            self.ssh(
                                "test -f /var/lib/qtr-cloud-e2e-ready && "
                                "systemctl is-active qemu-guest-agent"
                            )
                            return boot
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                (self.artifacts / "readiness-error.txt").write_text(
                    f"{type(error).__name__}: {error}\n"
                )
            time.sleep(3)
        raise TimeoutError("guest agent, SSH, cloud-init or new boot ID did not become ready")

    def diagnostics(self):
        serial = self.logs / f"{self.name}.serial.log"
        if serial.exists():
            shutil.copyfile(serial, self.artifacts / "serial.log")
        if self.address:
            try:
                output = self.ssh(
                    "sudo cat /var/log/cloud-init.log /var/log/cloud-init-output.log",
                    timeout=15,
                )
                (self.artifacts / "cloud-init.log").write_text(output)
            except (OSError, subprocess.SubprocessError):
                pass
        service_log = os.environ.get("QTR_CLOUD_SERVICE_LOG")
        if service_log and Path(service_log).is_file():
            shutil.copyfile(service_log, self.artifacts / "qtr-service.log")

    def cleanup(self):
        if self.run_started:
            # Do not delete even the separately tracked base if run cleanup fails.
            run = self.api("POST", f"/runs/{self.name}/cleanup")
            if run["state"] != "closed":
                raise RuntimeError("server-managed run did not close")
            self.run_started = False
        errors = []
        for kind, resource_id in reversed(self.resources[:]):
            try:
                if kind == "vms":
                    try:
                        self.api("POST", f"/vms/{resource_id}/destroy")
                    except urllib.error.HTTPError as error:
                        if error.code not in (404, 409):
                            raise
                try:
                    self.api("DELETE", f"/{kind}/{resource_id}")
                except urllib.error.HTTPError as error:
                    if error.code != 404:
                        raise
                self.resources.remove((kind, resource_id))
            except Exception as error:
                errors.append(f"{kind}/{resource_id}: {type(error).__name__}")
        if errors:
            raise RuntimeError("cleanup incomplete: " + "; ".join(errors))

    def run(self, image):
        print(f"Provisioning {self.name}", flush=True)
        (self.artifacts / "resources.json").write_text(json.dumps({
            "vm": self.name, "base": self.base, "overlay": self.overlay, "seed": self.seed,
        }, indent=2))
        command(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(self.key)])
        public_key = self.key.with_suffix(".pub").read_text().strip()
        # JSON is valid YAML: quote the public key without hand-written escaping.
        user_data = "#cloud-config\n" + json.dumps({
            "users": [{"name": "qtr", "sudo": "ALL=(ALL) NOPASSWD:ALL",
                       "shell": "/bin/bash", "lock_passwd": True,
                       "ssh_authorized_keys": [public_key]}],
            "ssh_pwauth": False,
            "packages": ["qemu-guest-agent"],
            "runcmd": [["systemctl", "enable", "--now", "qemu-guest-agent"],
                       ["touch", "/var/lib/qtr-cloud-e2e-ready"]],
        })
        if self.managed_run:
            self.run_started = True
            self.api("PUT", f"/runs/{self.name}")
        self.create(("images", self.base), "PUT", f"/images/{self.base}", image=image)
        print("Base imported; creating linked clone and cloud-init seed", flush=True)
        before = digest(self.images / self.base)
        if before != IMAGE_SHA256:
            raise AssertionError("import changed the base image")
        self.create(
            ("images", self.overlay), "POST", f"/images/{self.base}/clone",
            data={"id": self.overlay},
        )
        self.create(("media", self.seed), "POST", "/media/cloud-init", data={
            "id": self.seed, "instanceId": self.name, "localHostname": self.name, "userData": user_data,
        })
        self.create(("vms", self.name), "POST", "/vms", data={
            "name": self.name, "resources": {"vcpus": 2, "memoryMib": 2048},
            "disks": [{"imageId": self.overlay, "format": "qcow2", "bus": "virtio-blk"}],
            "networkId": "default", "cdroms": [{"id": "cloud-init", "mediaId": self.seed}],
            "console": {"graphics": "none", "serialLog": True},
        })
        print("VM defined; starting guest", flush=True)
        self.api("POST", f"/vms/{self.name}/start")
        boot = self.wait_ready()
        print("Guest ready; testing orderly reboot", flush=True)
        self.api("POST", f"/vms/{self.name}/reboot")
        new_boot = self.wait_ready(previous_boot=boot)
        (self.artifacts / "boot-ids.json").write_text(json.dumps({"before": boot, "after": new_boot}))
        self.diagnostics()
        # Remove the guest and its overlay before checking the backing file.
        base_resource = ("images", self.base)
        self.resources.remove(base_resource)
        try:
            self.cleanup()
            if digest(self.images / self.base) != before:
                raise AssertionError("guest modified the backing image")
        finally:
            self.resources.insert(0, base_resource)
        self.cleanup()
        for path in (self.images / self.overlay, self.images / self.base, self.media / self.seed):
            if path.exists():
                raise AssertionError(f"resource remains: {path}")
        try:
            self.api("GET", f"/vms/{self.name}")
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise
        else:
            raise AssertionError("VM remains after cleanup")
        print("qtr real cloud VM E2E passed", flush=True)


def main():
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, interrupted)
    root = Path(__file__).resolve().parents[2]
    artifacts = Path(os.environ.get("QTR_E2E_ARTIFACT_DIR", root / ".tmp/e2e/cloud-vm"))
    artifacts.mkdir(parents=True, exist_ok=True)
    temporary_root = Path(os.environ.get("TMPDIR", Path.home() / "tmp")) / "pi"
    temporary_root.mkdir(parents=True, exist_ok=True)
    cache = Path(os.environ.get("QTR_CLOUD_CACHE", root / ".tmp/cloud-images"))
    with tempfile.TemporaryDirectory(prefix="qtr-cloud-", dir=temporary_root) as directory:
        test = CloudTest(Path(directory), artifacts)
        try:
            test.run(verified_image(cache))
        finally:
            try:
                test.diagnostics()
            finally:
                test.cleanup()


if __name__ == "__main__":
    main()
