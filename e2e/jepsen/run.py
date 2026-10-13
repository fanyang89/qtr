#!/usr/bin/env python3
"""Launch the pinned Jepsen adapter against three real, disposable cloud guests."""

import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import subprocess
import tempfile
import urllib.error


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("qtr_cloud_fixture", ROOT / "e2e/cloud-vm/run.py")
CLOUD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CLOUD)
CONTROLLER_IMAGE = "clojure:temurin-21-lein-2.13.0"


class SmokeTest:
    def __init__(self, work, artifacts):
        self.work = work
        self.artifacts = artifacts
        self.fixture = CLOUD.CloudTest(work, artifacts)
        self.run_id = "smoke-" + secrets.token_hex(6)
        self.run_ids = [self.run_id, self.run_id + "-fail"]
        self.cidfile = work / "controller.cid"
        self.runs_attempted = False

    def stop_controller(self):
        if self.cidfile.exists():
            container = self.cidfile.read_text().strip()
            # Delete only the exact container Docker recorded for this invocation.
            if re.fullmatch(r"[0-9a-f]{64}", container):
                subprocess.run(["docker", "rm", "--force", container],
                               capture_output=True, timeout=30, check=True)
                self.cidfile.unlink()
            else:
                raise RuntimeError("invalid controller cidfile; refusing to guess a container ID")

    def cleanup_runs(self):
        if not self.runs_attempted:
            return
        failures = []
        for run_id in self.run_ids:
            try:
                run = self.fixture.api("POST", f"/runs/{run_id}/cleanup")
                if run["state"] != "closed":
                    raise RuntimeError("run did not close")
                (self.artifacts / f"{run_id}-final.json").write_text(json.dumps(run, indent=2))
            except urllib.error.HTTPError as error:
                if error.code != 404:
                    failures.append(run_id)
            except Exception:
                failures.append(run_id)
        if failures:
            raise RuntimeError("cleanup incomplete; retain base and inspect runs: " + ", ".join(failures))

    def diagnostics(self):
        files = [self.fixture.logs / f"{run_id}-{logical}.serial.log"
                 for run_id in self.run_ids for logical in ("n1", "n2", "n3")]
        service_log = os.environ.get("QTR_CLOUD_SERVICE_LOG")
        if service_log:
            files.append(Path(service_log))
        errors = []
        for path in files:
            try:
                if path.is_file():
                    destination = "qtr-service.log" if service_log and path == Path(service_log) else path.name
                    shutil.copyfile(path, self.artifacts / destination)
            except OSError as error:
                errors.append({"file": path.name, "error": type(error).__name__})
        if errors:
            (self.artifacts / "diagnostics-errors.json").write_text(json.dumps(errors, indent=2))

    def finish(self):
        controller_error = None
        try:
            self.stop_controller()
        except Exception as error:
            controller_error = error
        try:
            self.diagnostics()
        finally:
            # Never delete the base while a failed run or controller may need it.
            self.cleanup_runs()
            if controller_error:
                raise RuntimeError("controller removal failed; retain the base for recovery") from controller_error
            self.fixture.cleanup()

    def run(self, image):
        fixture = self.fixture
        (self.artifacts / "resources.json").write_text(json.dumps({
            "runIds": self.run_ids, "baseImageId": fixture.base,
        }, indent=2))
        CLOUD.command(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(fixture.key)])
        fixture.create(("images", fixture.base), "PUT", f"/images/{fixture.base}", image=image)
        base = fixture.images / fixture.base
        if CLOUD.digest(base) != CLOUD.IMAGE_SHA256:
            raise AssertionError("import changed the pinned base image")
        config = {
            "endpoint": fixture.api_url.removesuffix("/api/v1"),
            "tokenFile": "/secrets/api-token", "privateKeyPath": "/secrets/id_ed25519",
            "publicKey": fixture.key.with_suffix(".pub").read_text().strip(),
            "runId": self.run_id, "baseImageId": fixture.base,
            "artifactDir": "/artifacts",
        }
        config_path = self.work / "config.json"
        config_path.write_text(json.dumps(config))
        config_path.chmod(0o600)
        target = self.work / "target"
        target.mkdir()
        home = self.work / "home"
        home.mkdir()
        cache = Path(os.environ.get("QTR_JEPSEN_M2", ROOT / ".tmp/jepsen-smoke-m2")).resolve()
        cache.mkdir(parents=True, exist_ok=True)
        token = Path(os.environ["QTR_API_TOKEN_FILE"]).resolve()
        self.runs_attempted = True
        # Host networking reaches the loopback API and the libvirt guest subnet.
        # Only the controller runs here; libvirt and qtr remain on the host.
        network = os.environ.get("QTR_JEPSEN_NETWORK", "host")
        if network != "host" and not re.fullmatch(r"container:[A-Za-z0-9][A-Za-z0-9_.-]*", network):
            raise ValueError("QTR_JEPSEN_NETWORK must be host or container:<id-or-name>")
        command = [
            "docker", "run", "--cidfile", str(self.cidfile), "--network", network,
            "--user", f"{os.getuid()}:{os.getgid()}",
            "--volume", f"{ROOT / 'integrations/jepsen'}:/workspace:ro",
            "--volume", f"{target}:/workspace/target",
            "--volume", f"{home}:/home/controller",
            "--volume", f"{cache}:/home/controller/.m2",
            "--volume", f"{config_path}:/config.json:ro",
            "--volume", f"{token}:/secrets/api-token:ro",
            "--volume", f"{fixture.key}:/secrets/id_ed25519:ro",
            "--volume", f"{self.artifacts}:/artifacts",
            "--workdir", "/workspace", "--env", "HOME=/home/controller",
            "--env", "LEIN_HOME=/home/controller/.lein",
            "--env", "JVM_OPTS=-Xmx512m -Duser.home=/home/controller "
            "-Dlogback.configurationFile=/workspace/resources/qtr-smoke-logback.xml",
            CONTROLLER_IMAGE,
            "timeout", "--signal=TERM", "--kill-after=30s", "24m",
            "lein", "run", "-m", "qtr.jepsen.smoke", "/config.json",
        ]
        print(f"Starting three-node Jepsen smoke: {self.run_id}", flush=True)
        with (self.artifacts / "controller.log").open("w") as log:
            subprocess.run(command, stdout=log, stderr=subprocess.STDOUT,
                           check=True, timeout=25 * 60)
        result = json.loads((self.artifacts / "result.json").read_text())
        if result.get("ok") is not True:
            raise AssertionError("controller did not report success")
        self.cleanup_runs()
        if CLOUD.digest(base) != CLOUD.IMAGE_SHA256:
            raise AssertionError("Jepsen guests modified the backing image")
        fixture.cleanup()
        if base.exists():
            raise AssertionError("base image remains after deletion")
        print("qtr three-node Jepsen smoke and base-image checks passed", flush=True)


def main():
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, interrupted)
    artifacts = Path(os.environ.get("QTR_E2E_ARTIFACT_DIR", ROOT / ".tmp/e2e/jepsen")).resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    temporary_root = Path(os.environ.get("TMPDIR", Path.home() / "tmp")) / "pi"
    temporary_root.mkdir(parents=True, exist_ok=True)
    cache = Path(os.environ.get("QTR_CLOUD_CACHE", ROOT / ".tmp/cloud-images"))
    with tempfile.TemporaryDirectory(prefix="qtr-jepsen-", dir=temporary_root) as directory:
        test = SmokeTest(Path(directory), artifacts)
        try:
            test.run(CLOUD.verified_image(cache))
        finally:
            test.finish()


if __name__ == "__main__":
    main()
