use serde::{Deserialize, Serialize};

use crate::types::QueryFamily;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryMetrics {
    pub family: QueryFamily,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub throughput_qps: f64,
    pub round_trips: u64,
    pub iterations: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChurnMetrics {
    pub physical_mutations: u64,
    pub semantic_changes: u64,
    pub ratio: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorrectnessMetrics {
    pub false_allow: u64,
    pub false_block: u64,
    pub false_review: u64,
    pub false_contradiction: u64,
    pub missed_contradiction: u64,
    pub false_merge: u64,
    pub false_split: u64,
    pub incorrect_historical_replay: u64,
    /// Q9: relations the entity genuinely participates in that the query failed to return.
    pub missed_relations: u64,
    pub total_probes: u64,
    pub passed: u64,
}

impl CorrectnessMetrics {
    pub fn pass_rate(&self) -> f64 {
        if self.total_probes == 0 {
            1.0
        } else {
            self.passed as f64 / self.total_probes as f64
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComplexityMetrics {
    pub schema_loc: u64,
    pub query_loc: u64,
    pub rust_backend_loc: u64,
    pub db_objects: u64,
    pub indexes: u64,
    pub triggers_functions: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BenchmarkMetrics {
    pub backend: String,
    pub scale: String,
    pub ablation: String,
    pub seed: u64,
    pub cold: bool,
    pub ingest_ms: u64,
    pub queries: Vec<QueryMetrics>,
    pub churn: ChurnMetrics,
    pub correctness: CorrectnessMetrics,
    pub complexity: ComplexityMetrics,
    pub memory_bytes: u64,
}
