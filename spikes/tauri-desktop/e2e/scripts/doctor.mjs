import { spawnSync } from "node:child_process";

function requireBinary(bin, installHint) {
  const probe = spawnSync("which", [bin], { stdio: "pipe" });
  if (probe.status !== 0) {
    throw new Error(`missing required binary '${bin}'. ${installHint}`);
  }
}

try {
  requireBinary("tauri-driver", "Install with: cargo install tauri-driver --locked");
  requireBinary("WebKitWebDriver", "Install your distro package (example Debian/Ubuntu: apt install webkit2gtk-driver)");
  process.stdout.write("e2e doctor OK\n");
} catch (error) {
  process.stderr.write(`${String(error.message || error)}\n`);
  process.exit(1);
}
