import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const actionDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const cleanupScript = path.join(actionDir, "action", "cleanup.sh");
const result = spawnSync("bash", [cleanupScript], {
  env: process.env,
  stdio: "inherit",
});

if (result.error) {
  console.warn(`qtr cleanup could not start: ${result.error.message}`);
}
