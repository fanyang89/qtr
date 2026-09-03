import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const actionDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const setupScript = path.join(actionDir, "action", "setup.sh");
const input = (name) =>
  process.env[`INPUT_${name.replaceAll(" ", "_").toUpperCase()}`] ?? "";
const env = {
  ...process.env,
  QTR_ACTION_VERSION: input("version"),
  QTR_ACTION_REPOSITORY: input("repository"),
  QTR_ACTION_REQUIRE_KVM: input("require-kvm"),
  QTR_ACTION_API_PORT: input("api-port"),
  QTR_ACTION_ARCHIVE_URL: input("archive-url"),
  QTR_ACTION_CHECKSUM_URL: input("checksum-url"),
};
const result = spawnSync("bash", [setupScript], {
  env,
  stdio: "inherit",
});

if (result.error) {
  console.error(`Failed to start qtr setup: ${result.error.message}`);
  process.exit(1);
}

process.exit(result.status ?? 1);
