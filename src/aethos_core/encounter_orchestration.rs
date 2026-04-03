use std::fmt;

use crate::aethos_core::logging::log_verbose;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerAdapter {
    LanDatagram,
    RelayWebSocket,
    BleBootstrap,
}

impl BearerAdapter {
    fn as_str(self) -> &'static str {
        match self {
            Self::LanDatagram => "lan-datagram",
            Self::RelayWebSocket => "relay-websocket",
            Self::BleBootstrap => "ble-bootstrap",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerRole {
    DiscoveryBootstrap,
    ControlExchange,
    BulkTransfer,
}

impl BearerRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::DiscoveryBootstrap => "discovery-bootstrap",
            Self::ControlExchange => "control-exchange",
            Self::BulkTransfer => "bulk-transfer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncounterLifecycleState {
    DiscoveryObserved,
    ControlExchangeStarted,
    TransferPlanned,
    TransferExecuting,
    TransferInterrupted,
    TransferResumed,
    TransferCompleted,
    Closed,
}

impl EncounterLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::DiscoveryObserved => "discovery-observed",
            Self::ControlExchangeStarted => "control-exchange-started",
            Self::TransferPlanned => "transfer-planned",
            Self::TransferExecuting => "transfer-executing",
            Self::TransferInterrupted => "transfer-interrupted",
            Self::TransferResumed => "transfer-resumed",
            Self::TransferCompleted => "transfer-completed",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionReason {
    InitialSelection,
    #[allow(dead_code)]
    HealthUpgrade,
    #[allow(dead_code)]
    HealthDowngrade,
    NoProgress,
    RemoteClose,
    Resume,
}

impl TransitionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::InitialSelection => "initial-selection",
            Self::HealthUpgrade => "health-upgrade",
            Self::HealthDowngrade => "health-downgrade",
            Self::NoProgress => "no-progress",
            Self::RemoteClose => "remote-close",
            Self::Resume => "resume",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncounterAuditPoint {
    pub stage: &'static str,
    pub module_path: &'static str,
    pub function_name: &'static str,
}

impl fmt::Display for EncounterAuditPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{} stage={}",
            self.module_path, self.function_name, self.stage
        )
    }
}

pub fn canonical_audit_points() -> Vec<EncounterAuditPoint> {
    vec![
        EncounterAuditPoint {
            stage: "discovery-bootstrap",
            module_path: "src/main.rs",
            function_name: "start_background_gossip_sync",
        },
        EncounterAuditPoint {
            stage: "control-exchange",
            module_path: "src/relay/client.rs",
            function_name: "run_relay_encounter_gossipv1",
        },
        EncounterAuditPoint {
            stage: "bulk-transfer",
            module_path: "src/aethos_core/gossip_sync.rs",
            function_name: "transfer_items_for_request_with_shadow_context",
        },
        EncounterAuditPoint {
            stage: "interruption-resume",
            module_path: "src/relay/client.rs",
            function_name: "run_relay_round_on_socket",
        },
    ]
}

#[derive(Debug, Clone)]
pub struct EncounterManager {
    encounter_id: String,
    local_wayfarer_id: String,
    peer_wayfarer_id: Option<String>,
    state: EncounterLifecycleState,
    discovery_bearer: Option<BearerAdapter>,
    control_bearer: Option<BearerAdapter>,
    transfer_bearer: Option<BearerAdapter>,
}

impl EncounterManager {
    pub fn new(
        encounter_id: impl Into<String>,
        local_wayfarer_id: impl Into<String>,
        peer_wayfarer_id: Option<String>,
    ) -> Self {
        Self {
            encounter_id: encounter_id.into(),
            local_wayfarer_id: local_wayfarer_id.into(),
            peer_wayfarer_id,
            state: EncounterLifecycleState::DiscoveryObserved,
            discovery_bearer: None,
            control_bearer: None,
            transfer_bearer: None,
        }
    }

    pub fn observe_discovery(&mut self, bearer: BearerAdapter, observed_at_unix_ms: u64) {
        self.discovery_bearer = Some(bearer);
        self.state = EncounterLifecycleState::DiscoveryObserved;
        self.log_event(
            "encounter_discovery_observed",
            BearerRole::DiscoveryBootstrap,
            Some(bearer),
            observed_at_unix_ms,
            "reason=discovery-signal",
        );
    }

    pub fn start_control_exchange(
        &mut self,
        bearer: BearerAdapter,
        reason: TransitionReason,
        at_unix_ms: u64,
    ) {
        self.control_bearer = Some(bearer);
        self.state = EncounterLifecycleState::ControlExchangeStarted;
        self.log_event(
            "encounter_control_exchange_started",
            BearerRole::ControlExchange,
            Some(bearer),
            at_unix_ms,
            &format!("reason={}", reason.as_str()),
        );
    }

    pub fn set_transfer_bearer(
        &mut self,
        bearer: BearerAdapter,
        reason: TransitionReason,
        at_unix_ms: u64,
    ) {
        let previous = self.transfer_bearer;
        self.transfer_bearer = Some(bearer);
        self.state = EncounterLifecycleState::TransferExecuting;
        let event = if previous.is_some() {
            "encounter_bearer_upgrade_applied"
        } else {
            "encounter_bearer_selected"
        };
        self.log_event(
            event,
            BearerRole::BulkTransfer,
            Some(bearer),
            at_unix_ms,
            &format!(
                "reason={} previous_transfer_bearer={} next_transfer_bearer={}",
                reason.as_str(),
                previous.map(|item| item.as_str()).unwrap_or("none"),
                bearer.as_str(),
            ),
        );
    }

    pub fn downgrade_transfer_bearer(
        &mut self,
        next: BearerAdapter,
        reason: TransitionReason,
        at_unix_ms: u64,
    ) {
        let previous = self.transfer_bearer;
        self.transfer_bearer = Some(next);
        self.state = EncounterLifecycleState::TransferInterrupted;
        self.log_event(
            "encounter_bearer_downgrade_applied",
            BearerRole::BulkTransfer,
            Some(next),
            at_unix_ms,
            &format!(
                "reason={} previous_transfer_bearer={} next_transfer_bearer={}",
                reason.as_str(),
                previous.map(|item| item.as_str()).unwrap_or("none"),
                next.as_str(),
            ),
        );
    }

    pub fn record_scheduler_plan(
        &mut self,
        plan_id: &str,
        selected_items: usize,
        stop_reason: &str,
        tie_break_reason: &str,
        at_unix_ms: u64,
    ) {
        self.state = EncounterLifecycleState::TransferPlanned;
        self.log_event(
            "encounter_scheduler_plan_emitted",
            BearerRole::BulkTransfer,
            self.transfer_bearer,
            at_unix_ms,
            &format!(
                "plan_id={} selected_items={} stop_reason={} tie_break_reason={}",
                plan_id, selected_items, stop_reason, tie_break_reason
            ),
        );
    }

    pub fn record_scheduler_execution(
        &mut self,
        plan_id: &str,
        executed_items: usize,
        at_unix_ms: u64,
    ) {
        self.state = EncounterLifecycleState::TransferExecuting;
        self.log_event(
            "encounter_scheduler_plan_executed",
            BearerRole::BulkTransfer,
            self.transfer_bearer,
            at_unix_ms,
            &format!("plan_id={} executed_items={}", plan_id, executed_items),
        );
    }

    pub fn mark_interrupted(&mut self, reason: TransitionReason, at_unix_ms: u64) {
        self.state = EncounterLifecycleState::TransferInterrupted;
        self.log_event(
            "encounter_transfer_interrupted",
            BearerRole::BulkTransfer,
            self.transfer_bearer,
            at_unix_ms,
            &format!("reason={}", reason.as_str()),
        );
    }

    pub fn mark_resumed(&mut self, at_unix_ms: u64) {
        self.state = EncounterLifecycleState::TransferResumed;
        self.log_event(
            "encounter_transfer_resumed",
            BearerRole::BulkTransfer,
            self.transfer_bearer,
            at_unix_ms,
            &format!("reason={}", TransitionReason::Resume.as_str()),
        );
    }

    pub fn mark_transfer_completed(&mut self, transferred_items: usize, at_unix_ms: u64) {
        self.state = EncounterLifecycleState::TransferCompleted;
        self.log_event(
            "encounter_transfer_completed",
            BearerRole::BulkTransfer,
            self.transfer_bearer,
            at_unix_ms,
            &format!("transferred_items={}", transferred_items),
        );
    }

    pub fn close(&mut self, at_unix_ms: u64) {
        self.state = EncounterLifecycleState::Closed;
        self.log_event(
            "encounter_closed",
            BearerRole::ControlExchange,
            self.control_bearer,
            at_unix_ms,
            "reason=encounter-complete",
        );
    }

    fn log_event(
        &self,
        event: &str,
        role: BearerRole,
        bearer: Option<BearerAdapter>,
        at_unix_ms: u64,
        fields: &str,
    ) {
        log_verbose(&format!(
            "{} encounter_id={} local_wayfarer_id={} peer_wayfarer_id={} state={} bearer_role={} bearer_adapter={} at_unix_ms={} {}",
            event,
            self.encounter_id,
            self.local_wayfarer_id,
            self.peer_wayfarer_id.as_deref().unwrap_or("none"),
            self.state.as_str(),
            role.as_str(),
            bearer.map(|item| item.as_str()).unwrap_or("none"),
            at_unix_ms,
            fields,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_audit_points_cover_required_stages() {
        let points = canonical_audit_points();
        assert!(points
            .iter()
            .any(|item| item.stage == "discovery-bootstrap"));
        assert!(points.iter().any(|item| item.stage == "control-exchange"));
        assert!(points.iter().any(|item| item.stage == "bulk-transfer"));
        assert!(points
            .iter()
            .any(|item| item.stage == "interruption-resume"));
    }

    #[test]
    fn manager_tracks_upgrade_downgrade_and_resume() {
        let mut manager = EncounterManager::new("enc-1", "local-1", Some("peer-1".to_string()));
        manager.observe_discovery(BearerAdapter::LanDatagram, 1000);
        manager.start_control_exchange(
            BearerAdapter::RelayWebSocket,
            TransitionReason::InitialSelection,
            1100,
        );
        manager.set_transfer_bearer(
            BearerAdapter::RelayWebSocket,
            TransitionReason::InitialSelection,
            1200,
        );
        manager.set_transfer_bearer(
            BearerAdapter::LanDatagram,
            TransitionReason::HealthUpgrade,
            1300,
        );
        manager.downgrade_transfer_bearer(
            BearerAdapter::RelayWebSocket,
            TransitionReason::HealthDowngrade,
            1400,
        );
        manager.mark_resumed(1500);
        manager.mark_transfer_completed(5, 1600);
        manager.close(1700);
        assert_eq!(manager.state, EncounterLifecycleState::Closed);
    }

    #[test]
    fn canonical_fixture_bundle_for_gossip_v1_is_present() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("third_party/aethos/Fixtures/Protocol/gossip-v1");
        let required = [
            "README.md",
            "hello.json",
            "summary.json",
            "request.json",
            "transfer.json",
            "receipt.json",
            "relay_ingest.json",
        ];
        for file_name in required {
            assert!(
                root.join(file_name).exists(),
                "missing canonical fixture file: {}",
                file_name
            );
        }
    }
}
