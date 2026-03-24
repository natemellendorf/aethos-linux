import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  BellRing,
  CheckCircle2,
  ContactRound,
  MessageCircle,
  QrCode,
  Settings,
  Share2,
  Wifi,
  Wind,
  Maximize2,
  Minimize2,
  Paperclip,
  FileDown
} from "lucide-react";

import { Badge } from "./components/ui/badge";
import { Button } from "./components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./components/ui/card";
import { Input } from "./components/ui/input";
import { Textarea } from "./components/ui/textarea";
import { soundManager } from "./lib/soundManager";
import { cn } from "./lib/utils";

const TABS = [
  { id: "chats", label: "Chats", icon: MessageCircle },
  { id: "contacts", label: "Contacts", icon: ContactRound },
  { id: "share", label: "Share", icon: Share2 },
  { id: "settings", label: "Settings", icon: Settings }
];

const emptyRelay = { chipText: "Relays: idle", chipState: "idle", primaryStatus: "idle", secondaryStatus: "idle" };
const emptyGossip = { enabled: false, running: false, lastActivityMs: 0, lastEvent: "unknown" };
const LOG_FILTERS = {
  all: [],
  errors: ["failed", "error", "rejected", "panic"],
  send: ["send_message_", "gossip_record_local_payload_ok"],
  gossip: ["gossip_"],
  relay: ["relay_"],
  transfer: ["transfer_import_", "transfer_select_", "gossip_transfer_"]
};

const RELEASES_LATEST_URL = "https://api.github.com/repos/natemellendorf/aethos-client/releases/latest";
const DEFAULT_RELAY_ENDPOINT = "wss://aethos-relay.network";
const DONATE_CRYPTO_ADDRESS = "0x114227a8B460E462f408F138a929660531790ee3";
const DONATE_PAYPAL_URL = "https://www.paypal.com/donate/?hosted_button_id=TPTTR6TRKBYKS";
const MEDIA_PREVIEW_GATE_BYTES = 5 * 1024 * 1024;

function tinyId(id = "") {
  if (!id) return "-";
  if (id.length <= 24) return id;
  return `${id.slice(0, 10)}...${id.slice(-8)}`;
}

function formatMessageTimestamp(message) {
  const ms = messageUnixMs(message);
  if (!Number.isFinite(ms) || ms <= 0) return String(message?.timestamp || "");
  return new Date(ms).toLocaleTimeString([], {
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
    fractionalSecondDigits: 3
  });
}

function parseSentAtUnixMsFromText(input) {
  if (typeof input !== "string" || input.length === 0) return null;
  try {
    const value = JSON.parse(input);
    const sentMs = Number(value?.sent_at_unix_ms ?? value?.sentAtUnixMs);
    return Number.isFinite(sentMs) && sentMs > 0 ? sentMs : null;
  } catch {
    return null;
  }
}

function messageUnixMs(message) {
  const createdAtMs = Number(message?.createdAtUnixMs);
  if (Number.isFinite(createdAtMs) && createdAtMs > 0) return createdAtMs;

  const createdRaw = Number(message?.createdAtUnix);
  if (Number.isFinite(createdRaw) && createdRaw > 0) {
    return createdRaw > 1_000_000_000_000 ? createdRaw : createdRaw * 1000;
  }

  const timestampRaw = Number(message?.timestamp);
  if (Number.isFinite(timestampRaw) && timestampRaw > 0) {
    return timestampRaw > 1_000_000_000_000 ? timestampRaw : timestampRaw * 1000;
  }

  const localIdMatch = String(message?.msgId || "").match(/^local-(\d+)-/);
  if (localIdMatch) {
    const localMs = Number(localIdMatch[1]);
    if (Number.isFinite(localMs) && localMs > 0) return localMs;
  }

  const payloadSentMs = parseSentAtUnixMsFromText(message?.text);
  if (Number.isFinite(payloadSentMs) && payloadSentMs > 0) return payloadSentMs;

  return 0;
}

function createOptimisticOutgoingMessage(localId, text) {
  const nowMs = Date.now();
  return {
    msgId: localId,
    text,
    timestamp: new Date(nowMs).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" }),
    createdAtUnix: Math.floor(nowMs / 1000),
    direction: "Outgoing",
    seen: true,
    manifestIdHex: null,
    deliveredAt: null,
    outboundState: "sending",
    expiresAtUnixMs: null,
    lastSyncAttemptUnixMs: nowMs,
    lastSyncError: null,
    attachment: null
  };
}

function createOptimisticOutgoingMessageWithAttachment(localId, text, attachment) {
  return {
    ...createOptimisticOutgoingMessage(localId, text),
            attachment: attachment
      ? {
          fileName: attachment.fileName,
          mimeType: attachment.mimeType,
          sizeBytes: attachment.sizeBytes,
          contentB64: null
        }
      : null
  };
}

function formatOutgoingStatus(message) {
  if (message?.direction !== "Outgoing") return null;
  const state = message?.outboundState;
  if (state === "sending") return "Queued";
  if (state === "sent") return "Sent";
  if (state && typeof state === "object" && state.failed?.error) return `Failed: ${state.failed.error}`;
  if (message?.lastSyncError) return `Failed: ${message.lastSyncError}`;
  if (message?.deliveredAt) return "Sent";
  return null;
}

function formatBytes(bytes = 0) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

async function readFileAsBase64(file) {
  const buffer = await file.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  let binary = "";
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunkSize));
  }
  return btoa(binary);
}

function sanitizeDownloadFileName(input, fallback = "aethos-file") {
  const raw = String(input || "").trim();
  if (!raw) return fallback;
  const cleaned = raw
    .replace(/[\\/]/g, "_")
    .replace(/[^a-zA-Z0-9._-]/g, "_")
    .replace(/^[_\.]+|[_\.]+$/g, "")
    .slice(0, 180);
  return cleaned || fallback;
}

function mediaStateKey(message) {
  const transferId = message?.media?.transferId;
  const objectSha = message?.media?.objectSha256Hex;
  if (transferId) return `transfer:${transferId}`;
  if (objectSha) return `object:${objectSha}`;
  return null;
}

function mediaStatus(media) {
  return String(media?.status || "").trim().toLowerCase();
}

function mediaProgressPercent(media) {
  const total = Number(media?.totalBytes || 0);
  const got = Number(media?.receivedBytes || 0);
  if (!Number.isFinite(total) || total <= 0) return 0;
  if (!Number.isFinite(got) || got <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round((got / total) * 100)));
}

function outgoingMediaProgressPercent(message) {
  const state = String(message?.outboundState || "").trim().toLowerCase();
  if (state === "sent" || message?.deliveredAt) return 100;
  if (state === "sending") return 12;
  if (message?.outboundState && typeof message.outboundState === "object" && message.outboundState.failed?.error) {
    return 100;
  }
  if (message?.lastSyncError) return 100;
  return 12;
}

function mediaProgressText(message) {
  const status = mediaStatus(message?.media);
  if (status === "complete") return "Transfer complete";
  if (status === "failed") {
    return `Transfer failed${message?.media?.error ? `: ${message.media.error}` : ""}`;
  }
  if (message?.direction === "Outgoing") {
    const outbound = formatOutgoingStatus(message);
    return outbound ? `Send ${outbound.toLowerCase()}` : "Queued for delivery";
  }
  return `Receiving ${mediaProgressPercent(message?.media)}% (${message?.media?.receivedChunks || 0}/${message?.media?.chunkCount || 0} chunks)`;
}

function parseSemverLike(value = "") {
  const clean = String(value).trim().replace(/^v/i, "");
  const [major = "0", minor = "0", patch = "0"] = clean.split(".");
  return {
    raw: clean,
    major: Number.parseInt(major, 10) || 0,
    minor: Number.parseInt(minor, 10) || 0,
    patch: Number.parseInt(patch, 10) || 0
  };
}

function isVersionNewer(current, latest) {
  const a = parseSemverLike(current);
  const b = parseSemverLike(latest);
  if (b.major !== a.major) return b.major > a.major;
  if (b.minor !== a.minor) return b.minor > a.minor;
  return b.patch > a.patch;
}

function summarizeReleaseNotes(body = "") {
  const plain = body.replace(/\r/g, "").trim();
  if (!plain) return "No release notes summary available.";
  const firstParagraph = plain.split("\n\n").find((part) => part.trim()) || plain;
  const compact = firstParagraph.replace(/\n+/g, " ").trim();
  return compact.length > 220 ? `${compact.slice(0, 217)}...` : compact;
}

function pickInstallerAssetUrl(release) {
  const assets = Array.isArray(release?.assets) ? release.assets : [];
  const ua = navigator.userAgent || "";
  const isWindows = /Windows/i.test(ua);
  const isMac = /Macintosh|Mac OS X/i.test(ua);
  const isLinux = /Linux/i.test(ua) && !/Android/i.test(ua);

  const bySuffix = (suffix) => assets.find((asset) => asset?.browser_download_url?.toLowerCase().endsWith(suffix));

  if (isWindows) return bySuffix(".exe")?.browser_download_url || null;
  if (isMac) return bySuffix(".dmg")?.browser_download_url || null;
  if (isLinux) {
    return bySuffix(".appimage")?.browser_download_url || bySuffix(".deb")?.browser_download_url || null;
  }
  return null;
}

export default function App() {
  const [tab, setTab] = useState("chats");
  const [status, setStatus] = useState("Loading app state...");
  const [identity, setIdentity] = useState(null);
  const [settings, setSettings] = useState(null);
  const [relayEndpointsDraft, setRelayEndpointsDraft] = useState("");
  const [contacts, setContacts] = useState({});
  const [chat, setChat] = useState({ selectedContact: null, threads: {}, newContacts: [] });
  const [relayReports, setRelayReports] = useState([]);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [currentVersion, setCurrentVersion] = useState("0.0.0");
  const [updateNotice, setUpdateNotice] = useState(null);
  const [updateDismissed, setUpdateDismissed] = useState(false);
  const [copyToast, setCopyToast] = useState("");
  const [relayHealth, setRelayHealth] = useState(emptyRelay);
  const [gossipStatus, setGossipStatus] = useState(emptyGossip);
  const [shareQr, setShareQr] = useState(null);
  const [diagnostics, setDiagnostics] = useState(null);
  const [composer, setComposer] = useState("");
  const [composerAttachment, setComposerAttachment] = useState(null);
  const [loadingMediaByKey, setLoadingMediaByKey] = useState({});
  const [resolvedMediaByKey, setResolvedMediaByKey] = useState({});
  const [previewImageByKey, setPreviewImageByKey] = useState({});
  const [contactDraft, setContactDraft] = useState({ wayfarerId: "", alias: "" });
  const [showSplash, setShowSplash] = useState(true);
  const [splashFade, setSplashFade] = useState(false);
  const [networkPulseTs, setNetworkPulseTs] = useState(0);
  const [logTail, setLogTail] = useState({ logFilePath: "", totalLines: 0, shownLines: 0, content: "" });
  const [logFollow, setLogFollow] = useState(true);
  const [logStreaming, setLogStreaming] = useState(true);
  const [logFilter, setLogFilter] = useState("all");
  const [arrivingMessageIds, setArrivingMessageIds] = useState({});
  const [confirmDialog, setConfirmDialog] = useState(null);
  const logContainerRef = useRef(null);
  const threadContainerRef = useRef(null);
  const attachmentInputRef = useRef(null);
  const seenThreadMessageIdsRef = useRef(new Set());
  const seenIncomingMessageIdsRef = useRef(new Set());
  const hasHydratedIncomingRef = useRef(false);

  const entries = useMemo(
    () => Object.entries(contacts).sort((a, b) => (a[1] || "").localeCompare(b[1] || "", undefined, { sensitivity: "base" })),
    [contacts]
  );

  const selectedContactId = chat.selectedContact;
  const selectedThread = useMemo(() => {
    if (!selectedContactId) return [];
    const thread = chat.threads[selectedContactId] || [];
    return [...thread].sort((left, right) => {
      const leftMs = messageUnixMs(left);
      const rightMs = messageUnixMs(right);
      if (leftMs !== rightMs) return leftMs - rightMs;
      return String(left.msgId || "").localeCompare(String(right.msgId || ""));
    });
  }, [chat.threads, selectedContactId]);

  const mediaDebugRows = useMemo(() => {
    const rows = [];
    for (const [contactId, thread] of Object.entries(chat.threads || {})) {
      for (const message of thread || []) {
        if (!message?.media) continue;
        const media = message.media;
        const transferId = String(media.transferId || "");
        const chunkCount = Number(media.chunkCount || 0);
        const receivedChunks = Number(media.receivedChunks || 0);
        const status = String(media.status || "").trim() || "unknown";
        const contactAlias = contacts?.[contactId] || tinyId(contactId);
        const syncError = String(message?.lastSyncError || "").trim();
        const mediaError = String(media?.error || "").trim();
        const failedError =
          message?.outboundState && typeof message.outboundState === "object"
            ? String(message.outboundState?.failed?.error || "").trim()
            : "";
        const lastError = mediaError || syncError || failedError || "-";
        rows.push({
          key: `${contactId}:${transferId || message.msgId}`,
          direction: String(message.direction || "Unknown"),
          contactId,
          contactAlias,
          transferId: transferId || String(message.msgId || ""),
          chunkCount,
          receivedChunks,
          status,
          lastError,
          createdAtUnixMs: Number(messageUnixMs(message) || 0)
        });
      }
    }
    rows.sort((a, b) => b.createdAtUnixMs - a.createdAtUnixMs);
    return rows;
  }, [chat.threads, contacts]);

  useEffect(() => {
    const baseline = new Set(selectedThread.map((message) => message.msgId));
    seenThreadMessageIdsRef.current = baseline;
    setArrivingMessageIds({});
  }, [selectedContactId]);

  useEffect(() => {
    const prior = seenThreadMessageIdsRef.current;
    const nowIds = selectedThread.map((message) => message.msgId);
    const nextSet = new Set(nowIds);
    const addedIncoming = selectedThread
      .filter((message) => !prior.has(message.msgId) && message.direction === "Incoming")
      .map((message) => message.msgId);

    seenThreadMessageIdsRef.current = nextSet;
    if (addedIncoming.length === 0) return;

    setArrivingMessageIds((existing) => {
      const next = { ...existing };
      addedIncoming.forEach((id) => {
        next[id] = true;
      });
      return next;
    });

    const timer = setTimeout(() => {
      setArrivingMessageIds((existing) => {
        const next = { ...existing };
        addedIncoming.forEach((id) => {
          delete next[id];
        });
        return next;
      });
    }, 1200);

    return () => clearTimeout(timer);
  }, [selectedThread]);

  const runBootstrap = async () => {
    const startedAt = Date.now();
    try {
      const [boot, diag, gossip, relay, log] = await Promise.all([
        invoke("bootstrap_state"),
        invoke("app_diagnostics"),
        invoke("gossip_status"),
        invoke("relay_health_status"),
        invoke("read_app_log", { maxLines: 500 })
      ]);
      setIdentity(boot.identity);
      setSettings(boot.settings);
      setRelayEndpointsDraft((boot.settings?.relayEndpoints || []).join("\n"));
      setContacts(boot.contacts || {});
      const initialChat = boot.chat || { selectedContact: null, threads: {}, newContacts: [] };
      if (!initialChat.selectedContact && Object.keys(boot.contacts || {}).length > 0) {
        initialChat.selectedContact = Object.keys(boot.contacts || {})[0];
      }
      setChat(initialChat);
      setDiagnostics(diag);
      setGossipStatus(gossip);
      setRelayHealth(relay);
      setLogTail(log);
      setStatus("Ready");
      setNetworkPulseTs(Date.now());
    } catch (error) {
      setStatus(`Bootstrap failed: ${String(error)}`);
    } finally {
      const elapsed = Date.now() - startedAt;
      const delayBeforeFade = Math.max(600, 1800 - elapsed);
      setTimeout(() => {
        setSplashFade(true);
        setTimeout(() => setShowSplash(false), 1100);
      }, delayBeforeFade);
    }
  };

  useEffect(() => {
    runBootstrap();
  }, []);

  useEffect(() => {
    let cancelled = false;
    const checkForUpdates = async () => {
      try {
        const localVersion = await invoke("app_version");
        if (cancelled) return;
        setCurrentVersion(localVersion);

        const response = await fetch(RELEASES_LATEST_URL, {
          method: "GET",
          headers: { Accept: "application/vnd.github+json" }
        });
        if (!response.ok) return;
        const latest = await response.json();
        const latestTag = String(latest.tag_name || "").trim();
        if (!latestTag) return;
        if (!isVersionNewer(localVersion, latestTag)) return;

        if (cancelled) return;
        setUpdateNotice({
          currentVersion: parseSemverLike(localVersion).raw,
          latestVersion: parseSemverLike(latestTag).raw,
          notesSummary: summarizeReleaseNotes(latest.body || ""),
          notesUrl: latest.html_url || "https://github.com/natemellendorf/aethos-client/releases",
          downloadUrl: pickInstallerAssetUrl(latest)
        });
      } catch {
        // fail silently for non-disruptive UX
      }
    };

    const timer = setTimeout(checkForUpdates, 2200);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    appWindow.isFullscreen().then(setIsFullscreen).catch(() => {});
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten;
    listen("chat_snapshot", (event) => {
      if (disposed) return;
      const snapshot = event.payload || {};
      if (snapshot.chat) setChat(snapshot.chat);
      if (snapshot.contacts) setContacts(snapshot.contacts);
      setNetworkPulseTs(Date.now());
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {
        // keep chat view responsive
      });
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten;
    listen("sound_event", (event) => {
      if (disposed) return;
      const kind = event.payload?.kind;
      if (typeof kind === "string") {
        soundManager.play(kind);
      }
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {
        // keep app responsive if listener fails
      });

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    const nextIncoming = new Set();
    Object.values(chat.threads || {}).forEach((thread) => {
      thread.forEach((message) => {
        if (message.direction === "Incoming") {
          nextIncoming.add(message.msgId);
        }
      });
    });

    if (!hasHydratedIncomingRef.current) {
      seenIncomingMessageIdsRef.current = nextIncoming;
      hasHydratedIncomingRef.current = true;
      return;
    }

    const prior = seenIncomingMessageIdsRef.current;
    const hasNewIncoming = [...nextIncoming].some((id) => !prior.has(id));
    seenIncomingMessageIdsRef.current = nextIncoming;
    if (hasNewIncoming) {
      soundManager.play("receive");
    }
  }, [chat.threads]);

  useEffect(() => {
    if (tab !== "settings" || !logStreaming) return;

    let cancelled = false;
    const fetchLogs = async () => {
      try {
        const log = await invoke("read_app_log", { maxLines: 500 });
        if (!cancelled) {
          setLogTail(log);
        }
      } catch {
        // keep settings page responsive
      }
    };

    fetchLogs();
    const timer = setInterval(fetchLogs, 1500);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [tab, logStreaming]);

  useEffect(() => {
    if (!logFollow || tab !== "settings") return;
    const el = logContainerRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [logTail, logFollow, tab]);

  useEffect(() => {
    if (tab !== "chats") return;
    const el = threadContainerRef.current;
    if (!el) return;
    const raf = requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight;
    });
    return () => cancelAnimationFrame(raf);
  }, [tab, selectedContactId, selectedThread.length]);

  useEffect(() => {
    const existingKeys = new Set(selectedThread.map((message) => mediaStateKey(message)).filter(Boolean));
    setPreviewImageByKey((prev) => {
      let changed = false;
      const next = {};
      Object.entries(prev).forEach(([key, value]) => {
        if (existingKeys.has(key)) {
          next[key] = value;
        } else {
          changed = true;
        }
      });
      return changed ? next : prev;
    });
    setResolvedMediaByKey((prev) => {
      let changed = false;
      const next = {};
      Object.entries(prev).forEach(([key, value]) => {
        if (existingKeys.has(key)) {
          next[key] = value;
        } else {
          changed = true;
        }
      });
      return changed ? next : prev;
    });
  }, [selectedThread]);

  useEffect(() => {
    const timer = setInterval(async () => {
      try {
        const [gossip, relay] = await Promise.all([invoke("gossip_status"), invoke("relay_health_status")]);
        setGossipStatus(gossip);
        setRelayHealth(relay);
        if (gossip.lastActivityMs > 0 || relay.chipState === "ok" || relay.chipState === "warn") {
          setNetworkPulseTs(Date.now());
        }
      } catch {
        // keep UI responsive
      }
    }, 9000);
    return () => clearInterval(timer);
  }, []);

  const withNetworkPulse = async (fn) => {
    setNetworkPulseTs(Date.now());
    const result = await fn();
    setNetworkPulseTs(Date.now());
    return result;
  };

  useEffect(() => {
    const selected = chat.selectedContact || "";
    setContactDraft({
      wayfarerId: selected,
      alias: selected ? contacts[selected] || "" : ""
    });
  }, [chat.selectedContact, contacts]);

  useEffect(() => {
    if (!settings) return;
    setRelayEndpointsDraft((settings.relayEndpoints || []).join("\n"));
  }, [settings]);

  const saveChat = async (nextChat) => {
    const saved = await invoke("save_chat", { chat: nextChat });
    setChat(saved);
    return saved;
  };

  const selectContact = async (id) => {
    const next = { ...chat, selectedContact: id, newContacts: (chat.newContacts || []).filter((c) => c !== id) };
    const thread = [...(next.threads[id] || [])].map((m) => (m.direction === "Incoming" ? { ...m, seen: true } : m));
    next.threads = { ...next.threads, [id]: thread };
    await saveChat(next);
  };

  const submitContact = async (event) => {
    event.preventDefault();
    const wayfarerId = contactDraft.wayfarerId.trim();
    const alias = contactDraft.alias.trim();
    if (!wayfarerId || !alias) return setStatus("Contact ID and alias are required");
    try {
      const nextContacts = await withNetworkPulse(() => invoke("upsert_contact", { request: { wayfarerId, alias } }));
      setContacts(nextContacts);
      await selectContact(wayfarerId);
      setStatus(`Saved contact ${alias}`);
    } catch (error) {
      setStatus(`Saving contact failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const clearSelection = async () => {
    const nextChat = { ...chat, selectedContact: null };
    await saveChat(nextChat);
    setStatus("Contact selection cleared");
  };

  const removeSelected = async () => {
    if (!selectedContactId) return setStatus("Select a contact to remove");
    try {
      const nextContacts = await withNetworkPulse(() => invoke("remove_contact", { wayfarerId: selectedContactId }));
      const nextChat = { ...chat, selectedContact: Object.keys(nextContacts)[0] || null, threads: { ...chat.threads } };
      delete nextChat.threads[selectedContactId];
      setContacts(nextContacts);
      await saveChat(nextChat);
      setStatus("Contact removed");
    } catch (error) {
      setStatus(`Contact removal failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const requestConfirmation = ({ title, body, confirmLabel, confirmClassName, onConfirm }) => {
    setConfirmDialog({ title, body, confirmLabel, confirmClassName, onConfirm });
  };

  const performDeleteMessage = async (msgId) => {
    if (!selectedContactId) return;
    try {
      const nextChat = {
        ...chat,
        threads: {
          ...chat.threads,
          [selectedContactId]: (chat.threads[selectedContactId] || []).filter((message) => message.msgId !== msgId)
        }
      };
      await saveChat(nextChat);
      setStatus("Message deleted");
    } catch (error) {
      setStatus(`Delete failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const deleteMessage = (msgId) => {
    requestConfirmation({
      title: "Delete message?",
      body: "This permanently removes the selected message from local chat history.",
      confirmLabel: "Delete",
      confirmClassName: "bg-red-600 text-white hover:bg-red-500",
      onConfirm: () => performDeleteMessage(msgId)
    });
  };

  const performClearThreadMessages = async () => {
    if (!selectedContactId) return setStatus("Select a thread first");
    try {
      const nextChat = {
        ...chat,
        threads: {
          ...chat.threads,
          [selectedContactId]: []
        }
      };
      await saveChat(nextChat);
      setStatus("Thread cleared");
    } catch (error) {
      setStatus(`Thread clear failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const clearThreadMessages = () => {
    requestConfirmation({
      title: "Clear this thread?",
      body: "This permanently removes all local messages in the current thread.",
      confirmLabel: "Clear Thread",
      confirmClassName: "bg-red-600 text-white hover:bg-red-500",
      onConfirm: performClearThreadMessages
    });
  };

  const performClearAllMessages = async () => {
    try {
      const nextChat = {
        ...chat,
        selectedContact: null,
        threads: {},
        newContacts: []
      };
      await saveChat(nextChat);
      setStatus("All local messages deleted");
    } catch (error) {
      setStatus(`Delete all failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const clearAllMessages = () => {
    requestConfirmation({
      title: "Delete all messages?",
      body: "This permanently removes all local messages on this client.",
      confirmLabel: "Delete All",
      confirmClassName: "bg-red-600 text-white hover:bg-red-500",
      onConfirm: performClearAllMessages
    });
  };

  const handleConfirmDialogProceed = async () => {
    const action = confirmDialog?.onConfirm;
    setConfirmDialog(null);
    if (typeof action === "function") {
      await action();
    }
  };

  const onAttachmentPick = async (event) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;

    const isImage = String(file.type || "").toLowerCase().startsWith("image/");
    const maxBytes = isImage ? 128 * 1024 * 1024 : 2 * 1024 * 1024;
    if (file.size > maxBytes) {
      setStatus(`Attachment too large (max ${isImage ? "128 MB" : "2 MB"})`);
      soundManager.play("error");
      return;
    }

    try {
      const contentB64 = await readFileAsBase64(file);
      setComposerAttachment({
        fileName: file.name,
        mimeType: file.type || "application/octet-stream",
        sizeBytes: file.size,
        contentB64
      });
      setStatus(`Attached ${file.name}`);
    } catch {
      setStatus("Failed to read attachment");
      soundManager.play("error");
    }
  };

  const clearAttachment = () => {
    setComposerAttachment(null);
  };

  const resolveMediaForMessage = useCallback(async (message) => {
    const key = mediaStateKey(message);
    const objectSha = String(message?.media?.objectSha256Hex || "").trim();
    if (!key || !objectSha) return null;
    if (resolvedMediaByKey[key]) return resolvedMediaByKey[key];
    if (loadingMediaByKey[key]) return null;

    setLoadingMediaByKey((prev) => ({ ...prev, [key]: true }));
    try {
      const payload = await invoke("get_completed_media_path", { request: { objectSha256Hex: objectSha } });
      const resolved = {
        objectSha256Hex: objectSha,
        fileName: String(message?.media?.fileName || message?.attachment?.fileName || "aethos-media"),
        mimeType: String(payload?.mime || message?.media?.mimeType || "application/octet-stream"),
        sizeBytes: Number(payload?.sizeBytes || message?.media?.totalBytes || 0),
        path: String(payload?.path || "").trim()
      };
      if (!resolved.path) {
        throw new Error("completed media path missing");
      }
      setResolvedMediaByKey((prev) => ({ ...prev, [key]: resolved }));
      return resolved;
    } catch (error) {
      setStatus(`Failed to load media: ${String(error)}`);
      soundManager.play("error");
      return null;
    } finally {
      setLoadingMediaByKey((prev) => {
        const next = { ...prev };
        delete next[key];
        return next;
      });
    }
  }, [loadingMediaByKey, resolvedMediaByKey]);

  const loadMediaForMessage = useCallback(async (message) => {
    const key = mediaStateKey(message);
    if (!key) return;
    const resolved = await resolveMediaForMessage(message);
    if (!resolved) return;
    const mime = String(resolved.mimeType || "").toLowerCase();
    if (!mime.startsWith("image/")) return;
    const src = convertFileSrc(resolved.path);
    setPreviewImageByKey((prev) => ({ ...prev, [key]: src }));
  }, [resolveMediaForMessage]);

  const downloadResolvedMedia = useCallback(async (message) => {
    const resolved = await resolveMediaForMessage(message);
    if (!resolved) return;
    const href = convertFileSrc(resolved.path);
    const link = document.createElement("a");
    link.href = href;
    link.download = sanitizeDownloadFileName(resolved.fileName || message?.attachment?.fileName || "", "aethos-media");
    document.body.appendChild(link);
    link.click();
    link.remove();
  }, [resolveMediaForMessage]);

  const openMediaInSystemViewer = useCallback(async (message) => {
    const objectSha = String(message?.media?.objectSha256Hex || "").trim();
    if (!objectSha) return;
    try {
      await invoke("open_completed_media_in_system_viewer", { request: { objectSha256Hex: objectSha } });
    } catch (error) {
      setStatus(`Failed to open media: ${String(error)}`);
      soundManager.play("error");
    }
  }, []);

  const copyDonateAddress = async () => {
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(DONATE_CRYPTO_ADDRESS);
      } else {
        const input = document.createElement("textarea");
        input.value = DONATE_CRYPTO_ADDRESS;
        input.setAttribute("readonly", "");
        input.style.position = "absolute";
        input.style.left = "-9999px";
        document.body.appendChild(input);
        input.select();
        const ok = document.execCommand("copy");
        input.remove();
        if (!ok) throw new Error("clipboard_copy_failed");
      }
      setStatus("Crypto donation address copied");
      setCopyToast("ETH address copied to clipboard");
    } catch {
      setStatus("Could not copy crypto donation address");
      soundManager.play("error");
    }
  };

  useEffect(() => {
    if (!copyToast) return;
    const timer = setTimeout(() => setCopyToast(""), 2200);
    return () => clearTimeout(timer);
  }, [copyToast]);

  const sendMessage = async () => {
    if (!selectedContactId) return setStatus("Select a contact before sending");
    if (!canSendToSelectedContact) {
      return setStatus("Cannot send to unresolved peer thread. Select a saved Wayfarer contact.");
    }
    const body = composer.trim();
    if (!body && !composerAttachment) return setStatus("Message must include text or a file");

    const localId = `ui-local-${Date.now()}-${Math.random().toString(16).slice(2, 10)}`;
    const contactId = selectedContactId;
    const pendingAttachment = composerAttachment;
    setComposer("");
    setComposerAttachment(null);
    setChat((prev) => {
      const next = { ...prev, threads: { ...prev.threads } };
      const thread = [...(next.threads[contactId] || [])];
      thread.push(createOptimisticOutgoingMessageWithAttachment(localId, body, pendingAttachment));
      next.threads[contactId] = thread;
      next.selectedContact = contactId;
      return next;
    });
    setStatus("Sending message...");

    try {
      const response = await withNetworkPulse(() => invoke("send_message", {
        request: {
          wayfarerId: contactId,
          body,
          attachment: pendingAttachment
            ? {
                fileName: pendingAttachment.fileName,
                mimeType: pendingAttachment.mimeType,
                sizeBytes: pendingAttachment.sizeBytes,
                contentB64: pendingAttachment.contentB64
              }
            : null
        }
      }));
      setChat(response.chat);
      setContacts(response.contacts);
      setStatus(`${response.encounterStatus} · ${tinyId(response.message.msgId)}`);
      soundManager.play("send");
    } catch (error) {
      const failure = String(error);
      setChat((prev) => {
        const next = { ...prev, threads: { ...prev.threads } };
        const thread = [...(next.threads[contactId] || [])];
        const idx = thread.findIndex((item) => item.msgId === localId);
        if (idx >= 0) {
          thread[idx] = {
            ...thread[idx],
            outboundState: { failed: { error: failure } },
            lastSyncError: failure,
            lastSyncAttemptUnixMs: Date.now()
          };
          next.threads[contactId] = thread;
        }
        return next;
      });
      setStatus(`Send failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const syncInbox = async () => {
    try {
      const response = await withNetworkPulse(() => invoke("sync_inbox"));
      setChat(response.chat);
      setContacts(response.contacts);
      setStatus(response.status);
      if ((response.pulledMessages || 0) > 0) {
        soundManager.play("sync");
      }
    } catch (error) {
      setStatus(`Inbox sync failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const generateShareQr = async () => {
    try {
      const qr = await withNetworkPulse(() => invoke("generate_share_qr", { wayfarerId: identity?.wayfarerId || null }));
      setShareQr(qr);
      setStatus("Share QR generated");
    } catch (error) {
      setStatus(`Share QR generation failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const announceGossip = async () => {
    const next = await withNetworkPulse(() => invoke("gossip_announce_now"));
    setGossipStatus(next);
    setStatus("LAN gossip announce queued");
  };

  const importQrFile = async (file) => {
    if (!file) return;
    try {
      const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
      const wayfarerId = await invoke("decode_wayfarer_id_from_qr_bytes", { bytes });
      const alias = contacts[wayfarerId] || `Contact ${tinyId(wayfarerId)}`;
      const nextContacts = await withNetworkPulse(() => invoke("upsert_contact", { request: { wayfarerId, alias } }));
      setContacts(nextContacts);
      await selectContact(wayfarerId);
      setStatus(`Imported contact ${tinyId(wayfarerId)}`);
    } catch (error) {
      setStatus(`QR import failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const saveSettings = async (event) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const payload = {
      ...settings,
      relaySyncEnabled: form.get("relay_sync_enabled") === "on",
      gossipSyncEnabled: form.get("gossip_sync_enabled") === "on",
      verboseLoggingEnabled: form.get("verbose_logging_enabled") === "on",
      enterToSend: form.get("enter_to_send") === "on",
      messageTtlSeconds: Number(form.get("message_ttl_seconds") || settings.messageTtlSeconds),
      relayEndpoints: String(relayEndpointsDraft || "")
        .split("\n")
        .map((v) => v.trim())
        .filter(Boolean)
    };
    try {
      const saved = await withNetworkPulse(() => invoke("update_settings", { settings: payload }));
      setSettings(saved);
      setRelayHealth(await invoke("relay_health_status"));
      setGossipStatus(await invoke("gossip_status"));
      setLogTail(await invoke("read_app_log", { maxLines: 500 }));
      setStatus("Settings saved");
    } catch (error) {
      setStatus(`Settings update failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const resetRelayEndpoints = async () => {
    if (!settings) return;
    const defaults = [DEFAULT_RELAY_ENDPOINT];
    try {
      const saved = await withNetworkPulse(() => invoke("update_settings", {
        settings: {
          ...settings,
          relayEndpoints: defaults
        }
      }));
      setSettings(saved);
      setRelayEndpointsDraft(defaults.join("\n"));
      setRelayHealth(await invoke("relay_health_status"));
      setStatus("Relay configuration reset to default");
    } catch (error) {
      setStatus(`Relay reset failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const toggleFullscreen = async () => {
    try {
      const appWindow = getCurrentWindow();
      const next = !(await appWindow.isFullscreen());
      await appWindow.setFullscreen(next);
      setIsFullscreen(next);
      setStatus(next ? "Entered fullscreen" : "Exited fullscreen");
    } catch (error) {
      setStatus(`Fullscreen toggle failed: ${String(error)}`);
      soundManager.play("error");
    }
  };

  const selectedName = selectedContactId ? contacts[selectedContactId] || tinyId(selectedContactId) : "none";
  const canSendToSelectedContact = /^[0-9a-f]{64}$/.test(selectedContactId || "");
  const networkActive = Date.now() - networkPulseTs < 2200;
  const relayOnline = relayHealth.chipState === "ok" || relayHealth.chipState === "warn";
  const gossipOnline = gossipStatus.enabled && gossipStatus.running;
  const filteredLogLines = useMemo(() => {
    const lines = (logTail.content || "").split("\n");
    const needles = LOG_FILTERS[logFilter] || [];
    if (needles.length === 0) return lines;
    return lines.filter((line) => {
      const lower = line.toLowerCase();
      return needles.some((needle) => lower.includes(needle.toLowerCase()));
    });
  }, [logTail.content, logFilter]);
  const filteredLogContent = filteredLogLines.join("\n");

  return (
    <div className="relative min-h-screen overflow-x-hidden bg-[radial-gradient(circle_at_15%_10%,rgba(76,122,255,.26),transparent_42%),radial-gradient(circle_at_88%_-10%,rgba(42,189,255,.2),transparent_35%),#060914] text-foreground">
      <div className="app-atmosphere" aria-hidden="true" />
      <div className="app-atmosphere-grid" aria-hidden="true" />
      <div className="mx-auto max-w-7xl p-2.5 md:p-3">

        <div className="hero-stack">
          <Card className="hero-banner mb-1.5 overflow-hidden border-indigo-300/20">
            <CardContent className="relative p-0">
              <div className="hero-glow" aria-hidden="true" />
              <div className="hero-wind" aria-hidden="true" />
              <div className="hero-stars" aria-hidden="true">
                {Array.from({ length: 10 }).map((_, idx) => (
                  <span key={idx} className="hero-star" style={{ animationDelay: `${idx * 0.9}s` }} />
                ))}
              </div>
              <div className="hero-layout">
                <div className="hero-copy">
                  <p className="inline-flex w-fit items-center gap-1.5 rounded-full border border-cyan-200/30 bg-cyan-300/10 px-2 py-0.5 text-[10px] font-semibold tracking-[0.18em] text-cyan-50/90">
                    <Wind className="h-3 w-3" />
                    DELAY-TOLERANT MESSAGING
                  </p>
                  <div className="hero-headline-row">
                    <CardTitle className="hero-title text-xl md:text-2xl">AETHOS</CardTitle>
                    <p className="hero-subtitle text-[11px] leading-relaxed text-blue-100/90 md:text-xs">
                      Unstoppable messaging across relay and local airwaves, even through disconnects.
                    </p>
                  </div>
                  <div className="hero-mesh-strip">
                    <div className={cn("network-orbit", networkActive ? "is-active" : "") }>
                      <span className="network-core" />
                      <span className="network-orbit-ring ring-a" />
                      <span className="network-orbit-ring ring-b" />
                      <span className="network-orbit-dot dot-a" />
                      <span className="network-orbit-dot dot-b" />
                      <span className="network-orbit-dot dot-c" />
                      <p className="network-label">mesh signal {networkActive ? "flowing" : "standing by"}</p>
                    </div>
                  </div>
                </div>
                <div className="hero-mark-wrap">
                  <div className="hero-mark-ring" />
                  <div className="hero-mark-shell">
                    <img src="/logo.png" alt="Aethos mark" className="hero-mark" />
                  </div>
                </div>
                <div className="hero-donate-actions">
                  <Button type="button" variant="secondary" size="sm" className="h-7 px-2 text-[11px]" onClick={copyDonateAddress}>
                    ETH
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="h-7 border border-cyan-200/20 bg-background/30 px-2 text-[11px] hover:bg-cyan-500/10"
                    onClick={async () => {
                      try {
                        await invoke("open_external_url", { url: DONATE_PAYPAL_URL });
                        setStatus("Opened PayPal donation link");
                      } catch {
                        setStatus("Could not open PayPal donation link");
                        soundManager.play("error");
                      }
                    }}
                  >
                    PayPal
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>

        <div className="tabs-dock mb-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            {TABS.map((t) => {
              const Icon = t.icon;
              return (
                <Button key={t.id} data-testid={`tab-${t.id}`} variant={tab === t.id ? "default" : "ghost"} className="h-9 gap-1.5 px-3" onClick={() => setTab(t.id)}>
                  <Icon className="h-4 w-4" />
                  {t.label}
                </Button>
              );
            })}
            <div className="flex items-center gap-2">
              <Button variant="ghost" className="h-9 w-9 p-0" onClick={toggleFullscreen} title={isFullscreen ? "Exit fullscreen" : "Enter fullscreen"}>
                {isFullscreen ? <Minimize2 className="h-4 w-4" /> : <Maximize2 className="h-4 w-4" />}
              </Button>
              <span className={cn("tab-status-dot", relayOnline ? "is-online" : "is-offline")} title="Relay status">
                <Wifi className="h-3.5 w-3.5" />
              </span>
              <span className={cn("tab-status-dot", gossipOnline ? "is-online" : "is-offline")} title="LAN gossip status">
                <BellRing className="h-3.5 w-3.5" />
              </span>
            </div>
          </div>
        </div>

        {copyToast ? (
          <div className="mb-2 inline-flex items-center rounded-md border border-cyan-300/40 bg-cyan-500/15 px-3 py-1.5 text-xs font-medium text-cyan-100">
            {copyToast}
          </div>
        ) : null}

        {updateNotice && !updateDismissed ? (
          <Card className="mb-2 border-cyan-300/30 bg-cyan-500/8">
            <CardContent className="flex flex-wrap items-center justify-between gap-2 p-3 text-sm">
              <div className="min-w-[260px] flex-1">
                <p className="font-semibold text-cyan-100">Update available: v{updateNotice.latestVersion}</p>
                <p className="text-xs text-cyan-100/85">
                  You are on v{updateNotice.currentVersion}. {updateNotice.notesSummary}
                </p>
              </div>
              <div className="flex flex-wrap gap-2">
                <Button
                  variant="secondary"
                  className="h-8"
                  onClick={() => invoke("open_external_url", { url: updateNotice.notesUrl })}
                >
                  Release Notes
                </Button>
                {updateNotice.downloadUrl ? (
                  <Button
                    className="h-8"
                    onClick={() => invoke("open_external_url", { url: updateNotice.downloadUrl })}
                  >
                    Download Update
                  </Button>
                ) : null}
                <Button variant="ghost" className="h-8" onClick={() => setUpdateDismissed(true)}>
                  Dismiss
                </Button>
              </div>
            </CardContent>
          </Card>
        ) : null}

        {tab === "chats" && (
          <div className="grid gap-3 lg:h-[calc(100vh-220px)] lg:grid-cols-[285px_1fr]">
            <Card className="flex min-h-[320px] flex-col lg:h-full">
              <CardHeader className="p-3 pb-1"><CardTitle className="text-base">Contacts</CardTitle></CardHeader>
              <CardContent className="min-h-0 flex-1 space-y-1.5 overflow-auto p-3 pt-1">
                {entries.map(([id, alias]) => {
                  const unread = (chat.threads[id] || []).filter((m) => m.direction === "Incoming" && !m.seen).length;
                  const isNew = (chat.newContacts || []).includes(id);
                  return (
                    <button data-testid={`chat-contact-${id}`} key={id} className={cn("w-full rounded-lg border p-2.5 text-left", id === selectedContactId ? "border-blue-300 bg-blue-500/20" : "border-border/60 bg-background/50")} onClick={() => selectContact(id)}>
                      <div className="flex items-center justify-between gap-2"><span className="truncate font-semibold">{alias || tinyId(id)}</span>{isNew ? <Badge className="bg-violet-500/20">NEW</Badge> : unread > 0 ? <Badge>{unread}</Badge> : null}</div>
                      <p className="mt-1 truncate text-xs text-muted-foreground">{tinyId(id)}</p>
                    </button>
                  );
                })}
              </CardContent>
            </Card>
            <Card className="flex min-h-[360px] flex-col lg:h-full">
              <CardHeader className="p-3 pb-1"><CardTitle className="text-base">{selectedName}</CardTitle></CardHeader>
              <CardContent className="flex min-h-0 flex-1 flex-col p-3 pt-1">
                <div ref={threadContainerRef} className="mb-1.5 min-h-0 flex-1 space-y-2 overflow-auto rounded-lg border border-border/60 bg-background/40 p-2.5">
                  {selectedThread.length === 0 ? <p className="text-sm text-muted-foreground">No messages in this thread yet.</p> : selectedThread.map((m) => (
                    <div
                      data-testid={`message-${m.msgId}`}
                      key={m.msgId}
                      className={cn("message-bubble max-w-[85%] rounded-xl px-3 py-2 text-sm", m.direction === "Incoming" ? "message-bubble-incoming" : "message-bubble-outgoing ml-auto", arrivingMessageIds[m.msgId] ? "message-arrive" : "") }
                    >
                      {m.text ? <p>{m.text}</p> : null}
                      {m.media ? (
                        <div className="mt-1.5 rounded-md border border-blue-300/30 bg-slate-900/40 p-2 text-xs" data-testid={`media-manifest-${m.media.transferId || m.msgId}`}>
                          <p className="truncate font-semibold">{m.media.fileName || m.attachment?.fileName || "image"}</p>
                          <p className="text-cyan-100/80">{formatBytes(m.media.totalBytes || 0)} · {m.media.mimeType || "image"}</p>
                          <p className="text-cyan-100/80" data-testid={`media-progress-${m.media.transferId || m.msgId}`}>
                            {mediaProgressText(m)}
                          </p>
                          <div className="mt-1 h-1.5 w-full overflow-hidden rounded bg-slate-800/80">
                            <div
                              className={cn("h-full transition-all duration-300", mediaStatus(m.media) === "failed" ? "bg-red-500" : "bg-cyan-400")}
                              style={{ width: `${m.direction === "Outgoing" ? outgoingMediaProgressPercent(m) : mediaProgressPercent(m.media)}%` }}
                            />
                          </div>

                          {(() => {
                            const key = mediaStateKey(m);
                            const status = mediaStatus(m.media);
                            const resolved = key ? resolvedMediaByKey[key] : null;
                            const preview = key ? previewImageByKey[key] : null;
                            const loading = key ? loadingMediaByKey[key] : false;
                            const mime = String(resolved?.mimeType || m.media?.mimeType || "").toLowerCase();
                            const isImage = mime.startsWith("image/");
                            const sizeBytes = Number(resolved?.sizeBytes || m.media?.totalBytes || 0);
                            const requiresClickToPreview = isImage && sizeBytes > MEDIA_PREVIEW_GATE_BYTES;
                            if (status !== "complete") {
                              return (
                                <p className="mt-1 text-cyan-100/70" data-testid={`media-placeholder-${m.media.transferId || m.msgId}`}>
                                  Waiting for all chunks and hash verification before rendering image.
                                </p>
                              );
                            }
                            return (
                              <div className="mt-1">
                                {isImage ? (
                                  <>
                                    {!preview ? (
                                      <Button
                                        variant="ghost"
                                        size="sm"
                                        className="mt-1 h-7 px-2"
                                        data-testid={`media-load-${m.media.transferId || m.msgId}`}
                                        disabled={loading}
                                        onClick={() => loadMediaForMessage(m)}
                                      >
                                        {loading
                                          ? "Resolving preview..."
                                          : requiresClickToPreview
                                            ? "Click to load preview"
                                            : "Load Image"}
                                      </Button>
                                    ) : null}
                                    {requiresClickToPreview && !preview ? (
                                      <p className="mt-1 text-cyan-100/70">
                                        Large image preview is opt-in to keep memory usage low.
                                      </p>
                                    ) : null}
                                  </>
                                ) : null}
                                {preview ? (
                                  <img
                                    src={preview}
                                    alt={resolved?.fileName || "received image"}
                                    className="max-h-52 w-auto rounded border border-cyan-300/30"
                                    data-testid={`media-image-${m.media.transferId || m.msgId}`}
                                  />
                                ) : null}
                                <div className="mt-1 flex flex-wrap gap-2">
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    className="h-7 px-2"
                                    onClick={() => downloadResolvedMedia(m)}
                                  >
                                    <FileDown className="mr-1 h-3.5 w-3.5" />Save As
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    className="h-7 px-2"
                                    onClick={() => openMediaInSystemViewer(m)}
                                  >
                                    Open in System Viewer
                                  </Button>
                                </div>
                              </div>
                            );
                          })()}
                        </div>
                      ) : null}
                      {m.attachment && !m.media ? (
                        <div className="mt-1.5 rounded-md border border-cyan-300/30 bg-slate-900/35 p-2 text-xs">
                          <p className="truncate font-semibold">{m.attachment.fileName}</p>
                          <p className="text-cyan-100/75">{formatBytes(m.attachment.sizeBytes)} · {m.attachment.mimeType || "file"}</p>
                          {m.attachment.contentB64 ? (
                            <Button
                              variant="ghost"
                              size="sm"
                              className="mt-1 h-7 px-2"
                              onClick={() => {
                                const bin = atob(m.attachment.contentB64);
                                const bytes = new Uint8Array(bin.length);
                                for (let i = 0; i < bin.length; i += 1) bytes[i] = bin.charCodeAt(i);
                                const blob = new Blob([bytes], { type: m.attachment.mimeType || "application/octet-stream" });
                                const href = URL.createObjectURL(blob);
                                const link = document.createElement("a");
                                link.href = href;
                                link.download = sanitizeDownloadFileName(m.attachment.fileName || "", "aethos-attachment");
                                document.body.appendChild(link);
                                link.click();
                                link.remove();
                                setTimeout(() => URL.revokeObjectURL(href), 400);
                              }}
                            >
                              <FileDown className="mr-1 h-3.5 w-3.5" />Download
                            </Button>
                          ) : null}
                        </div>
                      ) : null}
                      <div className="mt-1 flex items-center justify-end gap-2 text-[11px] text-slate-300">
                        <span>{formatMessageTimestamp(m)}</span>
                        {formatOutgoingStatus(m) ? <span className="text-cyan-200/90">{formatOutgoingStatus(m)}</span> : null}
                        <Button
                          data-testid={`message-delete-${m.msgId}`}
                          variant="ghost"
                          size="sm"
                          className="h-6 px-2 text-[10px]"
                          onClick={() => deleteMessage(m.msgId)}
                        >
                          Delete
                        </Button>
                      </div>
                    </div>
                  ))}
                </div>
                <div className="mb-1 flex items-center gap-2">
                  <input data-testid="chat-attachment-input" ref={attachmentInputRef} type="file" className="hidden" onChange={onAttachmentPick} />
                  <Button
                    data-testid="chat-attach-file"
                    variant="ghost"
                    className="h-8"
                    disabled={!canSendToSelectedContact}
                    onClick={() => attachmentInputRef.current?.click()}
                  >
                    <Paperclip className="mr-1 h-4 w-4" />Attach File
                  </Button>
                  {composerAttachment ? (
                    <div className="flex min-w-0 items-center gap-2 rounded-md border border-cyan-300/30 bg-cyan-500/10 px-2 py-1 text-xs">
                      <span className="truncate">{composerAttachment.fileName}</span>
                      <span className="text-cyan-200/80">{formatBytes(composerAttachment.sizeBytes)}</span>
                      <Button variant="ghost" size="sm" className="h-6 px-2" onClick={clearAttachment}>Remove</Button>
                    </div>
                  ) : null}
                  <Button
                    data-testid="chat-clear-thread"
                    className="ml-auto h-8"
                    disabled={!selectedContactId || selectedThread.length === 0}
                    onClick={clearThreadMessages}
                  >
                    Clear Thread
                  </Button>
                </div>
                <Textarea
                  data-testid="chat-composer"
                  value={composer}
                  disabled={!canSendToSelectedContact}
                  onChange={(e) => setComposer(e.target.value)}
                  onKeyDown={(event) => {
                    const enterToSend = settings?.enterToSend !== false;
                    if (!enterToSend) return;
                    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing || event.repeat) return;
                    event.preventDefault();
                    void sendMessage();
                  }}
                  rows={2}
                  placeholder={!canSendToSelectedContact ? "Cannot reply to unresolved peer. Select a saved contact." : (settings?.enterToSend === false ? "Write a message or drop in an emoji/file..." : "Write a message... (Enter to send, Shift+Enter newline)")}
                />
                <Button
                  data-testid="chat-send"
                  className="mt-1 h-9 w-full"
                  disabled={!canSendToSelectedContact || (!composer.trim() && !composerAttachment)}
                  onClick={sendMessage}
                >
                  Send
                </Button>
              </CardContent>
            </Card>
          </div>
        )}

        {confirmDialog ? (
          <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/65 p-4">
            <div className="w-full max-w-md rounded-xl border border-border/70 bg-slate-900 p-4 shadow-xl">
              <h3 className="text-base font-semibold text-slate-100">{confirmDialog.title}</h3>
              <p className="mt-1 text-sm text-slate-300">{confirmDialog.body}</p>
              <div className="mt-4 flex justify-end gap-2">
                <Button variant="ghost" onClick={() => setConfirmDialog(null)}>Cancel</Button>
                <Button className={confirmDialog.confirmClassName || ""} onClick={handleConfirmDialogProceed}>
                  {confirmDialog.confirmLabel || "Confirm"}
                </Button>
              </div>
            </div>
          </div>
        ) : null}

        {tab === "contacts" && (
          <div className="grid gap-3 lg:grid-cols-2">
            <Card>
              <CardHeader className="p-3 pb-1"><CardTitle className="text-base">Manage Contacts</CardTitle></CardHeader>
              <CardContent className="p-3 pt-1">
                <p className="mb-2 text-sm text-muted-foreground">Editing: <strong>{selectedName}</strong> {selectedContactId ? `(${tinyId(selectedContactId)})` : ""}</p>
                <form className="space-y-2" onSubmit={submitContact}>
                  <Input data-testid="contact-wayfarer-id" name="wayfarer_id" value={contactDraft.wayfarerId} onChange={(e) => setContactDraft((d) => ({ ...d, wayfarerId: e.target.value }))} placeholder="64 lowercase hex chars" />
                  <Input data-testid="contact-alias" name="alias" value={contactDraft.alias} onChange={(e) => setContactDraft((d) => ({ ...d, alias: e.target.value }))} placeholder="Display name" />
                  <div className="flex flex-wrap gap-2">
                    <Button data-testid="contact-save" type="submit">Save Contact</Button>
                    <Button type="button" variant="destructive" onClick={removeSelected}>Remove Selected</Button>
                    <Button type="button" variant="ghost" onClick={clearSelection}>Clear Selection</Button>
                    <label className="inline-flex cursor-pointer items-center">
                      <input className="hidden" type="file" accept="image/png,image/jpeg,image/jpg" onChange={(e) => importQrFile(e.target.files && e.target.files[0])} />
                      <span className="inline-flex h-10 items-center rounded-md border border-border/70 px-4 text-sm font-medium hover:bg-white/10">Import QR</span>
                    </label>
                  </div>
                </form>
              </CardContent>
            </Card>
            <Card>
              <CardHeader className="p-3 pb-1"><CardTitle className="text-base">Address Book</CardTitle></CardHeader>
              <CardContent className="space-y-1.5 p-3 pt-1">
                {entries.map(([id, alias]) => (
                  <button key={id} className={cn("w-full rounded-lg border p-2.5 text-left", id === selectedContactId ? "border-cyan-300 bg-cyan-500/20" : "border-border/60 bg-background/50")} onClick={() => selectContact(id)}>
                    <p className="truncate font-semibold">{alias || tinyId(id)}</p>
                    <p className="truncate text-xs text-muted-foreground">{id}</p>
                  </button>
                ))}
              </CardContent>
            </Card>
          </div>
        )}

        {tab === "share" && (
          <Card>
            <CardHeader className="p-3 pb-1"><CardTitle className="text-base">Share Your Wayfarer ID</CardTitle></CardHeader>
            <CardContent className="space-y-2 p-3 pt-1">
              <pre data-testid="share-wayfarer-id" className="overflow-auto rounded-lg border border-border/60 bg-background/60 p-3 text-xs">{identity?.wayfarerId || "Unavailable"}</pre>
              <div className="flex flex-wrap gap-2">
                <Button variant="secondary" onClick={generateShareQr}><QrCode className="mr-2 h-4 w-4" />Generate QR</Button>
                {shareQr?.pngBase64 ? (
                  <Button variant="ghost" onClick={() => {
                    const bin = atob(shareQr.pngBase64);
                    const bytes = new Uint8Array(bin.length);
                    for (let i = 0; i < bin.length; i += 1) bytes[i] = bin.charCodeAt(i);
                    const blob = new Blob([bytes], { type: "image/png" });
                    const href = URL.createObjectURL(blob);
                    const link = document.createElement("a");
                    link.href = href;
                    link.download = "aethos-share-wayfarer-qr.png";
                    document.body.appendChild(link);
                    link.click();
                    link.remove();
                    setTimeout(() => URL.revokeObjectURL(href), 400);
                    setStatus("QR image downloaded");
                  }}>Download QR</Button>
                ) : null}
              </div>
              {shareQr?.pngBase64 ? <img alt="Share QR" src={`data:image/png;base64,${shareQr.pngBase64}`} className="max-w-xs rounded-xl border border-border bg-white p-2" /> : null}
            </CardContent>
          </Card>
        )}

        {tab === "settings" && settings && (
          <div className="space-y-3">
            <div className="grid gap-3 lg:grid-cols-2">
              <Card>
              <CardHeader className="p-3 pb-1"><CardTitle className="text-base">Sync Settings</CardTitle></CardHeader>
              <CardContent className="p-3 pt-1">
                <form className="space-y-2" onSubmit={saveSettings}>
                  <label className="flex items-center gap-2 text-sm"><input data-testid="settings-relay-sync" type="checkbox" name="relay_sync_enabled" defaultChecked={settings.relaySyncEnabled} /> Enable relay sync</label>
                  <label className="flex items-center gap-2 text-sm"><input data-testid="settings-gossip-sync" type="checkbox" name="gossip_sync_enabled" defaultChecked={settings.gossipSyncEnabled} /> Enable LAN gossip sync</label>
                  <label className="flex items-center gap-2 text-sm"><input data-testid="settings-verbose-logging" type="checkbox" name="verbose_logging_enabled" defaultChecked={settings.verboseLoggingEnabled} /> Enable verbose logging</label>
                  <label className="flex items-center gap-2 text-sm"><input data-testid="settings-enter-to-send" type="checkbox" name="enter_to_send" defaultChecked={settings.enterToSend !== false} /> Enter sends message (Shift+Enter newline)</label>
                  <Input name="message_ttl_seconds" type="number" defaultValue={settings.messageTtlSeconds} />
                  <Textarea
                    data-testid="settings-relay-endpoints"
                    name="relay_endpoints"
                    rows={4}
                    value={relayEndpointsDraft}
                    onChange={(event) => setRelayEndpointsDraft(event.target.value)}
                  />
                  <div className="flex flex-wrap gap-2">
                    <Button data-testid="settings-save" type="submit"><CheckCircle2 className="mr-2 h-4 w-4" />Save Settings</Button>
                    <Button type="button" variant="secondary" onClick={resetRelayEndpoints}>Reset Relay Default</Button>
                    <Button data-testid="settings-announce-gossip" type="button" variant="ghost" onClick={announceGossip}>Announce LAN Gossip</Button>
                    <Button type="button" variant="destructive" onClick={clearAllMessages}>Delete ALL Messages</Button>
                  </div>
                </form>
              </CardContent>
              </Card>
              <Card>
              <CardHeader className="p-3 pb-1"><CardTitle className="text-base">Diagnostics</CardTitle></CardHeader>
              <CardContent className="space-y-1.5 p-3 pt-1 text-sm text-muted-foreground">
                <p>{diagnostics ? `${diagnostics.platform}/${diagnostics.arch}` : "Loading"}</p>
                <p>Primary relay: {relayHealth.primaryStatus}</p>
                <p>Secondary relay: {relayHealth.secondaryStatus}</p>
                <p>Gossip event: {gossipStatus.lastEvent}</p>
                <div className="pt-2">
                  <Button variant="secondary" size="sm" onClick={async () => {
                    const reports = await invoke("run_relay_diagnostics", { request: { relayEndpoints: settings.relayEndpoints, authToken: null, traceItemId: null } });
                    setRelayReports(reports);
                    setStatus("Relay diagnostics completed");
                  }}>Run Relay Diagnostics</Button>
                </div>
                {relayReports.length > 0 ? relayReports.map((report, idx) => (
                  <div key={idx} className="rounded border border-border/50 bg-background/40 p-2 text-xs text-foreground">
                    <p className="font-semibold">{report.relayHttp}</p>
                    <p>Handshake: {report.handshakeStatus}</p>
                    <p>Encounter: {report.encounterStatus}</p>
                  </div>
                )) : null}

                <div className="mt-2 rounded border border-border/50 bg-background/30 p-2 text-xs">
                  <p className="mb-1 font-semibold text-foreground">Media Transfer Debug</p>
                  {mediaDebugRows.length === 0 ? (
                    <p className="text-muted-foreground" data-testid="media-debug-empty">No media transfers tracked yet.</p>
                  ) : (
                    <div className="max-h-48 overflow-auto space-y-1" data-testid="media-debug-list">
                      {mediaDebugRows.map((row) => (
                        <div key={row.key} className="rounded border border-border/40 bg-background/50 p-1.5">
                          <p className="text-foreground" data-testid={`media-debug-${row.transferId}`}>
                            {row.direction} · {row.contactAlias}
                          </p>
                          <p>transfer={tinyId(row.transferId)} status={row.status}</p>
                          <p>chunks={row.receivedChunks}/{row.chunkCount}</p>
                          <p>lastError={row.lastError}</p>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </CardContent>
              </Card>
            </div>

            <Card>
              <CardHeader className="p-3 pb-1">
                <CardTitle className="text-base">Live Client Logs</CardTitle>
                <p data-testid="settings-log-path" className="text-xs text-muted-foreground">{logTail.logFilePath || "Log path unavailable"}</p>
              </CardHeader>
              <CardContent className="p-3 pt-1">
                <div className="mb-2 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                  <Badge className="bg-slate-700/40">Showing {filteredLogLines.length} / {logTail.totalLines} lines</Badge>
                  <label className="inline-flex items-center gap-2">
                    <span>Filter</span>
                    <select
                      className="rounded border border-border/70 bg-background px-2 py-1 text-xs"
                      value={logFilter}
                      onChange={(event) => setLogFilter(event.target.value)}
                    >
                      <option value="all">All</option>
                      <option value="errors">Errors</option>
                      <option value="send">Send Path</option>
                      <option value="gossip">Gossip</option>
                      <option value="relay">Relay</option>
                      <option value="transfer">Transfer/Import</option>
                    </select>
                  </label>
                  <label className="inline-flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={logFollow}
                      onChange={(event) => setLogFollow(event.target.checked)}
                    />
                    Auto-follow
                  </label>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={async () => setLogTail(await invoke("read_app_log", { maxLines: 500 }))}
                  >
                    Refresh Logs
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => setLogStreaming((value) => !value)}
                  >
                    {logStreaming ? "Stop Live Logs" : "Start Live Logs"}
                  </Button>
                  <Button
                    size="sm"
                    variant="destructive"
                    onClick={async () => {
                      const cleared = await invoke("clear_app_log");
                      setLogTail(cleared);
                      setStatus("Client logs cleared");
                    }}
                  >
                    Clear Logs
                  </Button>
                </div>

                <div
                  ref={logContainerRef}
                  onScroll={(event) => {
                    const el = event.currentTarget;
                    const nearBottom = el.scrollHeight - (el.scrollTop + el.clientHeight) < 20;
                    if (logFollow && !nearBottom) setLogFollow(false);
                  }}
                  className="max-h-[280px] overflow-auto rounded-lg border border-border/60 bg-black/40 p-2.5"
                >
                  <pre className="whitespace-pre-wrap text-xs leading-5 text-cyan-100">{filteredLogContent || "(no log lines for current filter)"}</pre>
                </div>
              </CardContent>
            </Card>
          </div>
        )}

        <Card className="mt-2.5">
          <CardContent className="flex items-center gap-2 py-2 text-sm text-cyan-100">
            <Badge className="bg-cyan-500/20 border-cyan-400/40">Status</Badge>
            <span data-testid="status-text">{status}</span>
          </CardContent>
        </Card>

        {showSplash ? (
          <div className={cn("fixed inset-0 z-50 flex items-center justify-center bg-[rgba(5,8,18,0.45)] backdrop-blur-xl transition-opacity duration-1000", splashFade ? "opacity-0" : "opacity-100")}>
            <div className="rounded-2xl border border-cyan-300/30 bg-slate-900/40 px-10 py-8 text-center shadow-2xl">
              <img src="/logo.png" alt="Aethos logo" className="mx-auto mb-4 h-20 w-20 rounded-2xl object-cover" />
              <h2 className="text-2xl font-semibold tracking-wide">Aethos</h2>
              <p className="mt-1 text-sm text-cyan-100/80">Loading secure channels...</p>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}
