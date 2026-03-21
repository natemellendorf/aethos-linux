import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { expect } from "chai";
import { Builder, By, Capabilities, until } from "selenium-webdriver";

const __dirname = fileURLToPath(new URL(".", import.meta.url));
const E2E_DIR = path.resolve(__dirname, "..");
const DESKTOP_DIR = path.resolve(E2E_DIR, "..");
const TAURI_BIN = path.resolve(DESKTOP_DIR, "src-tauri", "target", "debug", "aethos");
const RUN_ID = process.env.AETHOS_E2E_RUN_ID || `run-${Date.now()}`;
const TEST_CASE_ID = process.env.AETHOS_E2E_TEST_CASE_ID || "dual-instance-gossip";
const SCENARIO = process.env.AETHOS_E2E_SCENARIO || "clean";
const RELAY_ENDPOINT = process.env.AETHOS_E2E_RELAY_ENDPOINT || "";
const GOSSIP_BASE_PORT = Number(process.env.AETHOS_E2E_GOSSIP_BASE_PORT || 58655);
const GOSSIP_PORT_A = GOSSIP_BASE_PORT;
const GOSSIP_PORT_B = GOSSIP_BASE_PORT + 1;
const ARTIFACT_ROOT = process.env.AETHOS_E2E_ARTIFACT_DIR
  ? path.resolve(process.env.AETHOS_E2E_ARTIFACT_DIR)
  : path.resolve(DESKTOP_DIR, "e2e", "artifacts", RUN_ID);

const TAURI_DRIVER_A_PORT = 4444;
const TAURI_DRIVER_B_PORT = 4454;
const TAURI_NATIVE_A_PORT = 4445;
const TAURI_NATIVE_B_PORT = 4455;

const tauriDriverProcs = [];
let cleanupTriggered = false;
const cleanupFns = [];

function stateRootPath(name) {
  return path.join(os.tmpdir(), `aethos-e2e-${name}-${Date.now()}-${Math.random().toString(16).slice(2, 10)}`);
}

function appLogPath(stateRoot) {
  return path.join(stateRoot, "aethos-linux", "aethos-linux.log");
}

function normalizedIdText(value) {
  return String(value || "").replace(/\s+/g, "").trim().toLowerCase();
}

async function readLogTail(filePath, maxChars = 10000) {
  try {
    const raw = await fs.readFile(filePath, "utf8");
    return raw.length > maxChars ? raw.slice(raw.length - maxChars) : raw;
  } catch {
    return "";
  }
}

async function writeJsonArtifact(fileName, payload) {
  await fs.mkdir(ARTIFACT_ROOT, { recursive: true });
  await fs.writeFile(path.join(ARTIFACT_ROOT, fileName), `${JSON.stringify(payload, null, 2)}\n`, "utf8");
}

async function waitFor(predicate, timeoutMs = 30000, intervalMs = 250) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const ok = await predicate();
    if (ok) return true;
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  return false;
}

async function clickElement(driver, selector, timeoutMs = 20000) {
  const element = await driver.wait(until.elementLocated(By.css(selector)), timeoutMs);
  await driver.wait(until.elementIsVisible(element), timeoutMs);
  await driver.executeScript("arguments[0].scrollIntoView({block: 'center', inline: 'center'});", element);
  try {
    await element.click();
  } catch {
    await driver.executeScript("arguments[0].click();", element);
  }
}

async function waitForSplashToClear(driver) {
  await waitFor(async () => {
    try {
      const overlays = await driver.findElements(By.css(".fixed.inset-0.z-50"));
      if (!overlays.length) return true;
      for (const overlay of overlays) {
        if (await overlay.isDisplayed()) {
          return false;
        }
      }
      return true;
    } catch {
      return false;
    }
  }, 30000, 300);
}

async function clickTab(driver, tabId) {
  await clickElement(driver, `[data-testid=\"tab-${tabId}\"]`);
}

async function openContactsAndAdd(driver, id, alias) {
  await clickTab(driver, "contacts");
  const idInput = await driver.findElement(By.css("[data-testid='contact-wayfarer-id']"));
  const aliasInput = await driver.findElement(By.css("[data-testid='contact-alias']"));
  await idInput.clear();
  await idInput.sendKeys(id);
  await aliasInput.clear();
  await aliasInput.sendKeys(alias);
  const saveBtn = await driver.findElement(By.css("[data-testid='contact-save']"));
  await driver.executeScript("arguments[0].scrollIntoView({block: 'center'});", saveBtn);
  try {
    await saveBtn.click();
  } catch {
    await driver.executeScript("arguments[0].click();", saveBtn);
  }
}

async function sendChatMessage(driver, text) {
  await clickTab(driver, "chats");
  const composer = await driver.findElement(By.css("[data-testid='chat-composer']"));
  await composer.clear();
  await composer.sendKeys(text);
  const sendBtn = await driver.findElement(By.css("[data-testid='chat-send']"));
  try {
    await sendBtn.click();
  } catch {
    await driver.executeScript("arguments[0].click();", sendBtn);
  }
}

async function clickContactInChats(driver, wayfarerId) {
  await clickTab(driver, "chats");
  const selector = `[data-testid='chat-contact-${wayfarerId.toLowerCase()}']`;
  await clickElement(driver, selector, 30000);
}

async function announceGossip(driver) {
  await clickTab(driver, "settings");
  await clickElement(driver, "[data-testid='settings-announce-gossip']");
}

async function configureSyncSettings(driver, { relaySyncEnabled, gossipSyncEnabled, verboseLoggingEnabled }) {
  await clickTab(driver, "settings");

  const setCheckbox = async (testId, enabled) => {
    const el = await driver.findElement(By.css(`[data-testid='${testId}']`));
    const current = await el.isSelected();
    if (current !== enabled) {
      try {
        await el.click();
      } catch {
        await driver.executeScript("arguments[0].click();", el);
      }
    }
  };

  await setCheckbox("settings-relay-sync", relaySyncEnabled);
  await setCheckbox("settings-gossip-sync", gossipSyncEnabled);
  await setCheckbox("settings-verbose-logging", verboseLoggingEnabled);
  if (RELAY_ENDPOINT) {
    const relayEndpoints = await driver.findElement(By.css("[data-testid='settings-relay-endpoints']"));
    await relayEndpoints.clear();
    await relayEndpoints.sendKeys(RELAY_ENDPOINT);
  }
  await clickElement(driver, "[data-testid='settings-save']");
}

async function getOwnWayfarerId(driver) {
  await clickTab(driver, "share");
  const pre = await driver.wait(until.elementLocated(By.css("[data-testid='share-wayfarer-id']")), 20000);
  const text = await pre.getText();
  const id = normalizedIdText(text);
  expect(id).to.match(/^[0-9a-f]{64}$/);
  return id;
}

async function openTauriSession(sessionName, stateRoot, tauriPort = 4444) {
  await fs.mkdir(stateRoot, { recursive: true });
  const env = {
    ...process.env,
    TAURI_AUTOMATION: "true",
    TAURI_WEBVIEW_AUTOMATION: "true",
    AETHOS_STATE_DIR: stateRoot,
    XDG_DATA_HOME: stateRoot,
    XDG_STATE_HOME: stateRoot,
    AETHOS_DISABLE_LAN_TCP: "1",
    AETHOS_GOSSIP_LAN_PORT: String(sessionName === "a" ? GOSSIP_PORT_A : GOSSIP_PORT_B),
    AETHOS_GOSSIP_PEER_PORTS: `${GOSSIP_PORT_A},${GOSSIP_PORT_B}`,
    AETHOS_GOSSIP_LOCALHOST_FANOUT: "1",
    AETHOS_GOSSIP_EAGER_UNICAST: "1",
    AETHOS_GOSSIP_LOOPBACK_ONLY: "1",
    AETHOS_E2E_INSTANCE: sessionName
  };

  const capabilities = new Capabilities();
  capabilities.setBrowserName("wry");
  capabilities.set("tauri:options", {
    application: TAURI_BIN,
    args: [
      `--aethos-state-dir=${stateRoot}`,
      `--aethos-gossip-lan-port=${sessionName === "a" ? GOSSIP_PORT_A : GOSSIP_PORT_B}`,
      `--aethos-gossip-peer-ports=${GOSSIP_PORT_A},${GOSSIP_PORT_B}`,
      "--aethos-disable-lan-tcp=1",
      "--aethos-gossip-localhost-fanout=1",
      "--aethos-gossip-eager-unicast=1",
      "--aethos-gossip-loopback-only=1"
    ],
    env
  });

  const driver = await new Builder()
    .usingServer(`http://127.0.0.1:${tauriPort}/`)
    .withCapabilities(capabilities)
    .build();

  cleanupFns.push(async () => {
    try {
      await driver.quit();
    } catch {
      // ignore teardown errors
    }
  });

  return {
    driver,
    stateRoot,
    logPath: appLogPath(stateRoot)
  };
}

async function startTauriDriver(port, nativePort) {
  const child = spawn(
    "tauri-driver",
    ["--port", String(port), "--native-port", String(nativePort)],
    {
      stdio: ["ignore", "inherit", "inherit"],
      env: process.env
    }
  );
  tauriDriverProcs.push(child);
  cleanupFns.push(async () => {
    if (!child.killed) {
      child.kill("SIGTERM");
    }
  });
}

async function shutdownAll() {
  if (cleanupTriggered) return;
  cleanupTriggered = true;
  while (cleanupFns.length > 0) {
    const fn = cleanupFns.pop();
    try {
      await fn();
    } catch {
      // ignore teardown errors
    }
  }
}

async function captureScreenshot(driver, fileName) {
  try {
    await fs.mkdir(ARTIFACT_ROOT, { recursive: true });
    const image = await driver.takeScreenshot();
    await fs.writeFile(path.join(ARTIFACT_ROOT, fileName), image, "base64");
  } catch {
    // best effort
  }
}

before(async function () {
  this.timeout(240000);

  const buildResult = spawnSync("npx", ["tauri", "build", "--debug", "--no-bundle"], {
    cwd: DESKTOP_DIR,
    stdio: "inherit",
    shell: true
  });
  if (buildResult.status !== 0) {
    throw new Error("tauri build failed");
  }

  await startTauriDriver(TAURI_DRIVER_A_PORT, TAURI_NATIVE_A_PORT);
  await startTauriDriver(TAURI_DRIVER_B_PORT, TAURI_NATIVE_B_PORT);

  await new Promise((resolve) => setTimeout(resolve, 1200));
});

after(async function () {
  await shutdownAll();
});

process.on("SIGINT", async () => {
  await shutdownAll();
  process.exit(130);
});

process.on("SIGTERM", async () => {
  await shutdownAll();
  process.exit(143);
});

describe("dual instance gossip e2e", function () {
  it("sends message between two isolated desktop instances and writes logs", async function () {
    this.timeout(300000);

    const a = await openTauriSession("a", stateRootPath("a"), TAURI_DRIVER_A_PORT);
    const b = await openTauriSession("b", stateRootPath("b"), TAURI_DRIVER_B_PORT);
    await writeJsonArtifact("run-index.json", {
      run_id: RUN_ID,
      test_case_id: TEST_CASE_ID,
      scenario: SCENARIO,
      started_at_unix_ms: Date.now(),
      topology: {
        nodes: [
          { label: "wayfarer-1", state_dir: a.stateRoot, tauri_driver_port: TAURI_DRIVER_A_PORT },
          { label: "wayfarer-2", state_dir: b.stateRoot, tauri_driver_port: TAURI_DRIVER_B_PORT }
        ]
      },
      artifacts: {
        instance_a_log: a.logPath,
        instance_b_log: b.logPath
      }
    });

    await waitForSplashToClear(a.driver);
    await waitForSplashToClear(b.driver);

    await configureSyncSettings(a.driver, {
      relaySyncEnabled: false,
      gossipSyncEnabled: true,
      verboseLoggingEnabled: true
    });
    await configureSyncSettings(b.driver, {
      relaySyncEnabled: false,
      gossipSyncEnabled: true,
      verboseLoggingEnabled: true
    });

    const idA = await getOwnWayfarerId(a.driver);
    const idB = await getOwnWayfarerId(b.driver);
    expect(idA).to.not.equal(idB);

    await openContactsAndAdd(a.driver, idB, "Peer B");
    await openContactsAndAdd(b.driver, idA, "Peer A");

    await clickContactInChats(a.driver, idB);
    await clickContactInChats(b.driver, idA);

    await announceGossip(a.driver);
    await announceGossip(b.driver);

    const msg = `e2e-${Date.now()}`;
    await sendChatMessage(a.driver, msg);

    const inboundVisible = await waitFor(async () => {
      await announceGossip(a.driver);
      await announceGossip(b.driver);
      await clickContactInChats(b.driver, idA);
      const threadText = await (await b.driver.findElement(By.css("body"))).getText();
      return threadText.includes(msg);
    }, 120000, 900);

    if (!inboundVisible) {
      await captureScreenshot(a.driver, "wayfarer-1-failure.png");
      await captureScreenshot(b.driver, "wayfarer-2-failure.png");
      const logA = await readLogTail(a.logPath);
      const logB = await readLogTail(b.logPath);
      await writeJsonArtifact("failure-summary.json", {
        run_id: RUN_ID,
        test_case_id: TEST_CASE_ID,
        scenario: SCENARIO,
        failed_at_unix_ms: Date.now(),
        failure: "message did not converge within timeout",
        logs: {
          wayfarer_1_tail: logA,
          wayfarer_2_tail: logB
        }
      });
      throw new Error(
        `message did not converge within timeout\n` +
          `instanceA log: ${a.logPath}\n${logA}\n` +
          `instanceB log: ${b.logPath}\n${logB}`
      );
    }

    await clickTab(a.driver, "settings");
    await clickTab(b.driver, "settings");
    const logPathTextA = await (await a.driver.findElement(By.css("[data-testid='settings-log-path']"))).getText();
    const logPathTextB = await (await b.driver.findElement(By.css("[data-testid='settings-log-path']"))).getText();

    expect(path.resolve(logPathTextA)).to.equal(path.resolve(a.logPath));
    expect(path.resolve(logPathTextB)).to.equal(path.resolve(b.logPath));

    const statusA = await (await a.driver.findElement(By.css("[data-testid='status-text']"))).getText();
    const statusB = await (await b.driver.findElement(By.css("[data-testid='status-text']"))).getText();
    expect(statusA.length).to.be.greaterThan(0);
    expect(statusB.length).to.be.greaterThan(0);

    await writeJsonArtifact("run-result.json", {
      run_id: RUN_ID,
      test_case_id: TEST_CASE_ID,
      scenario: SCENARIO,
      completed_at_unix_ms: Date.now(),
      status: "passed",
      node_status: {
        wayfarer_1: statusA,
        wayfarer_2: statusB
      }
    });
  });
});
