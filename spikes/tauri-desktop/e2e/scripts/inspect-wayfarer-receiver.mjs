import fs from "node:fs/promises";
import path from "node:path";

const STATE_ROOT = path.resolve(String(process.env.AETHOS_RECEIVER_STATE_DIR || "").trim() || ".");
const SENDER_WAYFARER_ID = String(process.env.AETHOS_RECEIVER_SENDER_ID || "").trim().toLowerCase();
const TRANSFER_ID_FILTER = String(process.env.AETHOS_RECEIVER_TRANSFER_ID || "").trim();
const WATCH_SECONDS = Number(process.env.AETHOS_RECEIVER_WATCH_SECONDS || "90");
const POLL_MS = Number(process.env.AETHOS_RECEIVER_POLL_MS || "1500");
const STALL_SECONDS = Number(process.env.AETHOS_RECEIVER_STALL_SECONDS || "18");

if (!STATE_ROOT || STATE_ROOT === ".") {
  throw new Error("set AETHOS_RECEIVER_STATE_DIR to the receiver state root (same value used for AETHOS_STATE_DIR)");
}

async function readJson(filePath) {
  const raw = await fs.readFile(filePath, "utf8");
  return JSON.parse(raw);
}

async function exists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch {
    return false;
  }
}

async function listDirs(root) {
  try {
    const entries = await fs.readdir(root, { withFileTypes: true });
    return entries.filter((entry) => entry.isDirectory()).map((entry) => entry.name);
  } catch {
    return [];
  }
}

async function collectTransferSnapshot() {
  const spoolRoot = path.join(STATE_ROOT, "media", "spool");
  const completedRoot = path.join(STATE_ROOT, "media", "complete");
  const chatPath = path.join(STATE_ROOT, "chat-history.json");

  const senderDirs = SENDER_WAYFARER_ID ? [SENDER_WAYFARER_ID] : await listDirs(spoolRoot);
  const transfers = [];

  for (const sender of senderDirs) {
    const senderRoot = path.join(spoolRoot, sender);
    const transferDirs = await listDirs(senderRoot);
    for (const transferId of transferDirs) {
      if (TRANSFER_ID_FILTER && TRANSFER_ID_FILTER !== transferId) continue;
      const statePath = path.join(senderRoot, transferId, "state.json");
      if (!(await exists(statePath))) continue;

      let state;
      try {
        state = await readJson(statePath);
      } catch {
        continue;
      }

      const chunksDir = path.join(senderRoot, transferId, "chunks");
      let chunkFiles = 0;
      let chunkBytes = 0;
      try {
        const files = await fs.readdir(chunksDir, { withFileTypes: true });
        for (const file of files) {
          if (!file.isFile()) continue;
          chunkFiles += 1;
          const stat = await fs.stat(path.join(chunksDir, file.name));
          chunkBytes += Number(stat.size || 0);
        }
      } catch {
        // ignore missing chunk dir
      }

      const completeMetaPath = path.join(completedRoot, `${state.objectSha256Hex}.json`);
      const completeBinPath = path.join(completedRoot, `${state.objectSha256Hex}.bin`);
      const completedMetaExists = await exists(completeMetaPath);
      const completedBinExists = await exists(completeBinPath);

      transfers.push({
        senderWayfarerId: sender,
        transferId,
        objectSha256Hex: String(state.objectSha256Hex || ""),
        totalBytes: Number(state.totalBytes || 0),
        chunkCount: Number(state.chunkCount || 0),
        receivedChunks: Number(state.receivedChunks || 0),
        receivedBytes: Number(state.receivedBytes || 0),
        completedUnixMs: state.completedUnixMs || null,
        failedError: String(state.failedError || ""),
        lastMissingUnixMs: Number(state.lastMissingUnixMs || 0),
        chunkFiles,
        chunkBytes,
        completedMetaExists,
        completedBinExists,
        statePath,
        chunksDir
      });
    }
  }

  let chatTransfers = [];
  try {
    const chat = await readJson(chatPath);
    for (const [contact, thread] of Object.entries(chat?.threads || {})) {
      for (const msg of thread || []) {
        if (!msg?.media?.transferId) continue;
        if (TRANSFER_ID_FILTER && String(msg.media.transferId) !== TRANSFER_ID_FILTER) continue;
        if (SENDER_WAYFARER_ID && contact !== SENDER_WAYFARER_ID) continue;
        chatTransfers.push({
          contact,
          transferId: String(msg.media.transferId),
          direction: String(msg.direction || ""),
          status: String(msg.media.status || ""),
          chunkCount: Number(msg.media.chunkCount || 0),
          receivedChunks: Number(msg.media.receivedChunks || 0),
          error: String(msg.media.error || ""),
          msgId: String(msg.msgId || "")
        });
      }
    }
  } catch {
    chatTransfers = [];
  }

  return {
    observedAtUnixMs: Date.now(),
    stateRoot: STATE_ROOT,
    senderFilter: SENDER_WAYFARER_ID || null,
    transferFilter: TRANSFER_ID_FILTER || null,
    transfers,
    chatTransfers
  };
}

async function main() {
  const started = Date.now();
  const deadline = started + WATCH_SECONDS * 1000;
  let previous = new Map();
  let finalSnapshot = null;
  let stallDetected = false;
  let stallDetails = null;

  while (Date.now() < deadline) {
    const snapshot = await collectTransferSnapshot();
    finalSnapshot = snapshot;

    const summary = {
      observedAtUnixMs: snapshot.observedAtUnixMs,
      transferCount: snapshot.transfers.length,
      active: snapshot.transfers.filter((t) => !t.completedUnixMs && !t.failedError).length,
      completed: snapshot.transfers.filter((t) => !!t.completedUnixMs).length,
      failed: snapshot.transfers.filter((t) => !!t.failedError).length
    };
    console.log(JSON.stringify(summary));

    for (const transfer of snapshot.transfers) {
      const key = `${transfer.senderWayfarerId}:${transfer.transferId}`;
      const prior = previous.get(key);
      const nowMs = snapshot.observedAtUnixMs;
      const completed = Boolean(transfer.completedUnixMs);
      const failed = Boolean(transfer.failedError);
      if (completed || failed) continue;

      if (!prior) {
        previous.set(key, {
          receivedChunks: transfer.receivedChunks,
          receivedBytes: transfer.receivedBytes,
          observedAtUnixMs: nowMs
        });
        continue;
      }

      const advanced =
        transfer.receivedChunks > prior.receivedChunks || transfer.receivedBytes > prior.receivedBytes;
      if (advanced) {
        previous.set(key, {
          receivedChunks: transfer.receivedChunks,
          receivedBytes: transfer.receivedBytes,
          observedAtUnixMs: nowMs
        });
        continue;
      }

      const stalledForMs = nowMs - prior.observedAtUnixMs;
      if (stalledForMs >= STALL_SECONDS * 1000) {
        stallDetected = true;
        stallDetails = {
          key,
          stalledForMs,
          transfer
        };
        break;
      }
    }

    if (stallDetected) break;
    if (summary.active === 0 && summary.completed > 0) break;
    await new Promise((resolve) => setTimeout(resolve, POLL_MS));
  }

  const report = {
    ok: !stallDetected,
    startedAtUnixMs: started,
    endedAtUnixMs: Date.now(),
    watchSeconds: WATCH_SECONDS,
    pollMs: POLL_MS,
    stallSeconds: STALL_SECONDS,
    stall: stallDetails,
    finalSnapshot
  };

  console.log(JSON.stringify(report, null, 2));
  if (stallDetected) {
    process.exit(2);
  }
}

main().catch((error) => {
  console.error(`receiver inspect failed: ${String(error?.stack || error)}`);
  process.exit(1);
});
