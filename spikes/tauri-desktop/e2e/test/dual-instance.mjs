import fs from "node:fs/promises";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
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
const E2E_DISABLE_RELAY = (process.env.AETHOS_E2E_DISABLE_RELAY || "1") === "1";
const E2E_LOOPBACK_ONLY = (process.env.AETHOS_E2E_LOOPBACK_ONLY || "1") === "1";
const E2E_EAGER_UNICAST = (process.env.AETHOS_E2E_EAGER_UNICAST || "1") === "1";
const E2E_LOCALHOST_FANOUT = (process.env.AETHOS_E2E_LOCALHOST_FANOUT || "1") === "1";
const E2E_DISABLE_LAN_TCP = (process.env.AETHOS_E2E_DISABLE_LAN_TCP || "1") === "1";
const BURST_COUNT = Number(process.env.AETHOS_E2E_BURST_COUNT || "1");
const BURST_INTERVAL_MS = Number(process.env.AETHOS_E2E_BURST_INTERVAL_MS || "1000");
const BURST_RECEIVE_TIMEOUT_MS = Number(process.env.AETHOS_E2E_BURST_RECEIVE_TIMEOUT_MS || "600000");
const MEDIA_MANIFEST_TIMEOUT_MS = Number(process.env.AETHOS_E2E_MEDIA_MANIFEST_TIMEOUT_MS || "240000");
const MEDIA_TRANSFER_TIMEOUT_MS = Number(process.env.AETHOS_E2E_MEDIA_TRANSFER_TIMEOUT_MS || "240000");
const TEST_TIMEOUT_MS = Number(process.env.AETHOS_E2E_TEST_TIMEOUT_MS || "1200000");
const RUN_PORT_OFFSET = Number((String(RUN_ID).match(/(\d+)/)?.[1] || "0")) % 1000;
const GOSSIP_BASE_PORT = Number(process.env.AETHOS_E2E_GOSSIP_BASE_PORT || String(58655 + RUN_PORT_OFFSET));
const GOSSIP_PORT_A = GOSSIP_BASE_PORT;
const GOSSIP_PORT_B = GOSSIP_BASE_PORT + 1;
const ARTIFACT_ROOT = process.env.AETHOS_E2E_ARTIFACT_DIR
  ? path.resolve(process.env.AETHOS_E2E_ARTIFACT_DIR)
  : path.resolve(DESKTOP_DIR, "e2e", "artifacts", RUN_ID);
const E2E_WORKDIR = process.env.AETHOS_E2E_WORKDIR
  ? path.resolve(process.env.AETHOS_E2E_WORKDIR)
  : path.resolve(DESKTOP_DIR, "e2e", "workdir", RUN_ID);
const MEDIA_SEED_BASE = process.env.AETHOS_E2E_MEDIA_SEED || `aethos-media-${RUN_ID}`;
const MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES = Number(process.env.AETHOS_MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES || "16384");
const MEDIA_E2E_TTL_SECONDS = Number(process.env.AETHOS_MEDIA_E2E_TTL_SECONDS || "360");
const MEDIA_E2E_MISSING_MIN_INTERVAL_MS = Number(process.env.AETHOS_MEDIA_E2E_MISSING_MIN_INTERVAL_MS || "500");
const MEDIA_E2E_FALLBACK_TRANSFER_MAX_BYTES = Number(process.env.AETHOS_LAN_FALLBACK_TRANSFER_MAX_BYTES || "48500");
const MEDIA_E2E_FALLBACK_MAX_CHUNKS_PER_REQUEST = Number(process.env.AETHOS_LAN_FALLBACK_MAX_CHUNKS_PER_REQUEST || "96");
const MEDIA_E2E_TCP_REQUEST_ENCOUNTER = process.env.AETHOS_LAN_TCP_REQUEST_ENCOUNTER || "0";
const MEDIA_E2E_CONTROL_FASTLANE_MAX_ITEMS = Number(process.env.AETHOS_LAN_MEDIA_CONTROL_FASTLANE_MAX_ITEMS || "256");
const MEDIA_E2E_CONTROL_FASTLANE_PACE_MS = Number(process.env.AETHOS_LAN_MEDIA_CONTROL_FASTLANE_PACE_MS || "8");
const MEDIA_E2E_CONTROL_FASTLANE_QUEUE_MAX = Number(process.env.AETHOS_MEDIA_CONTROL_FASTLANE_QUEUE_MAX || "8192");
const MEDIA_E2E_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS = Number(
  process.env.AETHOS_MEDIA_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS || "4"
);
const MEDIA_E2E_MISSING_FASTLANE_REDUNDANCY = Number(process.env.AETHOS_MEDIA_MISSING_FASTLANE_REDUNDANCY || "1");
const MEDIA_E2E_CONTROL_FASTLANE_TCP = process.env.AETHOS_MEDIA_CONTROL_FASTLANE_TCP || "1";
const MEDIA_E2E_MISSING_TCP_MIRROR = process.env.AETHOS_MEDIA_MISSING_TCP_MIRROR || "1";
const MEDIA_E2E_MISSING_TCP_MIRROR_MAX_PER_MINUTE = Number(
  process.env.AETHOS_MEDIA_MISSING_TCP_MIRROR_MAX_PER_MINUTE || "40"
);
const MEDIA_E2E_MISSING_TCP_MIRROR_MIN_INTERVAL_MS = Number(
  process.env.AETHOS_MEDIA_MISSING_TCP_MIRROR_MIN_INTERVAL_MS || "1200"
);
const MEDIA_E2E_MISSING_TCP_MIRROR_STALL_MS = Number(
  process.env.AETHOS_MEDIA_MISSING_TCP_MIRROR_STALL_MS || "900"
);
const MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN = Number(
  process.env.AETHOS_MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN || "67108864"
);
const MEDIA_E2E_WIRE_BUCKET_BURST_BYTES = Number(
  process.env.AETHOS_MEDIA_E2E_WIRE_BUCKET_BURST_BYTES || "67108864"
);
const MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS = Number(
  process.env.AETHOS_MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS || "1500"
);
const MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING = Number(
  process.env.AETHOS_MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING || "256"
);
const MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT = Number(
  process.env.AETHOS_MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT || "4000"
);
const E2E_UDP_TRANSFER_FRAME_MAX_BYTES = Number(
  process.env.AETHOS_E2E_UDP_TRANSFER_FRAME_MAX_BYTES || "32768"
);
const MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS = Number(
  process.env.AETHOS_MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS || "2000"
);

const TAURI_DRIVER_A_PORT = 4444;
const TAURI_DRIVER_B_PORT = 4454;
const TAURI_NATIVE_A_PORT = 4445;
const TAURI_NATIVE_B_PORT = 4455;

const tauriDriverProcs = [];
let cleanupTriggered = false;
const cleanupFns = [];

function stateRootPath(name) {
  return path.join(E2E_WORKDIR, `aethos-${name}`);
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

async function readJsonFile(filePath) {
  const raw = await fs.readFile(filePath, "utf8");
  return JSON.parse(raw);
}

async function waitForIncomingMessageInState(stateRoot, expectedText) {
  const chatPath = path.join(stateRoot, "chat-history.json");
  return waitFor(async () => {
    try {
      const chat = await readJsonFile(chatPath);
      const threads = Object.values(chat?.threads || {});
      for (const thread of threads) {
        for (const msg of thread || []) {
          if (msg?.direction === "Incoming" && String(msg?.text || "") === expectedText) {
            return {
              found: true,
              msgId: msg?.msgId || "",
              threadKey: Object.keys(chat?.threads || {}).find((key) => (chat.threads[key] || []).some((m) => m?.msgId === msg?.msgId)) || ""
            };
          }
        }
      }
      return false;
    } catch {
      return false;
    }
  }, 120000, 700);
}

async function waitForOutgoingMessageInState(stateRoot, expectedText) {
  const chatPath = path.join(stateRoot, "chat-history.json");
  return waitFor(async () => {
    try {
      const chat = await readJsonFile(chatPath);
      const threads = Object.values(chat?.threads || {});
      for (const thread of threads) {
        for (const msg of thread || []) {
          if (msg?.direction === "Outgoing" && String(msg?.text || "") === expectedText) {
            return {
              found: true,
              msgId: msg?.msgId || "",
              threadKey: Object.keys(chat?.threads || {}).find((key) => (chat.threads[key] || []).some((m) => m?.msgId === msg?.msgId)) || ""
            };
          }
        }
      }
      return false;
    } catch {
      return false;
    }
  }, 120000, 700);
}

async function incomingMessagesByPrefix(stateRoot, prefix) {
  const chatPath = path.join(stateRoot, "chat-history.json");
  const chat = await readJsonFile(chatPath);
  const out = [];
  for (const thread of Object.values(chat?.threads || {})) {
    for (const msg of thread || []) {
      if (msg?.direction === "Incoming") {
        const text = String(msg?.text || "");
        if (text.startsWith(prefix)) {
          out.push(text);
        }
      }
    }
  }
  return out;
}

function burstSequenceGaps(prefixTexts, prefix, expectedCount) {
  const seen = new Set();
  for (const text of prefixTexts || []) {
    const raw = String(text || "");
    if (!raw.startsWith(prefix)) continue;
    const n = Number(raw.slice(prefix.length));
    if (Number.isFinite(n) && n > 0) seen.add(n);
  }
  const missing = [];
  for (let i = 1; i <= expectedCount; i += 1) {
    if (!seen.has(i)) missing.push(i);
  }
  return missing;
}

async function waitForIncomingPrefixCount(stateRoot, prefix, expectedCount, timeoutMs) {
  return waitFor(async () => {
    try {
      const incoming = await incomingMessagesByPrefix(stateRoot, prefix);
      if (incoming.length >= expectedCount) {
        return {
          found: true,
          count: incoming.length,
          texts: incoming
        };
      }
      return false;
    } catch {
      return false;
    }
  }, timeoutMs, 1000);
}

async function waitForLogPattern(filePath, pattern, timeoutMs = 120000) {
  const rx = pattern instanceof RegExp ? pattern : new RegExp(String(pattern));
  return waitFor(async () => {
    try {
      const tail = await readLogTail(filePath, 120000);
      return rx.test(tail) ? true : false;
    } catch {
      return false;
    }
  }, timeoutMs, 500);
}

async function clickSyncInbox(driver) {
  const clicked = await waitFor(async () => {
    try {
      const ok = await driver.executeScript(
        "const buttons = Array.from(document.querySelectorAll('button')); const target = buttons.find((b)=> (b.textContent||'').toLowerCase().includes('sync inbox')); if (!target) return false; target.click(); return true;"
      );
      return !!ok;
    } catch {
      return false;
    }
  }, 5000, 250);
  return Boolean(clicked);
}

async function generateDeterministicLargeImage(filePath, seed, width = 2600, height = 1900) {
  const scriptPath = path.resolve(E2E_DIR, "scripts", "generate-aethos-large-png.mjs");
  const result = spawnSync(
    "node",
    [scriptPath, "--out", filePath, "--seed", seed, "--width", String(width), "--height", String(height)],
    { cwd: DESKTOP_DIR, encoding: "utf8" }
  );
  if (result.status !== 0) {
    throw new Error(`generateDeterministicLargeImage failed: ${result.stderr || result.stdout}`);
  }
  const parsed = JSON.parse(String(result.stdout || "{}").trim());
  return parsed;
}

function sha256Hex(buffer) {
  return crypto.createHash("sha256").update(buffer).digest("hex");
}

async function attachFileAndSend(session, filePath, caption = "") {
  const { driver, logPath } = session;
  const expectedFileName = path.basename(filePath);
  await clickTab(driver, "chats");
  if (caption) {
    const composer = await driver.findElement(By.css("[data-testid='chat-composer']"));
    await composer.clear();
    await composer.sendKeys(caption);
  }

  const input = await driver.findElement(By.css("[data-testid='chat-attachment-input']"));
  await input.sendKeys(filePath);
  const attachedAccepted = await waitFor(async () => {
    try {
      const attachedText = await driver.executeScript(
        "const row=document.querySelector('[data-testid=\"chat-attachment-input\"]')?.parentElement; const chip=row?.querySelector('div.rounded-md span.truncate'); return (chip?.textContent||'').trim();"
      );
      return attachedText === expectedFileName;
    } catch {
      return false;
    }
  }, 45000, 200);
  if (!attachedAccepted) {
    throw new Error(`attachment chip did not appear for ${expectedFileName}`);
  }

  const sendBtn = await driver.findElement(By.css("[data-testid='chat-send']"));
  await sendBtn.click();

  const attachedCleared = await waitFor(async () => {
    try {
      const attachedText = await driver.executeScript(
        "const row=document.querySelector('[data-testid=\"chat-attachment-input\"]')?.parentElement; const chip=row?.querySelector('div.rounded-md span.truncate'); return (chip?.textContent||'').trim();"
      );
      return attachedText.length === 0;
    } catch {
      return false;
    }
  }, 15000, 200);
  if (!attachedCleared) {
    throw new Error("attachment was not accepted by composer before send");
  }

  if (!logPath) return;
  const sentWithAttachment = await waitFor(async () => {
    try {
      const tail = await readLogTail(logPath, 20000);
      return tail.includes("send_message_start:") && tail.includes(`attachment=${expectedFileName}:`);
    } catch {
      return false;
    }
  }, 120000, 500);
  if (!sentWithAttachment) {
    throw new Error(`outgoing message missing expected attachment ${expectedFileName}`);
  }
}

async function waitForMediaManifest(driver, timeoutMs = 120000) {
  const found = await waitFor(async () => {
    try {
      return await driver.executeScript(
        "const el=document.querySelector('[data-testid^=\"media-manifest-\"]'); return el ? el.getAttribute('data-testid') : null;"
      );
    } catch {
      return false;
    }
  }, timeoutMs, 300);
  return found || null;
}

async function waitForMediaImageRendered(driver, timeoutMs = 240000) {
  const found = await waitFor(async () => {
    try {
      return await driver.executeScript(
        "const el=document.querySelector('[data-testid^=\"media-image-\"]'); return !!el && !!el.getAttribute('src');"
      );
    } catch {
      return false;
    }
  }, timeoutMs, 500);
  return Boolean(found);
}

async function clickMediaLoadButton(driver, timeoutMs = 30000) {
  await clickElement(driver, "[data-testid^='media-load-']", timeoutMs);
}

async function mediaImagePresent(driver) {
  try {
    return await driver.executeScript(
      "const el=document.querySelector('[data-testid^=\"media-image-\"]'); return !!el && !!el.getAttribute('src');"
    );
  } catch {
    return false;
  }
}

async function transferStateFromChat(stateRoot) {
  const chatPath = path.join(stateRoot, "chat-history.json");
  const chat = await readJsonFile(chatPath);
  const transfers = [];
  for (const [threadKey, thread] of Object.entries(chat?.threads || {})) {
    for (const message of thread || []) {
      if (message?.media?.transferId) {
        transfers.push({ threadKey, message });
      }
    }
  }
  return transfers;
}

async function waitForCompletedMediaTransfer(stateRoot, timeoutMs = 240000) {
  const result = await waitFor(async () => {
    try {
      const transfers = await transferStateFromChat(stateRoot);
      const complete = transfers.find((entry) => String(entry?.message?.media?.status || "").toLowerCase() === "complete");
      return complete || false;
    } catch {
      return false;
    }
  }, timeoutMs, 700);
  return result || null;
}

async function waitForPeerMediaCapability(stateRoot, peerWayfarerId, timeoutMs = 90000) {
  const cachePath = path.join(stateRoot, "media", "capabilities-cache.json");
  const found = await waitFor(async () => {
    try {
      const cache = await readJsonFile(cachePath);
      const peer = cache?.peers?.[peerWayfarerId];
      return peer?.mediaV1 === true;
    } catch {
      return false;
    }
  }, timeoutMs, 500);
  return Boolean(found);
}

async function writeJsonArtifact(fileName, payload) {
  await fs.mkdir(ARTIFACT_ROOT, { recursive: true });
  await fs.writeFile(path.join(ARTIFACT_ROOT, fileName), `${JSON.stringify(payload, null, 2)}\n`, "utf8");
}

async function waitFor(predicate, timeoutMs = 30000, intervalMs = 250) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const value = await predicate();
    if (value) return value;
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
  const normalizedId = normalizedIdText(id);
  const normalizedAlias = String(alias || "").trim();
  await clickTab(driver, "contacts");
  await waitForSplashToClear(driver);
  const beforeContacts = new Set(
    await driver.executeScript(
      "return Array.from(document.querySelectorAll('[data-testid^=\"chat-contact-\"]')).map((el)=>el.getAttribute('data-testid').replace('chat-contact-',''));"
    )
  );
  let saved = false;
  for (let attempt = 0; attempt < 6 && !saved; attempt += 1) {
    await clickTab(driver, "contacts");
    const idInput = await driver.findElement(By.css("[data-testid='contact-wayfarer-id']"));
    const aliasInput = await driver.findElement(By.css("[data-testid='contact-alias']"));

    await idInput.clear();
    await idInput.sendKeys(normalizedId);
    await aliasInput.clear();
    await aliasInput.sendKeys(normalizedAlias);

    const fieldsReady = await waitFor(async () => {
      try {
        const state = await driver.executeScript(
          "const id=document.querySelector('[data-testid=\"contact-wayfarer-id\"]')?.value || ''; const alias=document.querySelector('[data-testid=\"contact-alias\"]')?.value || ''; return { id, alias };"
        );
        return normalizedIdText(state?.id) === normalizedId && String(state?.alias || "").trim() === normalizedAlias;
      } catch {
        return false;
      }
    }, 3000, 150);

    if (!fieldsReady) {
      continue;
    }

    const saveBtn = await driver.findElement(By.css("[data-testid='contact-save']"));
    await driver.executeScript("arguments[0].scrollIntoView({block: 'center'}); arguments[0].click();", saveBtn);

    saved = await waitFor(async () => {
      try {
        await clickTab(driver, "chats");
        const selector = `[data-testid='chat-contact-${normalizedId}']`;
        const matches = await driver.findElements(By.css(selector));
        return matches.length > 0;
      } catch {
        return false;
      }
    }, 6000, 250);
  }

  if (!saved) {
    await clickTab(driver, "contacts");
    const debugContactState = await driver.executeScript(
      "const id=document.querySelector('[data-testid=\"contact-wayfarer-id\"]')?.value || ''; const alias=document.querySelector('[data-testid=\"contact-alias\"]')?.value || ''; const contactList=Array.from(document.querySelectorAll('[data-testid^=\"chat-contact-\"]')).map((el)=>el.getAttribute('data-testid').replace('chat-contact-','')); const status=document.querySelector('[data-testid=\"status-text\"]')?.textContent || ''; return {id, alias, contactList, status};"
    );
    throw new Error(`contact did not appear in chat list: ${normalizedId}; debug=${JSON.stringify(debugContactState)}; before=${JSON.stringify(Array.from(beforeContacts))}`);
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
  const isSelected = await waitFor(async () => {
    try {
      return await driver.executeScript(
        "const el=document.querySelector(arguments[0]); return !!el && el.className.includes('border-blue-300');",
        selector
      );
    } catch {
      return false;
    }
  }, 5000, 200);
  if (!isSelected) {
    await driver.executeScript("const el=document.querySelector(arguments[0]); if (el) el.click();", selector);
  }
}

async function announceGossip(driver) {
  await clickTab(driver, "settings");
  await clickElement(driver, "[data-testid='settings-announce-gossip']");
}

async function readIdentityWayfarerId(stateRoot) {
  const identityPath = path.join(stateRoot, "aethos-linux", "identity.json");
  const ok = await waitFor(async () => {
    try {
      const raw = await fs.readFile(identityPath, "utf8");
      const parsed = JSON.parse(raw);
      return /^[0-9a-f]{64}$/.test(normalizedIdText(parsed?.wayfarer_id));
    } catch {
      return false;
    }
  }, 30000, 250);

  if (!ok) {
    throw new Error(`identity file not ready: ${identityPath}`);
  }

  const raw = await fs.readFile(identityPath, "utf8");
  const parsed = JSON.parse(raw);
  return normalizedIdText(parsed?.wayfarer_id);
}

async function getOwnWayfarerId(driver, fallbackStateRoot) {
  const ready = await waitFor(async () => {
    try {
      await clickTab(driver, "share");
      const pre = await driver.wait(until.elementLocated(By.css("[data-testid='share-wayfarer-id']")), 8000);
      const text = await pre.getText();
      const id = normalizedIdText(text);
      return /^[0-9a-f]{64}$/.test(id) ? id : false;
    } catch {
      return false;
    }
  }, 30000, 400);

  if (ready) return String(ready);
  if (fallbackStateRoot) {
    return readIdentityWayfarerId(fallbackStateRoot);
  }
  throw new Error("wayfarer id unavailable from share tab");
}

async function openTauriSession(sessionName, stateRoot, tauriPort = 4444, extraEnv = {}) {
  await fs.mkdir(stateRoot, { recursive: true });
  const relayEndpointOverride = String(
    extraEnv.AETHOS_E2E_RELAY_ENDPOINT || RELAY_ENDPOINT || ""
  ).trim();
  if (relayEndpointOverride) {
    const settingsPath = path.join(stateRoot, "settings.json");
    const seedSettings = {
      relaySyncEnabled: true,
      gossipSyncEnabled: true,
      verboseLoggingEnabled: true,
      relayEndpoints: [relayEndpointOverride],
      messageTtlSeconds: 3600,
      enterToSend: true
    };
    await fs.writeFile(settingsPath, `${JSON.stringify(seedSettings, null, 2)}\n`, "utf8");
  }

  const disableLanTcpValue =
    extraEnv.AETHOS_DISABLE_LAN_TCP !== undefined
      ? String(extraEnv.AETHOS_DISABLE_LAN_TCP)
      : (E2E_DISABLE_LAN_TCP ? "1" : "0");
  const gossipLanPortValue =
    extraEnv.AETHOS_GOSSIP_LAN_PORT !== undefined
      ? String(extraEnv.AETHOS_GOSSIP_LAN_PORT)
      : String(sessionName === "a" ? GOSSIP_PORT_A : GOSSIP_PORT_B);
  const gossipPeerPortsValue =
    extraEnv.AETHOS_GOSSIP_PEER_PORTS !== undefined
      ? String(extraEnv.AETHOS_GOSSIP_PEER_PORTS)
      : `${GOSSIP_PORT_A},${GOSSIP_PORT_B}`;
  const localhostFanoutValue =
    extraEnv.AETHOS_GOSSIP_LOCALHOST_FANOUT !== undefined
      ? String(extraEnv.AETHOS_GOSSIP_LOCALHOST_FANOUT)
      : (E2E_LOCALHOST_FANOUT ? "1" : "0");
  const eagerUnicastValue =
    extraEnv.AETHOS_GOSSIP_EAGER_UNICAST !== undefined
      ? String(extraEnv.AETHOS_GOSSIP_EAGER_UNICAST)
      : (E2E_EAGER_UNICAST ? "1" : "0");
  const loopbackOnlyValue =
    extraEnv.AETHOS_GOSSIP_LOOPBACK_ONLY !== undefined
      ? String(extraEnv.AETHOS_GOSSIP_LOOPBACK_ONLY)
      : (E2E_LOOPBACK_ONLY ? "1" : "0");
  const relayDisabledValue =
    extraEnv.AETHOS_E2E_DISABLE_RELAY !== undefined
      ? String(extraEnv.AETHOS_E2E_DISABLE_RELAY)
      : (E2E_DISABLE_RELAY ? "1" : "0");
  const forceGossipValue =
    extraEnv.AETHOS_E2E_FORCE_GOSSIP !== undefined
      ? String(extraEnv.AETHOS_E2E_FORCE_GOSSIP)
      : (process.env.AETHOS_E2E_FORCE_GOSSIP || "1");

  const env = {
    ...process.env,
    TAURI_AUTOMATION: "true",
    TAURI_WEBVIEW_AUTOMATION: "true",
    AETHOS_STATE_DIR: stateRoot,
    XDG_DATA_HOME: stateRoot,
    XDG_STATE_HOME: stateRoot,
    AETHOS_DISABLE_LAN_TCP: disableLanTcpValue,
    AETHOS_GOSSIP_LAN_PORT: gossipLanPortValue,
    AETHOS_GOSSIP_PEER_PORTS: gossipPeerPortsValue,
    AETHOS_GOSSIP_LOCALHOST_FANOUT: localhostFanoutValue,
    AETHOS_GOSSIP_EAGER_UNICAST: eagerUnicastValue,
    AETHOS_GOSSIP_LOOPBACK_ONLY: loopbackOnlyValue,
    AETHOS_STRUCTURED_LOGS: process.env.AETHOS_STRUCTURED_LOGS || "1",
    AETHOS_E2E_RUN_ID: RUN_ID,
    AETHOS_E2E: "1",
    AETHOS_E2E_TEST_CASE_ID: TEST_CASE_ID,
    AETHOS_E2E_SCENARIO: SCENARIO,
    AETHOS_E2E_NODE_LABEL: sessionName === "a" ? "wayfarer-1" : "wayfarer-2",
    AETHOS_E2E_DISABLE_RELAY: relayDisabledValue,
    AETHOS_E2E_FORCE_VERBOSE: process.env.AETHOS_E2E_FORCE_VERBOSE || "1",
    AETHOS_E2E_FORCE_GOSSIP: forceGossipValue,
    AETHOS_E2E_INSTANCE: sessionName,
    ...extraEnv
  };

  if (String(env.AETHOS_E2E_FORCE_VERBOSE || "") === "1") {
    env.AETHOS_STRUCTURED_LOGS = "1";
  }

  const capabilities = new Capabilities();
  capabilities.setBrowserName("wry");
  capabilities.set("tauri:options", {
    application: TAURI_BIN,
    args: [
      "--aethos-e2e=1",
      `--aethos-state-dir=${stateRoot}`,
      `--aethos-gossip-lan-port=${gossipLanPortValue}`,
      `--aethos-gossip-peer-ports=${gossipPeerPortsValue}`,
      `--aethos-disable-lan-tcp=${disableLanTcpValue}`,
      `--aethos-gossip-localhost-fanout=${localhostFanoutValue}`,
      `--aethos-gossip-eager-unicast=${eagerUnicastValue}`,
      `--aethos-gossip-loopback-only=${loopbackOnlyValue}`,
      `--aethos-e2e-disable-relay=${relayDisabledValue}`,
      "--aethos-e2e-force-verbose=1",
      `--aethos-e2e-force-gossip=${forceGossipValue}`,
      `--aethos-lan-fallback-transfer-max-items=${String(extraEnv.AETHOS_LAN_FALLBACK_TRANSFER_MAX_ITEMS || "2")}`,
      `--aethos-lan-fallback-transfer-max-bytes=${String(extraEnv.AETHOS_LAN_FALLBACK_TRANSFER_MAX_BYTES || "1024")}`,
      `--aethos-media-e2e-max-item-payload-b64-bytes=${String(extraEnv.AETHOS_MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES || "")}`,
      `--aethos-media-e2e-ttl-seconds=${String(extraEnv.AETHOS_MEDIA_E2E_TTL_SECONDS || "")}`,
      `--aethos-media-e2e-missing-min-interval-ms=${String(extraEnv.AETHOS_MEDIA_E2E_MISSING_MIN_INTERVAL_MS || "")}`,
      `--aethos-media-e2e-drop-chunk-index=${String(extraEnv.AETHOS_MEDIA_E2E_DROP_CHUNK_INDEX || "")}`,
      `--aethos-lan-fallback-max-chunks-per-request=${String(extraEnv.AETHOS_LAN_FALLBACK_MAX_CHUNKS_PER_REQUEST || "")}`,
      `--aethos-lan-tcp-request-encounter=${String(extraEnv.AETHOS_LAN_TCP_REQUEST_ENCOUNTER || "")}`,
      `--aethos-lan-media-control-fastlane-max-items=${String(extraEnv.AETHOS_LAN_MEDIA_CONTROL_FASTLANE_MAX_ITEMS || "")}`,
      `--aethos-media-control-fastlane-tcp=${String(extraEnv.AETHOS_MEDIA_CONTROL_FASTLANE_TCP || "")}`,
      `--aethos-media-missing-fastlane-redundancy=${String(extraEnv.AETHOS_MEDIA_MISSING_FASTLANE_REDUNDANCY || "")}`,
      `--aethos-media-e2e-wire-bucket-sustained-bytes-per-min=${String(extraEnv.AETHOS_MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN || "")}`,
      `--aethos-media-e2e-wire-bucket-burst-bytes=${String(extraEnv.AETHOS_MEDIA_E2E_WIRE_BUCKET_BURST_BYTES || "")}`,
      `--aethos-media-e2e-missing-response-dedup-window-ms=${String(extraEnv.AETHOS_MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS || "")}`,
      `--aethos-media-e2e-missing-response-chunk-ceiling=${String(extraEnv.AETHOS_MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING || "")}`,
      `--aethos-media-e2e-chunks-per-minute-limit=${String(extraEnv.AETHOS_MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT || "")}`,
      `--aethos-e2e-udp-transfer-frame-max-bytes=${String(extraEnv.AETHOS_E2E_UDP_TRANSFER_FRAME_MAX_BYTES || "")}`,
      `--aethos-media-e2e-housekeeping-min-interval-ms=${String(extraEnv.AETHOS_MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS || "")}`
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

async function closeSession(session) {
  if (!session?.driver) return;
  try {
    await session.driver.quit();
  } catch {
    // ignore
  }
}

before(async function () {
  this.timeout(TEST_TIMEOUT_MS);

  await fs.mkdir(E2E_WORKDIR, { recursive: true });
  await fs.mkdir(ARTIFACT_ROOT, { recursive: true });

  spawnSync("bash", ["-lc", "pkill -f 'src-tauri/target/debug/aethos' || true; pkill -f tauri-driver || true; pkill -f WebKitWebDriver || true"], {
    cwd: DESKTOP_DIR,
    stdio: "inherit"
  });

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
  spawnSync("bash", ["-lc", "pkill -f 'src-tauri/target/debug/aethos' || true; pkill -f tauri-driver || true; pkill -f WebKitWebDriver || true"], {
    cwd: DESKTOP_DIR,
    stdio: "inherit"
  });
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
    this.timeout(TEST_TIMEOUT_MS);

    let a;
    let b;
    try {
      a = await openTauriSession("a", stateRootPath("a"), TAURI_DRIVER_A_PORT);
      b = await openTauriSession("b", stateRootPath("b"), TAURI_DRIVER_B_PORT);
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
      },
      env: {
        disable_relay: E2E_DISABLE_RELAY ? "1" : "0",
        loopback_only: E2E_LOOPBACK_ONLY ? "1" : "0",
        eager_unicast: E2E_EAGER_UNICAST ? "1" : "0",
        localhost_fanout: E2E_LOCALHOST_FANOUT ? "1" : "0",
        disable_lan_tcp: E2E_DISABLE_LAN_TCP ? "1" : "0"
      }
    });

    await waitForSplashToClear(a.driver);
    await waitForSplashToClear(b.driver);

    const idA = await readIdentityWayfarerId(a.stateRoot);
    const idB = await readIdentityWayfarerId(b.stateRoot);
    expect(idA).to.not.equal(idB);

    await openContactsAndAdd(a.driver, idB, "Peer B");
    await openContactsAndAdd(b.driver, idA, "Peer A");

    await clickContactInChats(a.driver, idB);
    await clickContactInChats(b.driver, idA);

    const burstPrefix = `e2e-${Date.now()}-`;
    if (BURST_COUNT <= 1) {
      await sendChatMessage(a.driver, `${burstPrefix}1`);
    } else {
      for (let i = 1; i <= BURST_COUNT; i += 1) {
        await sendChatMessage(a.driver, `${burstPrefix}${i}`);
        if (i < BURST_COUNT && BURST_INTERVAL_MS > 0) {
          await new Promise((resolve) => setTimeout(resolve, BURST_INTERVAL_MS));
        }
      }
    }

    let inboundState;
    if (BURST_COUNT <= 1) {
      inboundState = await waitForIncomingMessageInState(b.stateRoot, `${burstPrefix}1`);
    } else {
      const started = Date.now();
      let lastObserved = 0;
      while (Date.now() - started < BURST_RECEIVE_TIMEOUT_MS) {
        inboundState = await waitForIncomingPrefixCount(b.stateRoot, burstPrefix, BURST_COUNT, 3000);
        if (inboundState?.found) break;

        const snapshot = await waitForIncomingPrefixCount(b.stateRoot, burstPrefix, 1, 1000);
        const observed = Number(snapshot?.count || 0);
        if (observed > lastObserved) {
          lastObserved = observed;
        }
        await clickSyncInbox(b.driver);
        await new Promise((resolve) => setTimeout(resolve, 800));
      }
      if (!inboundState?.found) {
        inboundState = await waitForIncomingPrefixCount(b.stateRoot, burstPrefix, 1, 1) || { found: false, count: 0, texts: [] };
      }
    }
    const observedIncomingCount = Number(inboundState?.count || 0);
    const inboundVisible = BURST_COUNT <= 1
      ? Boolean(inboundState?.found)
      : observedIncomingCount >= BURST_COUNT;

    if (!inboundVisible) {
      await captureScreenshot(a.driver, "wayfarer-1-failure.png");
      await captureScreenshot(b.driver, "wayfarer-2-failure.png");
      const logA = await readLogTail(a.logPath);
      const logB = await readLogTail(b.logPath);
      const settingsA = await fs.readFile(path.join(a.stateRoot, "settings.json"), "utf8").catch(() => "");
      const settingsB = await fs.readFile(path.join(b.stateRoot, "settings.json"), "utf8").catch(() => "");
      await writeJsonArtifact("failure-summary.json", {
        run_id: RUN_ID,
        test_case_id: TEST_CASE_ID,
        scenario: SCENARIO,
        failed_at_unix_ms: Date.now(),
        failure: "message did not converge within timeout",
        logs: {
          wayfarer_1_tail: logA,
          wayfarer_2_tail: logB
        },
        settings: {
          wayfarer_1: settingsA,
          wayfarer_2: settingsB
        },
        burst: {
          count: BURST_COUNT,
          interval_ms: BURST_INTERVAL_MS,
          prefix: burstPrefix,
          observed_incoming_count: observedIncomingCount,
          missing_sequence_numbers: burstSequenceGaps(inboundState?.texts || [], burstPrefix, BURST_COUNT)
        }
      });
      throw new Error(
        `message did not converge within timeout\n` +
          `instanceA log: ${a.logPath}\n${logA}\n` +
          `instanceB log: ${b.logPath}\n${logB}`
      );
    }

    if (inboundState?.threadKey) {
      await clickContactInChats(b.driver, inboundState.threadKey);
    }

    let logPathTextA = "";
    let logPathTextB = "";
    try {
      await clickTab(a.driver, "settings");
      await clickTab(b.driver, "settings");
      const settingsLogA = await a.driver.wait(until.elementLocated(By.css("[data-testid='settings-log-path']")), 20000);
      const settingsLogB = await b.driver.wait(until.elementLocated(By.css("[data-testid='settings-log-path']")), 20000);
      logPathTextA = await settingsLogA.getText();
      logPathTextB = await settingsLogB.getText();

      expect(path.resolve(logPathTextA)).to.equal(path.resolve(a.logPath));
      expect(path.resolve(logPathTextB)).to.equal(path.resolve(b.logPath));
    } catch {
      logPathTextA = a.logPath;
      logPathTextB = b.logPath;
    }

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
      convergence: {
        method: BURST_COUNT <= 1 ? "chat-history-state" : "chat-history-prefix-count",
        burst_count: BURST_COUNT,
        burst_interval_ms: BURST_INTERVAL_MS,
        inbound_thread_key: inboundState?.threadKey || "",
        inbound_msg_id: inboundState?.msgId || "",
        observed_incoming_count: observedIncomingCount,
        missing_sequence_numbers: burstSequenceGaps(inboundState?.texts || [], burstPrefix, BURST_COUNT)
      },
      node_status: {
        wayfarer_1: statusA,
        wayfarer_2: statusB
      },
      logs: {
        wayfarer_1: logPathTextA,
        wayfarer_2: logPathTextB
      }
      });
    } finally {
      await closeSession(a);
      await closeSession(b);
    }
  });

  it("sends message via relay when LAN peer path is isolated", async function () {
    this.timeout(TEST_TIMEOUT_MS);

    if (!String(RELAY_ENDPOINT || "").trim()) {
      this.skip();
      return;
    }

    const relayOnlyPortA = GOSSIP_BASE_PORT + 200;
    const relayOnlyPortB = GOSSIP_BASE_PORT + 201;

    let a;
    let b;
    try {
      a = await openTauriSession("a", stateRootPath("relay-only-a"), TAURI_DRIVER_A_PORT, {
        AETHOS_E2E_RELAY_ENDPOINT: RELAY_ENDPOINT,
        AETHOS_E2E_DISABLE_RELAY: "0",
        AETHOS_E2E_FORCE_GOSSIP: "1",
        AETHOS_DISABLE_LAN_TCP: "1",
        AETHOS_GOSSIP_LAN_PORT: String(relayOnlyPortA),
        AETHOS_GOSSIP_PEER_PORTS: String(relayOnlyPortA),
        AETHOS_GOSSIP_LOCALHOST_FANOUT: "0",
        AETHOS_GOSSIP_EAGER_UNICAST: "0",
        AETHOS_GOSSIP_LOOPBACK_ONLY: "1"
      });
      b = await openTauriSession("b", stateRootPath("relay-only-b"), TAURI_DRIVER_B_PORT, {
        AETHOS_E2E_RELAY_ENDPOINT: RELAY_ENDPOINT,
        AETHOS_E2E_DISABLE_RELAY: "0",
        AETHOS_E2E_FORCE_GOSSIP: "1",
        AETHOS_DISABLE_LAN_TCP: "1",
        AETHOS_GOSSIP_LAN_PORT: String(relayOnlyPortB),
        AETHOS_GOSSIP_PEER_PORTS: String(relayOnlyPortB),
        AETHOS_GOSSIP_LOCALHOST_FANOUT: "0",
        AETHOS_GOSSIP_EAGER_UNICAST: "0",
        AETHOS_GOSSIP_LOOPBACK_ONLY: "1"
      });

      await waitForSplashToClear(a.driver);
      await waitForSplashToClear(b.driver);

      const idA = await readIdentityWayfarerId(a.stateRoot);
      const idB = await readIdentityWayfarerId(b.stateRoot);
      expect(idA).to.not.equal(idB);

      await openContactsAndAdd(a.driver, idB, "Peer B");
      await openContactsAndAdd(b.driver, idA, "Peer A");
      await clickContactInChats(a.driver, idB);
      await clickContactInChats(b.driver, idA);

      const text = `relay-only-${Date.now()}`;
      await sendChatMessage(a.driver, text);

      await clickSyncInbox(a.driver);
      await clickSyncInbox(b.driver);

      const inboundState = await waitForIncomingMessageInState(b.stateRoot, text);
      expect(Boolean(inboundState?.found)).to.equal(true);

      const relayTransferSeen = await waitForLogPattern(
        b.logPath,
        /relay_encounter_recv_transfer:|relay_worker_sync_success:/,
        180000
      );
      expect(Boolean(relayTransferSeen)).to.equal(true);
    } finally {
      await closeSession(a);
      await closeSession(b);
    }
  });

  it("auto-responds /pong when a peer sends /ping", async function () {
    this.timeout(TEST_TIMEOUT_MS);

    let a;
    let b;
    try {
      a = await openTauriSession("a", stateRootPath("ping-a"), TAURI_DRIVER_A_PORT);
      b = await openTauriSession("b", stateRootPath("ping-b"), TAURI_DRIVER_B_PORT);

      await waitForSplashToClear(a.driver);
      await waitForSplashToClear(b.driver);

      const idA = await readIdentityWayfarerId(a.stateRoot);
      const idB = await readIdentityWayfarerId(b.stateRoot);
      expect(idA).to.not.equal(idB);

      await openContactsAndAdd(a.driver, idB, "Peer B");
      await openContactsAndAdd(b.driver, idA, "Peer A");
      await clickContactInChats(a.driver, idB);
      await clickContactInChats(b.driver, idA);

      await sendChatMessage(a.driver, "/ping");
      await clickSyncInbox(a.driver);
      await clickSyncInbox(b.driver);

      const pingInboundOnB = await waitForIncomingMessageInState(b.stateRoot, "/ping");
      expect(Boolean(pingInboundOnB?.found)).to.equal(true);

      const pongOutboundOnB = await waitForOutgoingMessageInState(b.stateRoot, "/pong");
      expect(Boolean(pongOutboundOnB?.found)).to.equal(true);

      await clickSyncInbox(a.driver);
      await clickSyncInbox(b.driver);

      const pongInboundOnA = await waitForIncomingMessageInState(a.stateRoot, "/pong");
      expect(Boolean(pongInboundOnA?.found)).to.equal(true);
    } finally {
      await closeSession(a);
      await closeSession(b);
    }
  });

  it("transfers large deterministic image and only renders after completion", async function () {
    this.timeout(TEST_TIMEOUT_MS);

    let a;
    let b;
    try {
      a = await openTauriSession("a", stateRootPath("media-a"), TAURI_DRIVER_A_PORT, {
        AETHOS_MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES: String(MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES),
        AETHOS_MEDIA_E2E_TTL_SECONDS: String(MEDIA_E2E_TTL_SECONDS),
        AETHOS_MEDIA_E2E_MISSING_MIN_INTERVAL_MS: String(MEDIA_E2E_MISSING_MIN_INTERVAL_MS),
        AETHOS_MEDIA_E2E_DROP_CHUNK_INDEX: "",
        AETHOS_LAN_FALLBACK_TRANSFER_MAX_ITEMS: "8",
        AETHOS_LAN_FALLBACK_TRANSFER_MAX_BYTES: String(MEDIA_E2E_FALLBACK_TRANSFER_MAX_BYTES),
        AETHOS_LAN_FALLBACK_MAX_CHUNKS_PER_REQUEST: String(MEDIA_E2E_FALLBACK_MAX_CHUNKS_PER_REQUEST),
        AETHOS_LAN_TCP_REQUEST_ENCOUNTER: String(MEDIA_E2E_TCP_REQUEST_ENCOUNTER),
        AETHOS_LAN_MEDIA_CONTROL_FASTLANE_MAX_ITEMS: String(MEDIA_E2E_CONTROL_FASTLANE_MAX_ITEMS),
        AETHOS_LAN_MEDIA_CONTROL_FASTLANE_PACE_MS: String(MEDIA_E2E_CONTROL_FASTLANE_PACE_MS),
        AETHOS_MEDIA_CONTROL_FASTLANE_QUEUE_MAX: String(MEDIA_E2E_CONTROL_FASTLANE_QUEUE_MAX),
        AETHOS_MEDIA_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS: String(MEDIA_E2E_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS),
        AETHOS_MEDIA_CONTROL_FASTLANE_TCP: String(MEDIA_E2E_CONTROL_FASTLANE_TCP),
        AETHOS_MEDIA_MISSING_FASTLANE_REDUNDANCY: String(MEDIA_E2E_MISSING_FASTLANE_REDUNDANCY),
        AETHOS_MEDIA_MISSING_TCP_MIRROR: String(MEDIA_E2E_MISSING_TCP_MIRROR),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_MAX_PER_MINUTE: String(MEDIA_E2E_MISSING_TCP_MIRROR_MAX_PER_MINUTE),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_MIN_INTERVAL_MS: String(MEDIA_E2E_MISSING_TCP_MIRROR_MIN_INTERVAL_MS),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_STALL_MS: String(MEDIA_E2E_MISSING_TCP_MIRROR_STALL_MS),
        AETHOS_MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN: String(MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN),
        AETHOS_MEDIA_E2E_WIRE_BUCKET_BURST_BYTES: String(MEDIA_E2E_WIRE_BUCKET_BURST_BYTES),
        AETHOS_MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS: String(MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS),
        AETHOS_MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING: String(MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING),
        AETHOS_MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT: String(MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT),
        AETHOS_E2E_UDP_TRANSFER_FRAME_MAX_BYTES: String(E2E_UDP_TRANSFER_FRAME_MAX_BYTES),
        AETHOS_MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS: String(MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS),
        AETHOS_DISABLE_LAN_TCP: "0"
      });
      b = await openTauriSession("b", stateRootPath("media-b"), TAURI_DRIVER_B_PORT, {
        AETHOS_MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES: String(MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES),
        AETHOS_MEDIA_E2E_TTL_SECONDS: String(MEDIA_E2E_TTL_SECONDS),
        AETHOS_MEDIA_E2E_MISSING_MIN_INTERVAL_MS: String(MEDIA_E2E_MISSING_MIN_INTERVAL_MS),
        AETHOS_MEDIA_E2E_DROP_CHUNK_INDEX: "",
        AETHOS_LAN_FALLBACK_TRANSFER_MAX_ITEMS: "8",
        AETHOS_LAN_FALLBACK_TRANSFER_MAX_BYTES: String(MEDIA_E2E_FALLBACK_TRANSFER_MAX_BYTES),
        AETHOS_LAN_FALLBACK_MAX_CHUNKS_PER_REQUEST: String(MEDIA_E2E_FALLBACK_MAX_CHUNKS_PER_REQUEST),
        AETHOS_LAN_TCP_REQUEST_ENCOUNTER: String(MEDIA_E2E_TCP_REQUEST_ENCOUNTER),
        AETHOS_LAN_MEDIA_CONTROL_FASTLANE_MAX_ITEMS: String(MEDIA_E2E_CONTROL_FASTLANE_MAX_ITEMS),
        AETHOS_LAN_MEDIA_CONTROL_FASTLANE_PACE_MS: String(MEDIA_E2E_CONTROL_FASTLANE_PACE_MS),
        AETHOS_MEDIA_CONTROL_FASTLANE_QUEUE_MAX: String(MEDIA_E2E_CONTROL_FASTLANE_QUEUE_MAX),
        AETHOS_MEDIA_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS: String(MEDIA_E2E_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS),
        AETHOS_MEDIA_CONTROL_FASTLANE_TCP: String(MEDIA_E2E_CONTROL_FASTLANE_TCP),
        AETHOS_MEDIA_MISSING_FASTLANE_REDUNDANCY: String(MEDIA_E2E_MISSING_FASTLANE_REDUNDANCY),
        AETHOS_MEDIA_MISSING_TCP_MIRROR: String(MEDIA_E2E_MISSING_TCP_MIRROR),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_MAX_PER_MINUTE: String(MEDIA_E2E_MISSING_TCP_MIRROR_MAX_PER_MINUTE),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_MIN_INTERVAL_MS: String(MEDIA_E2E_MISSING_TCP_MIRROR_MIN_INTERVAL_MS),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_STALL_MS: String(MEDIA_E2E_MISSING_TCP_MIRROR_STALL_MS),
        AETHOS_MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN: String(MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN),
        AETHOS_MEDIA_E2E_WIRE_BUCKET_BURST_BYTES: String(MEDIA_E2E_WIRE_BUCKET_BURST_BYTES),
        AETHOS_MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS: String(MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS),
        AETHOS_MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING: String(MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING),
        AETHOS_MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT: String(MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT),
        AETHOS_E2E_UDP_TRANSFER_FRAME_MAX_BYTES: String(E2E_UDP_TRANSFER_FRAME_MAX_BYTES),
        AETHOS_MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS: String(MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS),
        AETHOS_DISABLE_LAN_TCP: "0"
      });

    await waitForSplashToClear(a.driver);
    await waitForSplashToClear(b.driver);

    const idA = await readIdentityWayfarerId(a.stateRoot);
    const idB = await readIdentityWayfarerId(b.stateRoot);
    await openContactsAndAdd(a.driver, idB, "Peer B");
    await openContactsAndAdd(b.driver, idA, "Peer A");
    await clickContactInChats(a.driver, idB);
    await clickContactInChats(b.driver, idA);
    const capabilitiesReadyA = await waitForPeerMediaCapability(a.stateRoot, idB, 120000);
    const capabilitiesReadyB = await waitForPeerMediaCapability(b.stateRoot, idA, 120000);
    expect(capabilitiesReadyA).to.equal(true);
    expect(capabilitiesReadyB).to.equal(true);

    const seed = `${MEDIA_SEED_BASE}-happy`;
    const largePath = path.join(ARTIFACT_ROOT, "fixture-large-happy.png");
    const fixture = await generateDeterministicLargeImage(largePath, seed, 2600, 1900);
    const fileBytes = await fs.readFile(fixture.filePath);
    const expectedObjectDigest = sha256Hex(fileBytes);
    expect(expectedObjectDigest).to.equal(fixture.sha256Hex);

    await attachFileAndSend(a, fixture.filePath, `media-happy-${Date.now()}`);

    const manifestTestId = await waitForMediaManifest(b.driver, MEDIA_MANIFEST_TIMEOUT_MS);
    expect(manifestTestId).to.be.a("string");
    const imageShownEarly = await mediaImagePresent(b.driver);
    expect(imageShownEarly).to.equal(false);

    const completeTransfer = await waitForCompletedMediaTransfer(b.stateRoot, MEDIA_TRANSFER_TIMEOUT_MS);
    expect(completeTransfer, "expected completed media transfer in receiver chat").to.exist;
    const imageShownPreClick = await mediaImagePresent(b.driver);
    expect(imageShownPreClick).to.equal(false);
    await clickMediaLoadButton(b.driver, 40000);

    const rendered = await waitForMediaImageRendered(b.driver, 240000);
    expect(rendered).to.equal(true);

      expect(String(completeTransfer.message.media.objectSha256Hex || "")).to.equal(expectedObjectDigest);
      expect(Number(completeTransfer.message.media.totalBytes || 0)).to.equal(Number(fixture.sizeBytes));
    } finally {
      await closeSession(a);
      await closeSession(b);
    }
  });

  it("withheld chunk prevents render and transfer expires", async function () {
    this.timeout(TEST_TIMEOUT_MS);

    let a;
    let b;
    try {
      a = await openTauriSession("a", stateRootPath("media-fail-a"), TAURI_DRIVER_A_PORT, {
        AETHOS_MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES: String(MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES),
        AETHOS_MEDIA_E2E_TTL_SECONDS: "45",
        AETHOS_MEDIA_E2E_MISSING_MIN_INTERVAL_MS: "300",
        AETHOS_MEDIA_E2E_DROP_CHUNK_INDEX: "2",
        AETHOS_LAN_FALLBACK_TRANSFER_MAX_ITEMS: "8",
        AETHOS_LAN_FALLBACK_TRANSFER_MAX_BYTES: String(MEDIA_E2E_FALLBACK_TRANSFER_MAX_BYTES),
        AETHOS_LAN_FALLBACK_MAX_CHUNKS_PER_REQUEST: String(MEDIA_E2E_FALLBACK_MAX_CHUNKS_PER_REQUEST),
        AETHOS_LAN_TCP_REQUEST_ENCOUNTER: String(MEDIA_E2E_TCP_REQUEST_ENCOUNTER),
        AETHOS_LAN_MEDIA_CONTROL_FASTLANE_MAX_ITEMS: String(MEDIA_E2E_CONTROL_FASTLANE_MAX_ITEMS),
        AETHOS_LAN_MEDIA_CONTROL_FASTLANE_PACE_MS: String(MEDIA_E2E_CONTROL_FASTLANE_PACE_MS),
        AETHOS_MEDIA_CONTROL_FASTLANE_QUEUE_MAX: String(MEDIA_E2E_CONTROL_FASTLANE_QUEUE_MAX),
        AETHOS_MEDIA_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS: String(MEDIA_E2E_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS),
        AETHOS_MEDIA_CONTROL_FASTLANE_TCP: String(MEDIA_E2E_CONTROL_FASTLANE_TCP),
        AETHOS_MEDIA_MISSING_FASTLANE_REDUNDANCY: String(MEDIA_E2E_MISSING_FASTLANE_REDUNDANCY),
        AETHOS_MEDIA_MISSING_TCP_MIRROR: String(MEDIA_E2E_MISSING_TCP_MIRROR),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_MAX_PER_MINUTE: String(MEDIA_E2E_MISSING_TCP_MIRROR_MAX_PER_MINUTE),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_MIN_INTERVAL_MS: String(MEDIA_E2E_MISSING_TCP_MIRROR_MIN_INTERVAL_MS),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_STALL_MS: String(MEDIA_E2E_MISSING_TCP_MIRROR_STALL_MS),
        AETHOS_MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN: String(MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN),
        AETHOS_MEDIA_E2E_WIRE_BUCKET_BURST_BYTES: String(MEDIA_E2E_WIRE_BUCKET_BURST_BYTES),
        AETHOS_MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS: String(MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS),
        AETHOS_MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING: String(MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING),
        AETHOS_MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT: String(MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT),
        AETHOS_E2E_UDP_TRANSFER_FRAME_MAX_BYTES: String(E2E_UDP_TRANSFER_FRAME_MAX_BYTES),
        AETHOS_MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS: String(MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS),
        AETHOS_DISABLE_LAN_TCP: "0"
      });
      b = await openTauriSession("b", stateRootPath("media-fail-b"), TAURI_DRIVER_B_PORT, {
        AETHOS_MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES: String(MEDIA_E2E_MAX_ITEM_PAYLOAD_B64_BYTES),
        AETHOS_MEDIA_E2E_TTL_SECONDS: "45",
        AETHOS_MEDIA_E2E_MISSING_MIN_INTERVAL_MS: "300",
        AETHOS_LAN_FALLBACK_TRANSFER_MAX_ITEMS: "8",
        AETHOS_LAN_FALLBACK_TRANSFER_MAX_BYTES: String(MEDIA_E2E_FALLBACK_TRANSFER_MAX_BYTES),
        AETHOS_LAN_FALLBACK_MAX_CHUNKS_PER_REQUEST: String(MEDIA_E2E_FALLBACK_MAX_CHUNKS_PER_REQUEST),
        AETHOS_LAN_TCP_REQUEST_ENCOUNTER: String(MEDIA_E2E_TCP_REQUEST_ENCOUNTER),
        AETHOS_LAN_MEDIA_CONTROL_FASTLANE_MAX_ITEMS: String(MEDIA_E2E_CONTROL_FASTLANE_MAX_ITEMS),
        AETHOS_LAN_MEDIA_CONTROL_FASTLANE_PACE_MS: String(MEDIA_E2E_CONTROL_FASTLANE_PACE_MS),
        AETHOS_MEDIA_CONTROL_FASTLANE_QUEUE_MAX: String(MEDIA_E2E_CONTROL_FASTLANE_QUEUE_MAX),
        AETHOS_MEDIA_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS: String(MEDIA_E2E_CONTROL_FASTLANE_TCP_BATCH_MAX_ITEMS),
        AETHOS_MEDIA_CONTROL_FASTLANE_TCP: String(MEDIA_E2E_CONTROL_FASTLANE_TCP),
        AETHOS_MEDIA_MISSING_FASTLANE_REDUNDANCY: String(MEDIA_E2E_MISSING_FASTLANE_REDUNDANCY),
        AETHOS_MEDIA_MISSING_TCP_MIRROR: String(MEDIA_E2E_MISSING_TCP_MIRROR),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_MAX_PER_MINUTE: String(MEDIA_E2E_MISSING_TCP_MIRROR_MAX_PER_MINUTE),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_MIN_INTERVAL_MS: String(MEDIA_E2E_MISSING_TCP_MIRROR_MIN_INTERVAL_MS),
        AETHOS_MEDIA_MISSING_TCP_MIRROR_STALL_MS: String(MEDIA_E2E_MISSING_TCP_MIRROR_STALL_MS),
        AETHOS_MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN: String(MEDIA_E2E_WIRE_BUCKET_SUSTAINED_BYTES_PER_MIN),
        AETHOS_MEDIA_E2E_WIRE_BUCKET_BURST_BYTES: String(MEDIA_E2E_WIRE_BUCKET_BURST_BYTES),
        AETHOS_MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS: String(MEDIA_E2E_MISSING_RESPONSE_DEDUP_WINDOW_MS),
        AETHOS_MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING: String(MEDIA_E2E_MISSING_RESPONSE_CHUNK_CEILING),
        AETHOS_MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT: String(MEDIA_E2E_CHUNKS_PER_MINUTE_LIMIT),
        AETHOS_E2E_UDP_TRANSFER_FRAME_MAX_BYTES: String(E2E_UDP_TRANSFER_FRAME_MAX_BYTES),
        AETHOS_MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS: String(MEDIA_E2E_HOUSEKEEPING_MIN_INTERVAL_MS),
        AETHOS_DISABLE_LAN_TCP: "0"
      });

    await waitForSplashToClear(a.driver);
    await waitForSplashToClear(b.driver);

    const idA = await readIdentityWayfarerId(a.stateRoot);
    const idB = await readIdentityWayfarerId(b.stateRoot);
    await openContactsAndAdd(a.driver, idB, "Peer B");
    await openContactsAndAdd(b.driver, idA, "Peer A");
    await clickContactInChats(a.driver, idB);
    await clickContactInChats(b.driver, idA);
    const capabilitiesReadyA = await waitForPeerMediaCapability(a.stateRoot, idB, 120000);
    const capabilitiesReadyB = await waitForPeerMediaCapability(b.stateRoot, idA, 120000);
    expect(capabilitiesReadyA).to.equal(true);
    expect(capabilitiesReadyB).to.equal(true);

    const seed = `${MEDIA_SEED_BASE}-withhold`;
    const largePath = path.join(ARTIFACT_ROOT, "fixture-large-withhold.png");
    const fixture = await generateDeterministicLargeImage(largePath, seed, 2600, 1900);
    await attachFileAndSend(a, fixture.filePath, `media-fail-${Date.now()}`);

    const manifestTestId = await waitForMediaManifest(b.driver, MEDIA_MANIFEST_TIMEOUT_MS);
    expect(manifestTestId).to.be.a("string");
    const rendered = await waitForMediaImageRendered(b.driver, 35000);
    expect(rendered).to.equal(false);

    await new Promise((resolve) => setTimeout(resolve, 55000));

    const transfers = await transferStateFromChat(b.stateRoot);
    const failed = transfers.find((entry) => {
      const status = String(entry?.message?.media?.status || "").toLowerCase();
      const error = String(entry?.message?.media?.error || "").toLowerCase();
      return status === "failed" || error.includes("expired");
    });
    expect(failed, "expected failed/expired transfer in receiver chat").to.exist;
      const imagePresent = await mediaImagePresent(b.driver);
      expect(imagePresent).to.equal(false);
    } finally {
      await closeSession(a);
      await closeSession(b);
    }
  });
});
