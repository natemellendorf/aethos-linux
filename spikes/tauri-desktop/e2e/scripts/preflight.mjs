import { spawnSync } from "node:child_process";

function hasBinary(name) {
  const probe = spawnSync("which", [name], { stdio: "pipe" });
  return probe.status === 0;
}

function runCommand(command, args, env = process.env) {
  const result = spawnSync(command, args, {
    stdio: "inherit",
    env,
    shell: false
  });
  if (result.status !== 0) {
    throw new Error(`command failed: ${command} ${args.join(" ")}`);
  }
}

function binaryWorks(command, args = ["--help"]) {
  const result = spawnSync(command, args, { stdio: "pipe", shell: false });
  const combinedOutput = `${String(result.stdout || "")}${String(result.stderr || "")}`;
  return {
    ok: result.status === 0,
    output: combinedOutput
  };
}

function ensureExplicitE2ERun() {
  const isExplicitE2ERun = process.env.AETHOS_E2E === "1";
  if (isExplicitE2ERun) {
    return true;
  }
  process.stdout.write("e2e preflight: AETHOS_E2E is not set to 1; skipping automatic dependency install\n");
  return false;
}

function ensureTauriDriver() {
  if (!hasBinary("tauri-driver")) {
    const shouldAutoInstall = (process.env.AETHOS_E2E_AUTO_INSTALL_TAURI_DRIVER || "1") !== "0";
    if (!shouldAutoInstall) {
      throw new Error(
        "missing required binary 'tauri-driver'. Install manually (cargo install tauri-driver --locked) or set AETHOS_E2E_AUTO_INSTALL_TAURI_DRIVER=1"
      );
    }
    if (!hasBinary("cargo")) {
      throw new Error(
        "missing required binary 'tauri-driver'. Automatic install requires cargo in PATH. Install Rust toolchain, then retry."
      );
    }

    process.stdout.write("e2e preflight: installing tauri-driver via cargo (binary missing)\n");
    runCommand("cargo", ["install", "tauri-driver", "--locked"]);
  }

  const tauriDriverProbe = binaryWorks("tauri-driver", ["--help"]);
  if (!tauriDriverProbe.ok) {
    const output = tauriDriverProbe.output.toLowerCase();
    if (output.includes("not supported on this platform")) {
      throw new Error(
        "tauri-driver is installed but not supported on this platform. Desktop WebDriver E2E is currently Linux-only."
      );
    }
    throw new Error(`tauri-driver is present but not runnable: ${tauriDriverProbe.output.trim()}`);
  }

  process.stdout.write("e2e preflight OK: tauri-driver present\n");
}

function maybeInstallWebKitWebDriver() {
  if (hasBinary("WebKitWebDriver")) {
    process.stdout.write("e2e preflight OK: WebKitWebDriver present\n");
    return;
  }

  const shouldAutoInstall = (process.env.AETHOS_E2E_AUTO_INSTALL_WEBKIT_DRIVER || "1") !== "0";
  if (!shouldAutoInstall) {
    throw new Error(
      "missing required binary 'WebKitWebDriver'. Install manually (Debian/Ubuntu: apt install webkit2gtk-driver) or set AETHOS_E2E_AUTO_INSTALL_WEBKIT_DRIVER=1"
    );
  }

  if (process.platform !== "linux") {
    throw new Error(
      "missing required binary 'WebKitWebDriver'. Automatic install is only supported on Linux (Debian/Ubuntu apt-get)."
    );
  }

  if (!hasBinary("apt-get")) {
    throw new Error(
      "missing required binary 'WebKitWebDriver'. Automatic install requires apt-get (Debian/Ubuntu)."
    );
  }

  const installerPrefix = process.getuid?.() === 0 ? [] : hasBinary("sudo") ? ["sudo"] : null;
  if (!installerPrefix) {
    throw new Error(
      "missing required binary 'WebKitWebDriver'. Re-run with root privileges or install manually: apt install webkit2gtk-driver"
    );
  }

  process.stdout.write("e2e preflight: installing webkit2gtk-driver via apt-get (WebKitWebDriver missing)\n");
  runCommand(installerPrefix[0] || "apt-get", installerPrefix.length ? [...installerPrefix.slice(1), "apt-get", "update"] : ["update"], {
    ...process.env,
    DEBIAN_FRONTEND: "noninteractive"
  });
  runCommand(installerPrefix[0] || "apt-get", installerPrefix.length ? [...installerPrefix.slice(1), "apt-get", "install", "-y", "webkit2gtk-driver"] : ["install", "-y", "webkit2gtk-driver"], {
    ...process.env,
    DEBIAN_FRONTEND: "noninteractive"
  });

  if (!hasBinary("WebKitWebDriver")) {
    throw new Error("webkit2gtk-driver install completed but 'WebKitWebDriver' is still unavailable");
  }

  process.stdout.write("e2e preflight OK: WebKitWebDriver installed\n");
}

try {
  if (!ensureExplicitE2ERun()) {
    process.exit(0);
  }
  ensureTauriDriver();
  maybeInstallWebKitWebDriver();
} catch (error) {
  process.stderr.write(`${String(error?.message || error)}\n`);
  process.exit(1);
}
