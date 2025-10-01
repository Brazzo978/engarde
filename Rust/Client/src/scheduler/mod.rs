use std::{cmp::Ordering, collections::HashMap, fmt, hash::Hash, time::Duration};

use serde::Deserialize;
use smallvec::SmallVec;

/// Identifier for a transmission path/link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathId(pub u32);

impl PathId {
    pub const fn new(id: u32) -> Self {
        PathId(id)
    }
}

/// Metadata attached to each packet. Currently empty but ready for future FEC work.
#[derive(Debug, Clone, Default)]
pub struct PacketMeta {
    pub fec: Option<FecMeta>,
}

/// Placeholder structure for forward error correction metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FecMeta {
    pub data_shards: usize,
    pub parity_shards: usize,
}

impl Default for FecMeta {
    fn default() -> Self {
        FecMeta {
            data_shards: 0,
            parity_shards: 0,
        }
    }
}

/// Represents the scheduling state for a single link/path.
#[derive(Debug, Clone)]
pub struct LinkState {
    pub id: PathId,
    pub up: bool,
    pub weight: f64,
    pub smoothed_rtt: Duration,
    pub loss: f64,
    pub send_bps: f64,
    pub inflight_bytes: f64,
    pub tokens: usize,
}

impl LinkState {
    pub fn new(id: PathId) -> Self {
        Self {
            id,
            up: true,
            weight: 1.0,
            smoothed_rtt: Duration::from_millis(0),
            loss: 0.0,
            send_bps: 0.0,
            inflight_bytes: 0.0,
            tokens: usize::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulerMetrics {
    pub replica2_primary: u64,
    pub replica2_secondary: u64,
    pub replica2_fallbacks: u64,
    pub no_token_skips: u64,
}

impl SchedulerMetrics {
    pub fn accumulate(&mut self, other: SchedulerMetrics) {
        self.replica2_primary += other.replica2_primary;
        self.replica2_secondary += other.replica2_secondary;
        self.replica2_fallbacks += other.replica2_fallbacks;
        self.no_token_skips += other.no_token_skips;
    }
}

pub trait Scheduler: Send {
    fn select_paths(
        &mut self,
        pkt_len: usize,
        meta: &PacketMeta,
        links: &mut [LinkState],
    ) -> SmallVec<[PathId; 4]>;

    fn metrics(&self) -> SchedulerMetrics {
        SchedulerMetrics::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerAlgorithm {
    Mirror,
    WeightedRoundRobin,
    Replica2Weighted,
    FecKn,
}

impl SchedulerAlgorithm {
    fn from_number(value: u64) -> Self {
        match value {
            1 => SchedulerAlgorithm::Mirror,
            2 => SchedulerAlgorithm::WeightedRoundRobin,
            3 => SchedulerAlgorithm::Mirror,
            _ => SchedulerAlgorithm::Mirror,
        }
    }
}

impl<'de> Deserialize<'de> for SchedulerAlgorithm {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AlgorithmVisitor;

        impl<'de> serde::de::Visitor<'de> for AlgorithmVisitor {
            type Value = SchedulerAlgorithm;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a scheduler algorithm identifier")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(SchedulerAlgorithm::from_number(value))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match v {
                    "mirror" | "mirror_all" | "mirrorall" => Ok(SchedulerAlgorithm::Mirror),
                    "weighted_round_robin" | "wrr" => Ok(SchedulerAlgorithm::WeightedRoundRobin),
                    "replica2_weighted" | "replica2" => Ok(SchedulerAlgorithm::Replica2Weighted),
                    "fec_kn" | "fec" => Ok(SchedulerAlgorithm::FecKn),
                    other => Err(E::custom(format!("unknown scheduler algorithm '{other}'"))),
                }
            }
        }

        deserializer.deserialize_any(AlgorithmVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AggregationConfig {
    #[serde(rename = "minLinksForAggregation")]
    pub min_links_for_aggregation: usize,
    #[serde(rename = "aggregationAlgorithm")]
    pub algorithm: SchedulerAlgorithm,
    pub replica2: Replica2WeightedConfig,
}

impl Default for AggregationConfig {
    fn default() -> Self {
        Self {
            min_links_for_aggregation: 1,
            algorithm: SchedulerAlgorithm::Mirror,
            replica2: Replica2WeightedConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Replica2WeightedConfig {
    #[serde(rename = "useWeights")]
    pub use_weights: bool,
    #[serde(rename = "lossPenalty")]
    pub loss_penalty: f64,
    #[serde(rename = "queuePenaltyScale")]
    pub queue_penalty_scale: f64,
    #[serde(rename = "rttAlpha")]
    pub rtt_alpha: f64,
}

impl Default for Replica2WeightedConfig {
    fn default() -> Self {
        Self {
            use_weights: true,
            loss_penalty: 5.0,
            queue_penalty_scale: 1.0,
            rtt_alpha: 1.0,
        }
    }
}

#[derive(Debug)]
pub enum SchedulerError {
    Unsupported(&'static str),
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchedulerError::Unsupported(name) => write!(f, "scheduler '{name}' is not implemented"),
        }
    }
}

impl std::error::Error for SchedulerError {}

pub struct SchedulerFactory;

impl SchedulerFactory {
    pub fn build(
        algorithm: SchedulerAlgorithm,
        config: &AggregationConfig,
    ) -> Result<Box<dyn Scheduler>, SchedulerError> {
        match algorithm {
            SchedulerAlgorithm::Mirror => Ok(Box::new(MirrorScheduler::default())),
            SchedulerAlgorithm::WeightedRoundRobin => {
                Ok(Box::new(WeightedRoundRobinScheduler::default()))
            }
            SchedulerAlgorithm::Replica2Weighted => {
                let fallback: Box<dyn Scheduler> = Box::new(WeightedRoundRobinScheduler::default());
                Ok(Box::new(Replica2WeightedScheduler::new(
                    config.min_links_for_aggregation,
                    config.replica2.clone(),
                    fallback,
                )))
            }
            SchedulerAlgorithm::FecKn => Err(SchedulerError::Unsupported("fec_kn")),
        }
    }
}

#[derive(Default)]
struct MirrorScheduler;

impl Scheduler for MirrorScheduler {
    fn select_paths(
        &mut self,
        pkt_len: usize,
        _meta: &PacketMeta,
        links: &mut [LinkState],
    ) -> SmallVec<[PathId; 4]> {
        let mut selected = SmallVec::<[PathId; 4]>::new();
        for link in links.iter_mut().filter(|l| l.up && l.tokens >= pkt_len) {
            selected.push(link.id);
            link.tokens = link.tokens.saturating_sub(pkt_len);
        }
        selected
    }
}

#[derive(Debug, Clone)]
struct WeightedEntry {
    id: PathId,
    weight: f64,
    current_weight: f64,
}

impl WeightedEntry {
    fn new(id: PathId, weight: f64) -> Self {
        WeightedEntry {
            id,
            weight,
            current_weight: 0.0,
        }
    }
}

#[derive(Default)]
struct WeightedRoundRobinScheduler {
    entries: Vec<WeightedEntry>,
    cache: HashMap<PathId, usize>,
}

impl WeightedRoundRobinScheduler {
    fn rebuild(&mut self, links: &[LinkState]) {
        let mut existing: HashMap<PathId, WeightedEntry> = self
            .entries
            .drain(..)
            .map(|entry| (entry.id, entry))
            .collect();
        self.cache.clear();
        for (position, link) in links.iter().filter(|l| l.up).enumerate() {
            let weight = if link.weight.is_finite() {
                link.weight.max(0.0)
            } else {
                0.0
            };
            let mut entry = existing
                .remove(&link.id)
                .unwrap_or_else(|| WeightedEntry::new(link.id, weight));
            if (entry.weight - weight).abs() > f64::EPSILON {
                entry.weight = weight;
                entry.current_weight = 0.0;
            }
            self.cache.insert(link.id, position);
            self.entries.push(entry);
        }
    }

    fn find_link_mut<'a>(links: &'a mut [LinkState], id: PathId) -> Option<&'a mut LinkState> {
        links.iter_mut().find(|link| link.id == id)
    }

    fn total_weight(&self, links: &[LinkState], pkt_len: usize) -> f64 {
        self.entries
            .iter()
            .filter_map(|entry| {
                let link = links.iter().find(|l| l.id == entry.id)?;
                if !link.up || link.tokens < pkt_len || entry.weight <= 0.0 {
                    None
                } else {
                    Some(entry.weight)
                }
            })
            .sum()
    }
}

impl Scheduler for WeightedRoundRobinScheduler {
    fn select_paths(
        &mut self,
        pkt_len: usize,
        _meta: &PacketMeta,
        links: &mut [LinkState],
    ) -> SmallVec<[PathId; 4]> {
        self.rebuild(links);

        let mut best_idx: Option<usize> = None;
        let mut best_weight = f64::NEG_INFINITY;
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            let Some(link) = links.iter().find(|l| l.id == entry.id) else {
                continue;
            };
            if !link.up || link.tokens < pkt_len || entry.weight <= 0.0 {
                continue;
            }
            entry.current_weight += entry.weight;
            if entry.current_weight > best_weight {
                best_weight = entry.current_weight;
                best_idx = Some(idx);
            }
        }

        if let Some(idx) = best_idx {
            let total = self.total_weight(links, pkt_len);
            if total <= 0.0 {
                return SmallVec::new();
            }

            let chosen_id = self.entries[idx].id;
            self.entries[idx].current_weight -= total;
            if let Some(link) = Self::find_link_mut(links, chosen_id) {
                link.tokens = link.tokens.saturating_sub(pkt_len);
            }
            let mut result = SmallVec::<[PathId; 4]>::new();
            result.push(chosen_id);
            result
        } else {
            SmallVec::new()
        }
    }
}

struct Replica2WeightedScheduler {
    min_links_for_aggregation: usize,
    config: Replica2WeightedConfig,
    fallback: Box<dyn Scheduler>,
    metrics: SchedulerMetrics,
}

impl Replica2WeightedScheduler {
    fn new(
        min_links_for_aggregation: usize,
        config: Replica2WeightedConfig,
        fallback: Box<dyn Scheduler>,
    ) -> Self {
        Self {
            min_links_for_aggregation,
            config,
            fallback,
            metrics: SchedulerMetrics::default(),
        }
    }

    fn compute_eta(&self, link: &LinkState) -> f64 {
        let rtt_component = self.config.rtt_alpha * link.smoothed_rtt.as_secs_f64();
        let send_bps = if link.send_bps > 0.0 {
            link.send_bps
        } else {
            1.0
        };
        let queue_component = self.config.queue_penalty_scale * (link.inflight_bytes / send_bps);
        let loss_component = link.loss * self.config.loss_penalty;
        let mut eta = rtt_component + queue_component + loss_component;
        if self.config.use_weights {
            let weight = link.weight.max(0.1);
            eta /= weight;
        }
        eta
    }

    fn eligible_links<'a>(
        &'a self,
        pkt_len: usize,
        links: &'a [LinkState],
    ) -> (Vec<Candidate>, u64) {
        let mut candidates = Vec::new();
        let mut token_skips = 0;
        for (idx, link) in links.iter().enumerate() {
            if !link.up {
                continue;
            }
            if link.tokens < pkt_len {
                token_skips += 1;
                continue;
            }
            let eta = self.compute_eta(link);
            candidates.push(Candidate {
                index: idx,
                id: link.id,
                eta,
            });
        }
        (candidates, token_skips)
    }

    fn select_via_fallback(
        &mut self,
        pkt_len: usize,
        meta: &PacketMeta,
        links: &mut [LinkState],
    ) -> SmallVec<[PathId; 4]> {
        self.metrics.replica2_fallbacks += 1;
        let result = self.fallback.select_paths(pkt_len, meta, links);
        let fallback_metrics = self.fallback.metrics();
        self.metrics.accumulate(fallback_metrics);
        result
    }
}

impl Scheduler for Replica2WeightedScheduler {
    fn select_paths(
        &mut self,
        pkt_len: usize,
        meta: &PacketMeta,
        links: &mut [LinkState],
    ) -> SmallVec<[PathId; 4]> {
        let min_links = self.min_links_for_aggregation.max(3);
        let links_up = links.iter().filter(|link| link.up).count();
        if links_up < min_links {
            return self.select_via_fallback(pkt_len, meta, links);
        }

        let (mut candidates, token_skips) = self.eligible_links(pkt_len, links);
        self.metrics.no_token_skips += token_skips;

        if candidates.is_empty() {
            return self.select_via_fallback(pkt_len, meta, links);
        }

        if candidates.len() == 1 {
            let candidate = candidates.remove(0);
            if let Some(link) = links.get_mut(candidate.index) {
                link.tokens = link.tokens.saturating_sub(pkt_len);
            }
            self.metrics.replica2_primary += 1;
            let mut result = SmallVec::<[PathId; 4]>::new();
            result.push(candidate.id);
            return result;
        }

        candidates.sort_by(|a, b| match a.eta.partial_cmp(&b.eta) {
            Some(Ordering::Equal) => a.id.cmp(&b.id),
            Some(order) => order,
            None => Ordering::Equal,
        });

        let mut result = SmallVec::<[PathId; 4]>::new();

        for candidate in candidates.iter().take(2) {
            if let Some(link) = links.get_mut(candidate.index) {
                link.tokens = link.tokens.saturating_sub(pkt_len);
            }
            result.push(candidate.id);
        }

        if !result.is_empty() {
            self.metrics.replica2_primary += 1;
        }
        if result.len() > 1 {
            self.metrics.replica2_secondary += 1;
        }

        result
    }

    fn metrics(&self) -> SchedulerMetrics {
        self.metrics
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    index: usize,
    id: PathId,
    eta: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(
        id: u32,
        up: bool,
        weight: f64,
        rtt_ms: u64,
        loss: f64,
        send_bps: f64,
        inflight: f64,
        tokens: usize,
    ) -> LinkState {
        LinkState {
            id: PathId::new(id),
            up,
            weight,
            smoothed_rtt: Duration::from_millis(rtt_ms),
            loss,
            send_bps,
            inflight_bytes: inflight,
            tokens,
        }
    }

    fn default_config() -> AggregationConfig {
        AggregationConfig {
            min_links_for_aggregation: 3,
            algorithm: SchedulerAlgorithm::Replica2Weighted,
            replica2: Replica2WeightedConfig::default(),
        }
    }

    #[test]
    fn replica2_selects_two_distinct_links_when_enough_candidates() {
        let mut scheduler =
            SchedulerFactory::build(SchedulerAlgorithm::Replica2Weighted, &default_config())
                .unwrap();

        let mut links = vec![
            link(1, true, 1.0, 10, 0.0, 1_000_000.0, 1000.0, 2000),
            link(2, true, 1.0, 12, 0.0, 1_000_000.0, 1100.0, 2000),
            link(3, true, 1.0, 30, 0.0, 1_000_000.0, 2000.0, 2000),
        ];
        let paths = scheduler.select_paths(1200, &PacketMeta::default(), &mut links);
        assert_eq!(paths.len(), 2);
        assert_ne!(paths[0], paths[1]);
    }

    #[test]
    fn replica2_falls_back_when_links_below_threshold() {
        let mut scheduler =
            SchedulerFactory::build(SchedulerAlgorithm::Replica2Weighted, &default_config())
                .unwrap();

        let mut links = vec![
            link(1, true, 1.0, 10, 0.0, 1_000_000.0, 500.0, 2000),
            link(2, true, 2.0, 15, 0.0, 1_000_000.0, 800.0, 2000),
        ];
        let paths = scheduler.select_paths(1200, &PacketMeta::default(), &mut links);
        assert_eq!(paths.len(), 1);
        let metrics = scheduler.metrics();
        assert_eq!(metrics.replica2_fallbacks, 1);
    }

    #[test]
    fn replica2_skips_links_without_tokens() {
        let mut scheduler =
            SchedulerFactory::build(SchedulerAlgorithm::Replica2Weighted, &default_config())
                .unwrap();

        let mut links = vec![
            link(1, true, 1.0, 10, 0.0, 1_000_000.0, 1000.0, 2000),
            link(2, true, 1.0, 11, 0.0, 1_000_000.0, 1000.0, 500),
            link(3, true, 1.0, 12, 0.0, 1_000_000.0, 1000.0, 2000),
        ];
        let paths = scheduler.select_paths(1200, &PacketMeta::default(), &mut links);
        assert_eq!(paths.len(), 2);
        assert!(!paths.iter().any(|id| id.0 == 2));
        let metrics = scheduler.metrics();
        assert_eq!(metrics.no_token_skips, 1);
    }

    #[test]
    fn higher_weights_are_preferred_with_equal_conditions() {
        let mut scheduler =
            SchedulerFactory::build(SchedulerAlgorithm::Replica2Weighted, &default_config())
                .unwrap();

        let mut primary_counts: HashMap<PathId, usize> = HashMap::new();
        let mut links = vec![
            link(1, true, 3.0, 10, 0.0, 1_000_000.0, 1000.0, 5000),
            link(2, true, 1.0, 10, 0.0, 1_000_000.0, 1000.0, 5000),
            link(3, true, 1.0, 10, 0.0, 1_000_000.0, 1000.0, 5000),
        ];

        for _ in 0..10 {
            for link in &mut links {
                link.tokens = 5000;
            }
            let paths = scheduler.select_paths(1200, &PacketMeta::default(), &mut links);
            if let Some(primary) = paths.get(0) {
                *primary_counts.entry(*primary).or_insert(0) += 1;
            }
        }

        let high_weight_primary = primary_counts
            .get(&PathId::new(1))
            .copied()
            .unwrap_or_default();
        let low_weight_primary_a = primary_counts
            .get(&PathId::new(2))
            .copied()
            .unwrap_or_default();
        let low_weight_primary_b = primary_counts
            .get(&PathId::new(3))
            .copied()
            .unwrap_or_default();
        assert!(high_weight_primary > low_weight_primary_a);
        assert!(high_weight_primary > low_weight_primary_b);
    }

    #[test]
    fn parse_aggregation_config_from_yaml() {
        let yaml = r#"
minLinksForAggregation: 3
aggregationAlgorithm: "replica2_weighted"
replica2:
  useWeights: true
  lossPenalty: 5.0
  queuePenaltyScale: 1.0
  rttAlpha: 1.0
"#;
        let config: AggregationConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.min_links_for_aggregation, 3);
        assert_eq!(config.algorithm, SchedulerAlgorithm::Replica2Weighted);
        assert!(config.replica2.use_weights);
        assert_eq!(config.replica2.loss_penalty, 5.0);
        assert_eq!(config.replica2.queue_penalty_scale, 1.0);
        assert_eq!(config.replica2.rtt_alpha, 1.0);
    }

    #[test]
    fn factory_returns_error_for_fec_kn() {
        let config = AggregationConfig::default();
        let err = SchedulerFactory::build(SchedulerAlgorithm::FecKn, &config)
            .err()
            .expect("expected fec_kn to be unsupported");
        match err {
            SchedulerError::Unsupported(name) => assert_eq!(name, "fec_kn"),
        }
    }
}
