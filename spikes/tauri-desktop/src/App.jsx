import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  BellRing,
  CheckCircle2,
  ContactRound,
  MessageCircle,
  QrCode,
  RefreshCcw,
  Settings,
  Share2,
  Wifi,
  Wind
} from "lucide-react";

import { Badge } from "./components/ui/badge";
import { Button } from "./components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./components/ui/card";
import { Input } from "./components/ui/input";
import { Textarea } from "./components/ui/textarea";
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

function tinyId(id = "") {
  return id ? `${id.slice(0, 10)}...${id.slice(-8)}` : "-";
}

function formatMessageTimestamp(message) {
  const raw = Number(message?.createdAtUnix ?? message?.timestamp);
  if (!Number.isFinite(raw) || raw <= 0) return String(message?.timestamp || "");
  const ms = raw > 1_000_000_000_000 ? raw : raw * 1000;
  return new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" });
}

export default function App() {
  const [tab, setTab] = useState("chats");
  const [status, setStatus] = useState("Loading app state...");
  const [identity, setIdentity] = useState(null);
  const [settings, setSettings] = useState(null);
  const [contacts, setContacts] = useState({});
  const [chat, setChat] = useState({ selectedContact: null, threads: {}, newContacts: [] });
  const [relayReports, setRelayReports] = useState([]);
  const [relayHealth, setRelayHealth] = useState(emptyRelay);
  const [gossipStatus, setGossipStatus] = useState(emptyGossip);
  const [shareQr, setShareQr] = useState(null);
  const [diagnostics, setDiagnostics] = useState(null);
  const [composer, setComposer] = useState("");
  const [contactDraft, setContactDraft] = useState({ wayfarerId: "", alias: "" });
  const [showSplash, setShowSplash] = useState(true);
  const [splashFade, setSplashFade] = useState(false);
  const [networkPulseTs, setNetworkPulseTs] = useState(0);
  const [logTail, setLogTail] = useState({ logFilePath: "", totalLines: 0, shownLines: 0, content: "" });
  const [logFollow, setLogFollow] = useState(true);
  const [logStreaming, setLogStreaming] = useState(true);
  const [logFilter, setLogFilter] = useState("all");
  const [arrivingMessageIds, setArrivingMessageIds] = useState({});
  const logContainerRef = useRef(null);
  const threadContainerRef = useRef(null);
  const seenThreadMessageIdsRef = useRef(new Set());

  const entries = useMemo(
    () => Object.entries(contacts).sort((a, b) => (a[1] || "").localeCompare(b[1] || "", undefined, { sensitivity: "base" })),
    [contacts]
  );

  const selectedContactId = chat.selectedContact;
  const selectedThread = selectedContactId ? chat.threads[selectedContactId] || [] : [];

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
    }
  };

  const sendMessage = async () => {
    if (!selectedContactId) return setStatus("Select a contact before sending");
    if (!composer.trim()) return setStatus("Message body cannot be empty");
    try {
      const response = await withNetworkPulse(() => invoke("send_message", { request: { wayfarerId: selectedContactId, body: composer.trim() } }));
      setChat(response.chat);
      setContacts(response.contacts);
      setComposer("");
      setStatus(`${response.encounterStatus} · ${tinyId(response.message.msgId)}`);
    } catch (error) {
      setStatus(`Send failed: ${String(error)}`);
    }
  };

  const syncInbox = async () => {
    try {
      const response = await withNetworkPulse(() => invoke("sync_inbox"));
      setChat(response.chat);
      setContacts(response.contacts);
      setStatus(response.status);
    } catch (error) {
      setStatus(`Inbox sync failed: ${String(error)}`);
    }
  };

  const generateShareQr = async () => {
    try {
      const qr = await withNetworkPulse(() => invoke("generate_share_qr", { wayfarerId: identity?.wayfarerId || null }));
      setShareQr(qr);
      setStatus("Share QR generated");
    } catch (error) {
      setStatus(`Share QR generation failed: ${String(error)}`);
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
      relayEndpoints: String(form.get("relay_endpoints") || "")
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
    }
  };

  const selectedName = selectedContactId ? contacts[selectedContactId] || tinyId(selectedContactId) : "none";
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
      <div className="mx-auto max-w-7xl p-5">

        <div className="hero-stack">
          <Card className="hero-banner mb-4 overflow-hidden border-indigo-300/20">
            <CardContent className="relative p-0">
              <div className="hero-glow" aria-hidden="true" />
              <div className="hero-wind" aria-hidden="true" />
              <div className="hero-stars" aria-hidden="true">
                {Array.from({ length: 10 }).map((_, idx) => (
                  <span key={idx} className="hero-star" style={{ animationDelay: `${idx * 0.9}s` }} />
                ))}
              </div>
              <div className="relative grid gap-6 px-6 py-7 md:grid-cols-[1.4fr_1fr] md:px-8">
                <div className="space-y-4">
                  <p className="inline-flex items-center gap-2 rounded-full border border-cyan-200/30 bg-cyan-300/10 px-3 py-1 text-xs font-semibold tracking-[0.22em] text-cyan-50/90">
                    <Wind className="h-3.5 w-3.5" />
                    DELAY-TOLERANT MESSAGING
                  </p>
                  <CardTitle className="hero-title text-4xl md:text-5xl">AETHOS</CardTitle>
                  <p className="max-w-xl text-sm leading-relaxed text-blue-100/90 md:text-base">
                    Messages float across relay and local airwaves, surviving gaps and reconnects so conversations keep moving.
                  </p>
                  <div className="hero-mesh-strip">
                    <div className={cn("network-orbit", networkActive ? "is-active" : "")}>
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
              </div>
            </CardContent>
          </Card>
        </div>

        <div className="tabs-dock mb-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            {TABS.map((t) => {
              const Icon = t.icon;
              return (
                <Button key={t.id} variant={tab === t.id ? "default" : "ghost"} className="gap-2" onClick={() => setTab(t.id)}>
                  <Icon className="h-4 w-4" />
                  {t.label}
                </Button>
              );
            })}
            <div className="flex items-center gap-2">
              <span className={cn("tab-status-dot", relayOnline ? "is-online" : "is-offline")} title="Relay status">
                <Wifi className="h-3.5 w-3.5" />
              </span>
              <span className={cn("tab-status-dot", gossipOnline ? "is-online" : "is-offline")} title="LAN gossip status">
                <BellRing className="h-3.5 w-3.5" />
              </span>
            </div>
          </div>
        </div>

        {tab === "chats" && (
          <div className="grid gap-4 lg:grid-cols-[300px_1fr]">
            <Card>
              <CardHeader><CardTitle>Contacts</CardTitle></CardHeader>
              <CardContent className="space-y-2">
                {entries.map(([id, alias]) => {
                  const unread = (chat.threads[id] || []).filter((m) => m.direction === "Incoming" && !m.seen).length;
                  const isNew = (chat.newContacts || []).includes(id);
                  return (
                    <button key={id} className={cn("w-full rounded-lg border p-3 text-left", id === selectedContactId ? "border-blue-300 bg-blue-500/20" : "border-border/60 bg-background/50")} onClick={() => selectContact(id)}>
                      <div className="flex items-center justify-between gap-2"><span className="truncate font-semibold">{alias || tinyId(id)}</span>{isNew ? <Badge className="bg-violet-500/20">NEW</Badge> : unread > 0 ? <Badge>{unread}</Badge> : null}</div>
                      <p className="mt-1 truncate text-xs text-muted-foreground">{tinyId(id)}</p>
                    </button>
                  );
                })}
              </CardContent>
            </Card>
            <Card>
              <CardHeader><CardTitle>{selectedName}</CardTitle></CardHeader>
              <CardContent>
                <div ref={threadContainerRef} className="mb-3 max-h-[360px] space-y-2 overflow-auto rounded-lg border border-border/60 bg-background/40 p-3">
                  {selectedThread.length === 0 ? <p className="text-sm text-muted-foreground">No messages in this thread yet.</p> : selectedThread.map((m) => (
                    <div key={m.msgId} className={cn("message-bubble max-w-[85%] rounded-xl px-3 py-2 text-sm", m.direction === "Incoming" ? "message-bubble-incoming" : "message-bubble-outgoing ml-auto", arrivingMessageIds[m.msgId] ? "message-arrive" : "") }>
                      <p>{m.text}</p>
                      <p className="mt-1 text-[11px] text-slate-300">{formatMessageTimestamp(m)}</p>
                    </div>
                  ))}
                </div>
                <div className="mb-2 flex gap-2"><Button variant="secondary" onClick={syncInbox}><RefreshCcw className="mr-2 h-4 w-4" />Sync Inbox</Button></div>
                <Textarea
                  value={composer}
                  onChange={(e) => setComposer(e.target.value)}
                  onKeyDown={(event) => {
                    const enterToSend = settings?.enterToSend !== false;
                    if (!enterToSend) return;
                    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing || event.repeat) return;
                    event.preventDefault();
                    void sendMessage();
                  }}
                  rows={3}
                  placeholder={settings?.enterToSend === false ? "Write a message..." : "Write a message... (Enter to send, Shift+Enter newline)"}
                />
                <Button className="mt-2 w-full" onClick={sendMessage}>Send</Button>
              </CardContent>
            </Card>
          </div>
        )}

        {tab === "contacts" && (
          <div className="grid gap-4 lg:grid-cols-2">
            <Card>
              <CardHeader><CardTitle>Manage Contacts</CardTitle></CardHeader>
              <CardContent>
                <p className="mb-3 text-sm text-muted-foreground">Editing: <strong>{selectedName}</strong> {selectedContactId ? `(${tinyId(selectedContactId)})` : ""}</p>
                <form className="space-y-3" onSubmit={submitContact}>
                  <Input name="wayfarer_id" value={contactDraft.wayfarerId} onChange={(e) => setContactDraft((d) => ({ ...d, wayfarerId: e.target.value }))} placeholder="64 lowercase hex chars" />
                  <Input name="alias" value={contactDraft.alias} onChange={(e) => setContactDraft((d) => ({ ...d, alias: e.target.value }))} placeholder="Display name" />
                  <div className="flex flex-wrap gap-2">
                    <Button type="submit">Save Contact</Button>
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
              <CardHeader><CardTitle>Address Book</CardTitle></CardHeader>
              <CardContent className="space-y-2">
                {entries.map(([id, alias]) => (
                  <button key={id} className={cn("w-full rounded-lg border p-3 text-left", id === selectedContactId ? "border-cyan-300 bg-cyan-500/20" : "border-border/60 bg-background/50")} onClick={() => selectContact(id)}>
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
            <CardHeader><CardTitle>Share Your Wayfarer ID</CardTitle></CardHeader>
            <CardContent className="space-y-3">
              <pre className="overflow-auto rounded-lg border border-border/60 bg-background/60 p-3 text-xs">{identity?.wayfarerId || "Unavailable"}</pre>
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
          <div className="space-y-4">
            <div className="grid gap-4 lg:grid-cols-2">
              <Card>
              <CardHeader><CardTitle>Sync Settings</CardTitle></CardHeader>
              <CardContent>
                <form className="space-y-3" onSubmit={saveSettings}>
                  <label className="flex items-center gap-2 text-sm"><input type="checkbox" name="relay_sync_enabled" defaultChecked={settings.relaySyncEnabled} /> Enable relay sync</label>
                  <label className="flex items-center gap-2 text-sm"><input type="checkbox" name="gossip_sync_enabled" defaultChecked={settings.gossipSyncEnabled} /> Enable LAN gossip sync</label>
                  <label className="flex items-center gap-2 text-sm"><input type="checkbox" name="verbose_logging_enabled" defaultChecked={settings.verboseLoggingEnabled} /> Enable verbose logging</label>
                  <label className="flex items-center gap-2 text-sm"><input type="checkbox" name="enter_to_send" defaultChecked={settings.enterToSend !== false} /> Enter sends message (Shift+Enter newline)</label>
                  <Input name="message_ttl_seconds" type="number" defaultValue={settings.messageTtlSeconds} />
                  <Textarea name="relay_endpoints" rows={4} defaultValue={(settings.relayEndpoints || []).join("\n")} />
                  <div className="flex flex-wrap gap-2">
                    <Button type="submit"><CheckCircle2 className="mr-2 h-4 w-4" />Save Settings</Button>
                    <Button type="button" variant="ghost" onClick={announceGossip}>Announce LAN Gossip</Button>
                  </div>
                </form>
              </CardContent>
              </Card>
              <Card>
              <CardHeader><CardTitle>Diagnostics</CardTitle></CardHeader>
              <CardContent className="space-y-2 text-sm text-muted-foreground">
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
              </CardContent>
              </Card>
            </div>

            <Card>
              <CardHeader>
                <CardTitle>Live Client Logs</CardTitle>
                <p className="text-xs text-muted-foreground">{logTail.logFilePath || "Log path unavailable"}</p>
              </CardHeader>
              <CardContent>
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
                  className="max-h-[320px] overflow-auto rounded-lg border border-border/60 bg-black/40 p-3"
                >
                  <pre className="whitespace-pre-wrap text-xs leading-5 text-cyan-100">{filteredLogContent || "(no log lines for current filter)"}</pre>
                </div>
              </CardContent>
            </Card>
          </div>
        )}

        <Card className="mt-4">
          <CardContent className="flex items-center gap-2 py-3 text-sm text-cyan-100">
            <Badge className="bg-cyan-500/20 border-cyan-400/40">Status</Badge>
            <span>{status}</span>
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
