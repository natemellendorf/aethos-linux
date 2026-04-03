use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverySignal {
    pub peer_hint: String,
    pub observed_at_unix_ms: u64,
    pub rssi: Option<i16>,
    pub bearer_type: &'static str,
    pub source: &'static str,
}

pub trait BleDiscoverySource {
    fn poll_signals(&mut self, now_unix_ms: u64) -> Vec<DiscoverySignal>;
}

pub struct BleDiscoveryGate {
    dedupe_window: Duration,
    last_seen_by_peer: HashMap<String, u64>,
}

impl BleDiscoveryGate {
    pub fn new(dedupe_window: Duration) -> Self {
        Self {
            dedupe_window,
            last_seen_by_peer: HashMap::new(),
        }
    }

    pub fn poll_ready(
        &mut self,
        source: &mut dyn BleDiscoverySource,
        now_unix_ms: u64,
    ) -> Vec<DiscoverySignal> {
        let mut out = Vec::new();
        for signal in source.poll_signals(now_unix_ms) {
            let allow = self
                .last_seen_by_peer
                .get(&signal.peer_hint)
                .map(|previous| {
                    signal.observed_at_unix_ms.saturating_sub(*previous)
                        >= self.dedupe_window.as_millis() as u64
                })
                .unwrap_or(true);
            if allow {
                self.last_seen_by_peer
                    .insert(signal.peer_hint.clone(), signal.observed_at_unix_ms);
                out.push(signal);
            }
        }
        out
    }
}

pub enum DiscoveryAdapter {
    Simulated(SimulatedBleDiscoverySource),
    BluetoothCtl(BluetoothCtlDiscoverySource),
    Disabled,
}

impl BleDiscoverySource for DiscoveryAdapter {
    fn poll_signals(&mut self, now_unix_ms: u64) -> Vec<DiscoverySignal> {
        match self {
            Self::Simulated(source) => source.poll_signals(now_unix_ms),
            Self::BluetoothCtl(source) => source.poll_signals(now_unix_ms),
            Self::Disabled => Vec::new(),
        }
    }
}

pub fn discovery_adapter_from_env() -> DiscoveryAdapter {
    if let Ok(raw) = std::env::var("AETHOS_BLE_SIMULATED_SIGNALS") {
        let simulated = SimulatedBleDiscoverySource::from_env_string(&raw);
        if !simulated.pending.is_empty() {
            return DiscoveryAdapter::Simulated(simulated);
        }
    }

    let enabled = std::env::var("AETHOS_DISABLE_BLE")
        .ok()
        .map(|value| value.trim() != "1")
        .unwrap_or(true);
    if !enabled {
        return DiscoveryAdapter::Disabled;
    }

    DiscoveryAdapter::BluetoothCtl(BluetoothCtlDiscoverySource::default())
}

#[derive(Debug, Clone)]
struct SimulatedSignalSeed {
    peer_hint: String,
    rssi: Option<i16>,
}

pub struct SimulatedBleDiscoverySource {
    pending: Vec<SimulatedSignalSeed>,
    emitted_once: bool,
}

impl SimulatedBleDiscoverySource {
    fn from_env_string(raw: &str) -> Self {
        let pending = raw
            .split(',')
            .filter_map(|entry| {
                let trimmed = entry.trim();
                if trimmed.is_empty() {
                    return None;
                }
                let mut parts = trimmed.split('@');
                let peer_hint = parts.next()?.trim().to_string();
                if peer_hint.is_empty() {
                    return None;
                }
                let rssi = parts
                    .next()
                    .and_then(|value| value.trim().parse::<i16>().ok());
                Some(SimulatedSignalSeed { peer_hint, rssi })
            })
            .collect::<Vec<_>>();
        Self {
            pending,
            emitted_once: false,
        }
    }
}

impl BleDiscoverySource for SimulatedBleDiscoverySource {
    fn poll_signals(&mut self, now_unix_ms: u64) -> Vec<DiscoverySignal> {
        if self.emitted_once {
            return Vec::new();
        }
        self.emitted_once = true;
        self.pending
            .iter()
            .map(|seed| DiscoverySignal {
                peer_hint: seed.peer_hint.clone(),
                observed_at_unix_ms: now_unix_ms,
                rssi: seed.rssi,
                bearer_type: "ble",
                source: "simulated",
            })
            .collect()
    }
}

pub struct BluetoothCtlDiscoverySource {
    poll_interval: Duration,
    last_poll: Option<Instant>,
}

impl Default for BluetoothCtlDiscoverySource {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(8),
            last_poll: None,
        }
    }
}

impl BleDiscoverySource for BluetoothCtlDiscoverySource {
    fn poll_signals(&mut self, now_unix_ms: u64) -> Vec<DiscoverySignal> {
        if let Some(last_poll) = self.last_poll {
            if last_poll.elapsed() < self.poll_interval {
                return Vec::new();
            }
        }
        self.last_poll = Some(Instant::now());

        let output = match Command::new("bluetoothctl").arg("devices").output() {
            Ok(output) => output,
            Err(_) => return Vec::new(),
        };
        if !output.status.success() {
            return Vec::new();
        }

        parse_bluetoothctl_devices(&String::from_utf8_lossy(&output.stdout), now_unix_ms)
    }
}

fn parse_bluetoothctl_devices(raw: &str, now_unix_ms: u64) -> Vec<DiscoverySignal> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with("Device ") {
                return None;
            }
            let mut parts = trimmed.split_whitespace();
            let _device = parts.next();
            let mac = parts.next()?.to_ascii_lowercase();
            let name = parts.collect::<Vec<_>>().join(" ");
            let peer_hint = if let Some(stripped) = name.strip_prefix("aethos-") {
                stripped.trim().to_string()
            } else if let Some(stripped) = name.strip_prefix("AETHOS-") {
                stripped.trim().to_string()
            } else {
                format!("ble:{}", mac)
            };
            Some(DiscoverySignal {
                peer_hint,
                observed_at_unix_ms: now_unix_ms,
                rssi: None,
                bearer_type: "ble",
                source: "bluetoothctl",
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_source_emits_once_for_deterministic_harness() {
        let mut source =
            SimulatedBleDiscoverySource::from_env_string("peer-alpha@-55,peer-beta@-49");
        let first = source.poll_signals(1000);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].peer_hint, "peer-alpha");
        assert_eq!(first[0].rssi, Some(-55));
        let second = source.poll_signals(2000);
        assert!(second.is_empty());
    }

    #[test]
    fn gate_dedupes_duplicate_signals_within_window() {
        struct InlineSource {
            frames: Vec<Vec<DiscoverySignal>>,
        }
        impl BleDiscoverySource for InlineSource {
            fn poll_signals(&mut self, _now_unix_ms: u64) -> Vec<DiscoverySignal> {
                if self.frames.is_empty() {
                    return Vec::new();
                }
                self.frames.remove(0)
            }
        }

        let signal = DiscoverySignal {
            peer_hint: "peer-1".to_string(),
            observed_at_unix_ms: 1000,
            rssi: Some(-60),
            bearer_type: "ble",
            source: "test",
        };
        let signal_soon = DiscoverySignal {
            observed_at_unix_ms: 1500,
            ..signal.clone()
        };
        let signal_later = DiscoverySignal {
            observed_at_unix_ms: 9000,
            ..signal
        };
        let mut source = InlineSource {
            frames: vec![vec![signal_soon.clone()], vec![signal_later.clone()]],
        };
        let mut gate = BleDiscoveryGate::new(Duration::from_secs(5));
        gate.last_seen_by_peer.insert("peer-1".to_string(), 1000);

        let first = gate.poll_ready(&mut source, 1500);
        assert!(first.is_empty());
        let second = gate.poll_ready(&mut source, 9000);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].observed_at_unix_ms, 9000);
    }

    #[test]
    fn parser_supports_aethos_name_or_ephemeral_fallback() {
        let raw = "Device AA:BB:CC:DD:EE:FF aethos-wayfarer-peer\nDevice 11:22:33:44:55:66 Other Device\n";
        let parsed = parse_bluetoothctl_devices(raw, 123);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].peer_hint, "wayfarer-peer");
        assert_eq!(parsed[1].peer_hint, "ble:11:22:33:44:55:66");
    }
}
