use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncounterScoreWeights {
    pub scarcity: i32,
    pub safety: i32,
    pub expiry: i32,
    pub stagnation: i32,
    pub proximity: i32,
    pub size: i32,
    pub intent: i32,
    pub content_class: i32,
}

impl EncounterScoreWeights {
    pub const CANONICAL_V1: Self = Self {
        scarcity: 26,
        safety: 22,
        expiry: 16,
        stagnation: 12,
        proximity: 10,
        size: 8,
        intent: 4,
        content_class: 2,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EncounterClass {
    Blink,
    Short,
    Durable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncounterTieBreakReason {
    None,
    SizeBytes,
    ExpiryAtUnixMs,
    KnownReplicaCount,
    LastForwardedAtUnixMs,
    DestinationRank,
    ItemId,
}

impl EncounterTieBreakReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SizeBytes => "sizeBytes",
            Self::ExpiryAtUnixMs => "expiryAtUnixMs",
            Self::KnownReplicaCount => "knownReplicaCount",
            Self::LastForwardedAtUnixMs => "lastForwardedAtUnixMs",
            Self::DestinationRank => "destinationRank",
            Self::ItemId => "itemID",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncounterSelectionReason {
    SelectedPrefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncounterSelectionStopReason {
    Completed,
    BudgetItemsExhausted,
    BudgetBytesExhausted,
    EncounterTimeExhausted,
    DurableRatioCapReached,
    NoEligibleItems,
}

impl EncounterSelectionStopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::BudgetItemsExhausted => "budget-items-exhausted",
            Self::BudgetBytesExhausted => "budget-bytes-exhausted",
            Self::EncounterTimeExhausted => "encounter-time-exhausted",
            Self::DurableRatioCapReached => "durable-ratio-cap-reached",
            Self::NoEligibleItems => "no-eligible-items",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncounterScoreComponents {
    pub scarcity: f64,
    pub safety: f64,
    pub expiry: f64,
    pub stagnation: f64,
    pub proximity: f64,
    pub size: f64,
    pub intent: f64,
    pub content_class: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncounterScoreBreakdown {
    pub item_id: String,
    pub components: EncounterScoreComponents,
    pub score_numerator: i32,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProximityClass {
    DestinationPeer,
    LikelyCloser,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BudgetProfile {
    pub max_items: i32,
    pub max_bytes: i32,
    pub max_duration_ms: Option<i32>,
    pub durable_cargo_ratio_cap: Option<f64>,
    pub preferred_transfer_unit_bytes: i32,
    pub expiry_urgency_horizon_ms: u64,
    pub stagnation_horizon_ms: u64,
    pub target_replica_count_default: i32,
}

impl BudgetProfile {
    #[allow(dead_code)]
    pub fn new(max_items: i32, max_bytes: i32) -> Self {
        Self {
            max_items,
            max_bytes,
            max_duration_ms: None,
            durable_cargo_ratio_cap: None,
            preferred_transfer_unit_bytes: 32_768,
            expiry_urgency_horizon_ms: 900_000,
            stagnation_horizon_ms: 3_600_000,
            target_replica_count_default: 6,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CargoItem {
    pub item_id: String,
    pub tier: i32,
    pub size_bytes: i32,
    pub expiry_at_unix_ms: u64,
    pub known_replica_count: Option<i32>,
    pub target_replica_count: Option<i32>,
    pub durably_stored: Option<bool>,
    pub relay_ingested: Option<bool>,
    pub receipt_coverage: Option<f64>,
    pub last_forwarded_at_unix_ms: Option<u64>,
    pub proximity_class: Option<ProximityClass>,
    pub explicit_user_initiated: Option<bool>,
    pub content_class_score: Option<f64>,
    pub destination_rank: i32,
    pub estimated_duration_ms: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedCargoItem {
    pub cargo_item: CargoItem,
    pub score_breakdown: EncounterScoreBreakdown,
    pub tie_break_reason: EncounterTieBreakReason,
    pub selection_reason: Option<EncounterSelectionReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncounterSchedulerResult {
    pub ranked_items: Vec<RankedCargoItem>,
    pub selected_prefix: Vec<RankedCargoItem>,
    pub score_breakdowns: Vec<EncounterScoreBreakdown>,
    pub stop_reason: EncounterSelectionStopReason,
    pub tie_break_reason: EncounterTieBreakReason,
}

impl EncounterSchedulerResult {
    pub fn ranking_order(&self) -> Vec<String> {
        self.ranked_items
            .iter()
            .map(|item| item.cargo_item.item_id.clone())
            .collect()
    }

    pub fn selected_prefix_item_ids(&self) -> Vec<String> {
        self.selected_prefix
            .iter()
            .map(|item| item.cargo_item.item_id.clone())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    InvalidBudgetMaxItems(i32),
    InvalidBudgetMaxBytes(i32),
    InvalidBudgetMaxDurationMs(i32),
    InvalidBudgetPreferredTransferUnitBytes(i32),
    InvalidBudgetTargetReplicaCountDefault(i32),
    InvalidTier { item_id: String, value: i32 },
    InvalidSizeBytes { item_id: String, value: i32 },
    InvalidDestinationRank { item_id: String, value: i32 },
    InvalidKnownReplicaCount { item_id: String, value: i32 },
    InvalidTargetReplicaCount { item_id: String, value: i32 },
    InvalidEstimatedDurationMs { item_id: String, value: i32 },
    InvalidItemId(String),
    DuplicateItemId(String),
}

#[derive(Debug, Clone, Copy)]
pub struct EncounterSchedulerV1 {
    score_weights: EncounterScoreWeights,
    clock_skew_tolerance_ms: u64,
}

#[derive(Debug, Clone)]
struct ScoredItem {
    cargo_item: CargoItem,
    known_replica_count: i32,
    last_forwarded_at_unix_ms: u64,
    score_breakdown: EncounterScoreBreakdown,
}

impl EncounterSchedulerV1 {
    pub fn new() -> Self {
        Self {
            score_weights: EncounterScoreWeights::CANONICAL_V1,
            clock_skew_tolerance_ms: 30_000,
        }
    }

    pub fn schedule(
        &self,
        _encounter_class: EncounterClass,
        budget: &BudgetProfile,
        now_unix_ms: u64,
        cargo_items: &[CargoItem],
    ) -> Result<EncounterSchedulerResult, SchedulerError> {
        self.validate_budget(budget)?;
        let validated = self.validate_and_filter_eligible(cargo_items, budget, now_unix_ms)?;
        if validated.is_empty() {
            return Ok(EncounterSchedulerResult {
                ranked_items: Vec::new(),
                selected_prefix: Vec::new(),
                score_breakdowns: Vec::new(),
                stop_reason: EncounterSelectionStopReason::NoEligibleItems,
                tie_break_reason: EncounterTieBreakReason::None,
            });
        }

        let mut ranked_scored: Vec<ScoredItem> = validated
            .iter()
            .map(|item| self.score(item, budget, now_unix_ms))
            .collect();
        ranked_scored.sort_by(|lhs, rhs| {
            if self.ranks_before(lhs, rhs) {
                std::cmp::Ordering::Less
            } else if self.ranks_before(rhs, lhs) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });

        let mut ranked_items = Vec::with_capacity(ranked_scored.len());
        let mut global_tie_break_reason = EncounterTieBreakReason::None;
        for (index, scored) in ranked_scored.iter().enumerate() {
            let tie_reason = if index == 0 {
                EncounterTieBreakReason::None
            } else {
                self.tie_break_reason_between(&ranked_scored[index - 1], scored)
            };
            if global_tie_break_reason == EncounterTieBreakReason::None
                && tie_reason != EncounterTieBreakReason::None
            {
                global_tie_break_reason = tie_reason;
            }
            ranked_items.push(RankedCargoItem {
                cargo_item: scored.cargo_item.clone(),
                score_breakdown: scored.score_breakdown.clone(),
                tie_break_reason: tie_reason,
                selection_reason: None,
            });
        }

        let mut selected_indices = Vec::with_capacity(std::cmp::min(
            ranked_items.len(),
            budget.max_items.max(0) as usize,
        ));
        let mut selected_total_bytes = 0i32;
        let mut selected_durable_cargo_bytes = 0i32;
        let mut selected_duration_ms = 0i32;
        let durable_cargo_ratio_cap = budget.durable_cargo_ratio_cap.map(clamp01);
        let mut stop_reason = EncounterSelectionStopReason::Completed;

        for (index, ranked_item) in ranked_items.iter().enumerate() {
            if selected_indices.len() >= budget.max_items.max(0) as usize {
                stop_reason = EncounterSelectionStopReason::BudgetItemsExhausted;
                break;
            }

            let candidate = &ranked_item.cargo_item;
            let projected_total_bytes = selected_total_bytes.saturating_add(candidate.size_bytes);
            if projected_total_bytes > budget.max_bytes {
                stop_reason = EncounterSelectionStopReason::BudgetBytesExhausted;
                break;
            }

            if let Some(max_duration_ms) = budget.max_duration_ms {
                let projected_duration_ms = selected_duration_ms
                    .saturating_add(candidate.estimated_duration_ms.unwrap_or(0));
                if projected_duration_ms > max_duration_ms {
                    stop_reason = EncounterSelectionStopReason::EncounterTimeExhausted;
                    break;
                }
            }

            if let Some(durable_cargo_ratio_cap) = durable_cargo_ratio_cap {
                let projected_durable_cargo_bytes = selected_durable_cargo_bytes.saturating_add(
                    if is_durable_cargo_tier(candidate.tier) {
                        candidate.size_bytes
                    } else {
                        0
                    },
                );
                let projected_durable_cargo_ratio = if projected_total_bytes > 0 {
                    projected_durable_cargo_bytes as f64 / projected_total_bytes as f64
                } else {
                    0.0
                };
                if projected_durable_cargo_ratio > durable_cargo_ratio_cap {
                    stop_reason = EncounterSelectionStopReason::DurableRatioCapReached;
                    break;
                }
            }

            selected_indices.push(index);
            selected_total_bytes = projected_total_bytes;
            selected_duration_ms =
                selected_duration_ms.saturating_add(candidate.estimated_duration_ms.unwrap_or(0));
            if is_durable_cargo_tier(candidate.tier) {
                selected_durable_cargo_bytes =
                    selected_durable_cargo_bytes.saturating_add(candidate.size_bytes);
            }
        }

        let selected_index_set: std::collections::BTreeSet<usize> =
            selected_indices.iter().copied().collect();
        let ranked_with_selection: Vec<RankedCargoItem> = ranked_items
            .into_iter()
            .enumerate()
            .map(|(index, mut ranked_item)| {
                if selected_index_set.contains(&index) {
                    ranked_item.selection_reason = Some(EncounterSelectionReason::SelectedPrefix);
                }
                ranked_item
            })
            .collect();

        let selected_prefix = selected_indices
            .iter()
            .map(|index| ranked_with_selection[*index].clone())
            .collect::<Vec<_>>();

        Ok(EncounterSchedulerResult {
            score_breakdowns: ranked_with_selection
                .iter()
                .map(|ranked| ranked.score_breakdown.clone())
                .collect(),
            ranked_items: ranked_with_selection,
            selected_prefix,
            stop_reason,
            tie_break_reason: global_tie_break_reason,
        })
    }

    fn validate_budget(&self, budget: &BudgetProfile) -> Result<(), SchedulerError> {
        if budget.max_items < 0 {
            return Err(SchedulerError::InvalidBudgetMaxItems(budget.max_items));
        }
        if budget.max_bytes < 0 {
            return Err(SchedulerError::InvalidBudgetMaxBytes(budget.max_bytes));
        }
        if let Some(max_duration_ms) = budget.max_duration_ms {
            if max_duration_ms < 0 {
                return Err(SchedulerError::InvalidBudgetMaxDurationMs(max_duration_ms));
            }
        }
        if budget.preferred_transfer_unit_bytes < 1 {
            return Err(SchedulerError::InvalidBudgetPreferredTransferUnitBytes(
                budget.preferred_transfer_unit_bytes,
            ));
        }
        if budget.target_replica_count_default < 1 {
            return Err(SchedulerError::InvalidBudgetTargetReplicaCountDefault(
                budget.target_replica_count_default,
            ));
        }
        Ok(())
    }

    fn validate_and_filter_eligible(
        &self,
        cargo_items: &[CargoItem],
        budget: &BudgetProfile,
        now_unix_ms: u64,
    ) -> Result<Vec<CargoItem>, SchedulerError> {
        let mut seen = std::collections::BTreeSet::new();
        let expiry_threshold = saturated_add(now_unix_ms, self.clock_skew_tolerance_ms);
        let mut eligible = Vec::with_capacity(cargo_items.len());

        for item in cargo_items {
            if !is_valid_item_id(&item.item_id) {
                return Err(SchedulerError::InvalidItemId(item.item_id.clone()));
            }
            if !seen.insert(item.item_id.clone()) {
                return Err(SchedulerError::DuplicateItemId(item.item_id.clone()));
            }
            if !(0..=5).contains(&item.tier) {
                return Err(SchedulerError::InvalidTier {
                    item_id: item.item_id.clone(),
                    value: item.tier,
                });
            }
            if item.size_bytes < 1 {
                return Err(SchedulerError::InvalidSizeBytes {
                    item_id: item.item_id.clone(),
                    value: item.size_bytes,
                });
            }
            if item.destination_rank < 0 {
                return Err(SchedulerError::InvalidDestinationRank {
                    item_id: item.item_id.clone(),
                    value: item.destination_rank,
                });
            }
            if let Some(value) = item.known_replica_count {
                if value < 0 {
                    return Err(SchedulerError::InvalidKnownReplicaCount {
                        item_id: item.item_id.clone(),
                        value,
                    });
                }
            }
            if let Some(value) = item.target_replica_count {
                if value < 1 {
                    return Err(SchedulerError::InvalidTargetReplicaCount {
                        item_id: item.item_id.clone(),
                        value,
                    });
                }
            }
            if let Some(value) = item.estimated_duration_ms {
                if value < 0 {
                    return Err(SchedulerError::InvalidEstimatedDurationMs {
                        item_id: item.item_id.clone(),
                        value,
                    });
                }
            }

            let _ = std::cmp::max(
                1,
                item.target_replica_count
                    .unwrap_or(budget.target_replica_count_default),
            );
            if expiry_threshold < item.expiry_at_unix_ms {
                eligible.push(item.clone());
            }
        }
        Ok(eligible)
    }

    fn score(&self, item: &CargoItem, budget: &BudgetProfile, now_unix_ms: u64) -> ScoredItem {
        let known_replica_count = item.known_replica_count.unwrap_or(0);
        let target_replica_count = std::cmp::max(
            1,
            item.target_replica_count
                .unwrap_or(budget.target_replica_count_default),
        );
        let durably_stored = item.durably_stored.unwrap_or(false);
        let relay_ingested = item.relay_ingested.unwrap_or(false);
        let receipt_coverage = clamp01(item.receipt_coverage.unwrap_or(0.0));
        let last_forwarded_at_unix_ms = item.last_forwarded_at_unix_ms.unwrap_or(0);
        let proximity_class = item.proximity_class.unwrap_or(ProximityClass::Other);
        let explicit_user_initiated = item.explicit_user_initiated.unwrap_or(false);
        let content_class_score = clamp01(item.content_class_score.unwrap_or(0.0));

        let scarcity_raw =
            clamp01(1.0 - (known_replica_count as f64 / target_replica_count as f64).min(1.0));
        let durable_risk = if durably_stored { 0.0 } else { 1.0 };
        let relay_risk = if relay_ingested { 0.0 } else { 1.0 };
        let receipt_risk = 1.0 - receipt_coverage;
        let safety_raw = clamp01(durable_risk * 0.45 + relay_risk * 0.35 + receipt_risk * 0.20);

        let ttl_ms = int_difference(item.expiry_at_unix_ms, now_unix_ms);
        let expiry_raw = clamp01(
            1.0 - (ttl_ms as f64 / std::cmp::max(1, budget.expiry_urgency_horizon_ms) as f64)
                .min(1.0),
        );

        let idle_ms = int_difference(now_unix_ms, last_forwarded_at_unix_ms);
        let stagnation_raw = clamp01(
            (idle_ms as f64 / std::cmp::max(1, budget.stagnation_horizon_ms) as f64).min(1.0),
        );

        let proximity_raw = match proximity_class {
            ProximityClass::DestinationPeer => 1.0,
            ProximityClass::LikelyCloser => 0.6,
            ProximityClass::Other => 0.0,
        };

        let size_denominator =
            (std::cmp::max(1, budget.preferred_transfer_unit_bytes) as f64).ln_1p();
        let size_raw = if size_denominator == 0.0 {
            0.0
        } else {
            let size_ratio = (item.size_bytes as f64).ln_1p() / size_denominator;
            clamp01(1.0 - size_ratio.min(1.0))
        };

        let intent_raw = if explicit_user_initiated { 1.0 } else { 0.0 };

        let scarcity_u = quantize_millionths(scarcity_raw);
        let safety_u = quantize_millionths(safety_raw);
        let expiry_u = quantize_millionths(expiry_raw);
        let stagnation_u = quantize_millionths(stagnation_raw);
        let proximity_u = quantize_millionths(proximity_raw);
        let size_u = quantize_millionths(size_raw);
        let intent_u = quantize_millionths(intent_raw);
        let content_class_u = quantize_millionths(content_class_score);

        let score_numerator = scarcity_u * self.score_weights.scarcity
            + safety_u * self.score_weights.safety
            + expiry_u * self.score_weights.expiry
            + stagnation_u * self.score_weights.stagnation
            + proximity_u * self.score_weights.proximity
            + size_u * self.score_weights.size
            + intent_u * self.score_weights.intent
            + content_class_u * self.score_weights.content_class;

        let score_breakdown = EncounterScoreBreakdown {
            item_id: item.item_id.clone(),
            components: EncounterScoreComponents {
                scarcity: from_millionths(scarcity_u),
                safety: from_millionths(safety_u),
                expiry: from_millionths(expiry_u),
                stagnation: from_millionths(stagnation_u),
                proximity: from_millionths(proximity_u),
                size: from_millionths(size_u),
                intent: from_millionths(intent_u),
                content_class: from_millionths(content_class_u),
            },
            score_numerator,
            score: from_score_numerator(score_numerator),
        };

        ScoredItem {
            cargo_item: item.clone(),
            known_replica_count,
            last_forwarded_at_unix_ms,
            score_breakdown,
        }
    }

    fn ranks_before(&self, lhs: &ScoredItem, rhs: &ScoredItem) -> bool {
        if lhs.cargo_item.tier != rhs.cargo_item.tier {
            return lhs.cargo_item.tier < rhs.cargo_item.tier;
        }
        if lhs.score_breakdown.score_numerator != rhs.score_breakdown.score_numerator {
            return lhs.score_breakdown.score_numerator > rhs.score_breakdown.score_numerator;
        }
        if lhs.cargo_item.size_bytes != rhs.cargo_item.size_bytes {
            return lhs.cargo_item.size_bytes < rhs.cargo_item.size_bytes;
        }
        if lhs.cargo_item.expiry_at_unix_ms != rhs.cargo_item.expiry_at_unix_ms {
            return lhs.cargo_item.expiry_at_unix_ms < rhs.cargo_item.expiry_at_unix_ms;
        }
        if lhs.known_replica_count != rhs.known_replica_count {
            return lhs.known_replica_count < rhs.known_replica_count;
        }
        if lhs.last_forwarded_at_unix_ms != rhs.last_forwarded_at_unix_ms {
            return lhs.last_forwarded_at_unix_ms < rhs.last_forwarded_at_unix_ms;
        }
        if lhs.cargo_item.destination_rank != rhs.cargo_item.destination_rank {
            return lhs.cargo_item.destination_rank > rhs.cargo_item.destination_rank;
        }
        lhs.cargo_item.item_id > rhs.cargo_item.item_id
    }

    fn tie_break_reason_between(
        &self,
        previous: &ScoredItem,
        current: &ScoredItem,
    ) -> EncounterTieBreakReason {
        if previous.cargo_item.tier != current.cargo_item.tier
            || previous.score_breakdown.score_numerator != current.score_breakdown.score_numerator
        {
            return EncounterTieBreakReason::None;
        }
        if previous.cargo_item.size_bytes != current.cargo_item.size_bytes {
            return EncounterTieBreakReason::SizeBytes;
        }
        if previous.cargo_item.expiry_at_unix_ms != current.cargo_item.expiry_at_unix_ms {
            return EncounterTieBreakReason::ExpiryAtUnixMs;
        }
        if previous.known_replica_count != current.known_replica_count {
            return EncounterTieBreakReason::KnownReplicaCount;
        }
        if previous.last_forwarded_at_unix_ms != current.last_forwarded_at_unix_ms {
            return EncounterTieBreakReason::LastForwardedAtUnixMs;
        }
        if previous.cargo_item.destination_rank != current.cargo_item.destination_rank {
            return EncounterTieBreakReason::DestinationRank;
        }
        if previous.cargo_item.item_id != current.cargo_item.item_id {
            return EncounterTieBreakReason::ItemId;
        }
        EncounterTieBreakReason::None
    }
}

impl Default for EncounterSchedulerV1 {
    fn default() -> Self {
        Self::new()
    }
}

fn is_durable_cargo_tier(tier: i32) -> bool {
    tier == 4 || tier == 5
}

fn saturated_add(lhs: u64, rhs: u64) -> u64 {
    lhs.saturating_add(rhs)
}

fn int_difference(lhs: u64, rhs: u64) -> u64 {
    lhs.saturating_sub(rhs)
}

fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

fn quantize_millionths(value: f64) -> i32 {
    let clamped = clamp01(value);
    let quantized = (clamped * 1_000_000.0).round_ties_even() as i64;
    quantized.clamp(0, 1_000_000) as i32
}

fn from_millionths(millionths: i32) -> f64 {
    millionths as f64 / 1_000_000.0
}

fn from_score_numerator(score_numerator: i32) -> f64 {
    score_numerator as f64 / 100_000_000.0
}

fn is_valid_item_id(value: &str) -> bool {
    value.len() == 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[derive(Debug, Deserialize)]
    struct EncounterFixtureManifest {
        fixtures: Vec<FixtureReference>,
    }

    #[derive(Debug, Deserialize)]
    struct FixtureReference {
        fixture: String,
    }

    #[derive(Debug, Deserialize)]
    struct EncounterFixture {
        #[serde(rename = "encounterClass")]
        encounter_class: EncounterClass,
        #[serde(rename = "nowUnixMs")]
        now_unix_ms: u64,
        #[serde(rename = "budgetProfile")]
        budget_profile: FixtureBudgetProfile,
        #[serde(rename = "cargoItems")]
        cargo_items: Vec<FixtureCargoItem>,
        expected: FixtureExpected,
    }

    #[derive(Debug, Deserialize)]
    struct FixtureBudgetProfile {
        #[serde(rename = "maxItems")]
        max_items: i32,
        #[serde(rename = "maxBytes")]
        max_bytes: i32,
        #[serde(rename = "maxDurationMs")]
        max_duration_ms: Option<i32>,
        #[serde(rename = "durableCargoRatioCap")]
        durable_cargo_ratio_cap: Option<f64>,
        #[serde(rename = "preferredTransferUnitBytes")]
        preferred_transfer_unit_bytes: Option<i32>,
        #[serde(rename = "expiryUrgencyHorizonMs")]
        expiry_urgency_horizon_ms: Option<u64>,
        #[serde(rename = "stagnationHorizonMs")]
        stagnation_horizon_ms: Option<u64>,
        #[serde(rename = "targetReplicaCountDefault")]
        target_replica_count_default: Option<i32>,
    }

    impl FixtureBudgetProfile {
        fn scheduler_budget(&self) -> BudgetProfile {
            BudgetProfile {
                max_items: self.max_items,
                max_bytes: self.max_bytes,
                max_duration_ms: self.max_duration_ms,
                durable_cargo_ratio_cap: self.durable_cargo_ratio_cap,
                preferred_transfer_unit_bytes: self.preferred_transfer_unit_bytes.unwrap_or(32_768),
                expiry_urgency_horizon_ms: self.expiry_urgency_horizon_ms.unwrap_or(900_000),
                stagnation_horizon_ms: self.stagnation_horizon_ms.unwrap_or(3_600_000),
                target_replica_count_default: self.target_replica_count_default.unwrap_or(6),
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct FixtureCargoItem {
        #[serde(rename = "itemID")]
        item_id: String,
        tier: i32,
        #[serde(rename = "sizeBytes")]
        size_bytes: i32,
        #[serde(rename = "expiryAtUnixMs")]
        expiry_at_unix_ms: u64,
        #[serde(rename = "knownReplicaCount")]
        known_replica_count: Option<i32>,
        #[serde(rename = "targetReplicaCount")]
        target_replica_count: Option<i32>,
        #[serde(rename = "durablyStored")]
        durably_stored: Option<bool>,
        #[serde(rename = "relayIngested")]
        relay_ingested: Option<bool>,
        #[serde(rename = "receiptCoverage")]
        receipt_coverage: Option<f64>,
        #[serde(rename = "lastForwardedAtUnixMs")]
        last_forwarded_at_unix_ms: Option<u64>,
        #[serde(rename = "proximityClass")]
        proximity_class: Option<ProximityClass>,
        #[serde(rename = "explicitUserInitiated")]
        explicit_user_initiated: Option<bool>,
        #[serde(rename = "contentClassScore")]
        content_class_score: Option<f64>,
        #[serde(rename = "destinationRank")]
        destination_rank: i32,
        #[serde(rename = "estimatedDurationMs")]
        estimated_duration_ms: Option<i32>,
    }

    impl FixtureCargoItem {
        fn scheduler_item(&self) -> CargoItem {
            CargoItem {
                item_id: self.item_id.clone(),
                tier: self.tier,
                size_bytes: self.size_bytes,
                expiry_at_unix_ms: self.expiry_at_unix_ms,
                known_replica_count: self.known_replica_count,
                target_replica_count: self.target_replica_count,
                durably_stored: self.durably_stored,
                relay_ingested: self.relay_ingested,
                receipt_coverage: self.receipt_coverage,
                last_forwarded_at_unix_ms: self.last_forwarded_at_unix_ms,
                proximity_class: self.proximity_class,
                explicit_user_initiated: self.explicit_user_initiated,
                content_class_score: self.content_class_score,
                destination_rank: self.destination_rank,
                estimated_duration_ms: self.estimated_duration_ms,
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct FixtureExpected {
        #[serde(rename = "rankingOrder")]
        ranking_order: Vec<String>,
        #[serde(rename = "selectedPrefix")]
        selected_prefix: Vec<String>,
        #[serde(rename = "scoreBreakdowns")]
        score_breakdowns: Vec<FixtureScoreBreakdown>,
        #[serde(rename = "stopReason")]
        stop_reason: String,
        #[serde(rename = "tieBreakReason")]
        tie_break_reason: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct FixtureScoreBreakdown {
        #[serde(rename = "itemID")]
        item_id: String,
        components: FixtureScoreComponents,
        #[serde(rename = "scoreNumerator")]
        score_numerator: i32,
        score: f64,
    }

    #[derive(Debug, Deserialize)]
    struct FixtureScoreComponents {
        scarcity: f64,
        safety: f64,
        expiry: f64,
        stagnation: f64,
        proximity: f64,
        size: f64,
        intent: f64,
        #[serde(rename = "contentClass")]
        content_class: f64,
    }

    #[test]
    fn encounter_scheduler_matches_canonical_routing_fixtures() {
        let scheduler = EncounterSchedulerV1::new();
        let manifest = load_fixture_manifest();
        for fixture_path in manifest.fixtures.iter().map(|f| f.fixture.as_str()) {
            let fixture = load_fixture(fixture_path);
            let cargo_items = fixture
                .cargo_items
                .iter()
                .map(FixtureCargoItem::scheduler_item)
                .collect::<Vec<_>>();
            let result = scheduler
                .schedule(
                    fixture.encounter_class,
                    &fixture.budget_profile.scheduler_budget(),
                    fixture.now_unix_ms,
                    &cargo_items,
                )
                .expect("fixture schedule");

            assert_eq!(result.ranking_order(), fixture.expected.ranking_order);
            assert_eq!(
                result.selected_prefix_item_ids(),
                fixture.expected.selected_prefix
            );
            assert_eq!(result.stop_reason.as_str(), fixture.expected.stop_reason);
            assert_eq!(
                result.tie_break_reason.as_str(),
                fixture
                    .expected
                    .tie_break_reason
                    .as_deref()
                    .unwrap_or(EncounterTieBreakReason::None.as_str())
            );
            assert_eq!(
                result.score_breakdowns.len(),
                fixture.expected.score_breakdowns.len()
            );

            for (actual, expected) in result
                .score_breakdowns
                .iter()
                .zip(fixture.expected.score_breakdowns.iter())
            {
                assert_eq!(actual.item_id, expected.item_id);
                assert_eq!(actual.score_numerator, expected.score_numerator);
                assert_eq!(
                    millionths(actual.components.scarcity),
                    millionths(expected.components.scarcity)
                );
                assert_eq!(
                    millionths(actual.components.safety),
                    millionths(expected.components.safety)
                );
                assert_eq!(
                    millionths(actual.components.expiry),
                    millionths(expected.components.expiry)
                );
                assert_eq!(
                    millionths(actual.components.stagnation),
                    millionths(expected.components.stagnation)
                );
                assert_eq!(
                    millionths(actual.components.proximity),
                    millionths(expected.components.proximity)
                );
                assert_eq!(
                    millionths(actual.components.size),
                    millionths(expected.components.size)
                );
                assert_eq!(
                    millionths(actual.components.intent),
                    millionths(expected.components.intent)
                );
                assert_eq!(
                    millionths(actual.components.content_class),
                    millionths(expected.components.content_class)
                );
                assert_eq!(
                    score_denominator(actual.score),
                    score_denominator(expected.score)
                );
            }
        }
    }

    #[test]
    fn encounter_scheduler_score_math_matches_canonical_known_vector() {
        let scheduler = EncounterSchedulerV1::new();
        let now_unix_ms = 1_760_000_000_000u64;
        let item = CargoItem {
            item_id: "0000000000000000000000000000000000000000000000000000000000000101".to_string(),
            tier: 0,
            size_bytes: 256,
            expiry_at_unix_ms: 1_760_000_120_000,
            known_replica_count: Some(0),
            target_replica_count: None,
            durably_stored: None,
            relay_ingested: None,
            receipt_coverage: Some(0.1),
            last_forwarded_at_unix_ms: Some(1_759_999_400_000),
            proximity_class: Some(ProximityClass::DestinationPeer),
            explicit_user_initiated: Some(true),
            content_class_score: Some(0.3),
            destination_rank: 9,
            estimated_duration_ms: None,
        };
        let result = scheduler
            .schedule(
                EncounterClass::Blink,
                &BudgetProfile::new(1, 10_000_000),
                now_unix_ms,
                &[item],
            )
            .expect("schedule known vector");
        let breakdown = result.score_breakdowns.first().expect("breakdown present");

        assert_eq!(millionths(breakdown.components.scarcity), 1_000_000);
        assert_eq!(millionths(breakdown.components.safety), 980_000);
        assert_eq!(millionths(breakdown.components.expiry), 866_667);
        assert_eq!(millionths(breakdown.components.stagnation), 166_667);
        assert_eq!(millionths(breakdown.components.proximity), 1_000_000);
        assert_eq!(millionths(breakdown.components.size), 466_293);
        assert_eq!(millionths(breakdown.components.intent), 1_000_000);
        assert_eq!(millionths(breakdown.components.content_class), 300_000);
        assert_eq!(breakdown.score_numerator, 81_757_020);
        assert_eq!(score_denominator(breakdown.score), 81_757_020);
    }

    #[test]
    fn encounter_scheduler_is_stable_across_repeated_runs() {
        let scheduler = EncounterSchedulerV1::new();
        let fixture = load_fixture("./repeated-run-stability-mixed-ties.json");
        let budget = fixture.budget_profile.scheduler_budget();
        let cargo_items = fixture
            .cargo_items
            .iter()
            .map(FixtureCargoItem::scheduler_item)
            .collect::<Vec<_>>();
        let baseline = scheduler
            .schedule(
                fixture.encounter_class,
                &budget,
                fixture.now_unix_ms,
                &cargo_items,
            )
            .expect("baseline schedule");
        for _ in 0..64 {
            let rerun = scheduler
                .schedule(
                    fixture.encounter_class,
                    &budget,
                    fixture.now_unix_ms,
                    &cargo_items,
                )
                .expect("rerun schedule");
            assert_eq!(rerun, baseline);
        }
    }

    #[test]
    fn encounter_scheduler_handles_horizon_and_expiry_edge_cases() {
        let scheduler = EncounterSchedulerV1::new();
        let now_unix_ms = 1_760_000_000_000u64;
        let filtered_by_clock_skew = CargoItem {
            item_id: "0000000000000000000000000000000000000000000000000000000000000901".to_string(),
            tier: 1,
            size_bytes: 100,
            expiry_at_unix_ms: now_unix_ms + 30_000,
            known_replica_count: None,
            target_replica_count: None,
            durably_stored: None,
            relay_ingested: None,
            receipt_coverage: None,
            last_forwarded_at_unix_ms: None,
            proximity_class: None,
            explicit_user_initiated: None,
            content_class_score: None,
            destination_rank: 1,
            estimated_duration_ms: None,
        };
        let at_expiry_horizon = CargoItem {
            item_id: "0000000000000000000000000000000000000000000000000000000000000902".to_string(),
            tier: 1,
            size_bytes: 100,
            expiry_at_unix_ms: now_unix_ms + 900_000,
            known_replica_count: None,
            target_replica_count: None,
            durably_stored: None,
            relay_ingested: None,
            receipt_coverage: None,
            last_forwarded_at_unix_ms: Some(now_unix_ms - 3_600_000),
            proximity_class: None,
            explicit_user_initiated: None,
            content_class_score: None,
            destination_rank: 2,
            estimated_duration_ms: None,
        };
        let result = scheduler
            .schedule(
                EncounterClass::Short,
                &BudgetProfile::new(10, 100_000),
                now_unix_ms,
                &[filtered_by_clock_skew, at_expiry_horizon],
            )
            .expect("horizon schedule");
        assert_eq!(
            result.ranking_order(),
            vec!["0000000000000000000000000000000000000000000000000000000000000902".to_string()]
        );
        let scored = result
            .score_breakdowns
            .first()
            .expect("scored item present");
        assert_eq!(millionths(scored.components.expiry), 0);
        assert_eq!(millionths(scored.components.stagnation), 1_000_000);
    }

    #[test]
    fn encounter_scheduler_logarithmic_size_curve_is_monotonic_and_saturates() {
        let scheduler = EncounterSchedulerV1::new();
        let now_unix_ms = 1_760_000_000_000u64;
        let tiny = CargoItem {
            item_id: "0000000000000000000000000000000000000000000000000000000000000a01".to_string(),
            tier: 3,
            size_bytes: 1,
            expiry_at_unix_ms: now_unix_ms + 1_800_000,
            known_replica_count: None,
            target_replica_count: None,
            durably_stored: None,
            relay_ingested: None,
            receipt_coverage: None,
            last_forwarded_at_unix_ms: None,
            proximity_class: None,
            explicit_user_initiated: None,
            content_class_score: None,
            destination_rank: 1,
            estimated_duration_ms: None,
        };
        let medium = CargoItem {
            item_id: "0000000000000000000000000000000000000000000000000000000000000a02".to_string(),
            tier: 3,
            size_bytes: 1_024,
            expiry_at_unix_ms: now_unix_ms + 1_800_000,
            known_replica_count: None,
            target_replica_count: None,
            durably_stored: None,
            relay_ingested: None,
            receipt_coverage: None,
            last_forwarded_at_unix_ms: None,
            proximity_class: None,
            explicit_user_initiated: None,
            content_class_score: None,
            destination_rank: 1,
            estimated_duration_ms: None,
        };
        let preferred_unit = CargoItem {
            item_id: "0000000000000000000000000000000000000000000000000000000000000a03".to_string(),
            tier: 3,
            size_bytes: 32_768,
            expiry_at_unix_ms: now_unix_ms + 1_800_000,
            known_replica_count: None,
            target_replica_count: None,
            durably_stored: None,
            relay_ingested: None,
            receipt_coverage: None,
            last_forwarded_at_unix_ms: None,
            proximity_class: None,
            explicit_user_initiated: None,
            content_class_score: None,
            destination_rank: 1,
            estimated_duration_ms: None,
        };
        let beyond_preferred = CargoItem {
            item_id: "0000000000000000000000000000000000000000000000000000000000000a04".to_string(),
            tier: 3,
            size_bytes: 65_536,
            expiry_at_unix_ms: now_unix_ms + 1_800_000,
            known_replica_count: None,
            target_replica_count: None,
            durably_stored: None,
            relay_ingested: None,
            receipt_coverage: None,
            last_forwarded_at_unix_ms: None,
            proximity_class: None,
            explicit_user_initiated: None,
            content_class_score: None,
            destination_rank: 1,
            estimated_duration_ms: None,
        };

        let result = scheduler
            .schedule(
                EncounterClass::Durable,
                &BudgetProfile::new(10, 1_000_000),
                now_unix_ms,
                &[
                    tiny.clone(),
                    medium.clone(),
                    preferred_unit.clone(),
                    beyond_preferred.clone(),
                ],
            )
            .expect("size curve schedule");
        let mut by_id = BTreeMap::new();
        for breakdown in result.score_breakdowns {
            by_id.insert(breakdown.item_id, breakdown.components.size);
        }
        let tiny_size = *by_id.get(&tiny.item_id).expect("tiny size score");
        let medium_size = *by_id.get(&medium.item_id).expect("medium size score");
        let preferred_size = *by_id
            .get(&preferred_unit.item_id)
            .expect("preferred unit size score");
        let beyond_preferred_size = *by_id
            .get(&beyond_preferred.item_id)
            .expect("beyond preferred unit size score");

        assert!(tiny_size > medium_size);
        assert!(medium_size > preferred_size);
        assert_eq!(millionths(preferred_size), 0);
        assert_eq!(millionths(beyond_preferred_size), 0);
    }

    fn fixture_root() -> PathBuf {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
        let shared = workspace.join("third_party/aethos/Fixtures/Routing/encounter-ranking");
        if shared.exists() {
            return shared;
        }
        workspace.join("test-data/routing/encounter-ranking")
    }

    fn load_fixture_manifest() -> EncounterFixtureManifest {
        let root = fixture_root();
        let bytes = fs::read(root.join("manifest.json")).expect("read encounter fixture manifest");
        serde_json::from_slice::<EncounterFixtureManifest>(&bytes)
            .expect("parse encounter fixture manifest")
    }

    fn load_fixture(path: &str) -> EncounterFixture {
        let root = fixture_root();
        let normalized = path.strip_prefix("./").unwrap_or(path);
        let bytes = fs::read(root.join(normalized)).expect("read encounter fixture");
        serde_json::from_slice::<EncounterFixture>(&bytes).expect("parse encounter fixture")
    }

    fn millionths(value: f64) -> i32 {
        (value * 1_000_000.0).round_ties_even() as i32
    }

    fn score_denominator(value: f64) -> i32 {
        (value * 100_000_000.0).round_ties_even() as i32
    }
}
