//! Schema-evolution experiment.
//!
//! The question: when the ontology grows, how much of the existing query surface breaks?
//!
//! Both backends implement Q9 (role-agnostic traversal) against the base ontology. We then
//! extend the ontology with a new party kind (`trust`) and a 4-ary relation
//! (`control-via-nominee`), reload, and re-run the **unmodified** Q9 implementation.
//!
//! Two numbers come out of it:
//!
//! * **recall after extension** — what fraction of the entity's real relations the frozen
//!   query still returns. Anything below 1.0 means the backend silently under-reports
//!   after an ontology change, which in a governance system is a correctness failure
//!   rather than a performance one.
//! * **repair LOC** — how much query code has to be written to get back to 1.0.

use std::path::Path;

use benchmark_core::{
    ComplianceStore, Neighborhood, OntologyGeneration, QueryFamily, QueryProbe, Scale,
};
use benchmark_fixtures::generate_fixtures_with;
use serde::{Deserialize, Serialize};

use crate::benchmark::{POSTGRES_URL, TYPEDB_ADDRESS, TYPEDB_DATABASE};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationReport {
    pub generation: String,
    /// Relations the oracle says exist, summed over all Q9 probes.
    pub expected_edges: u64,
    /// Relations the frozen query actually returned.
    pub found_edges: u64,
    /// Relation types the oracle says exist.
    pub expected_relation_types: Vec<String>,
    /// Relation types the frozen query actually returned.
    pub found_relation_types: Vec<String>,
    /// Probes for which the frozen query returned exactly the oracle's answer.
    pub exact_probes: u64,
    pub total_probes: u64,
}

impl GenerationReport {
    pub fn recall(&self) -> f64 {
        if self.expected_edges == 0 {
            1.0
        } else {
            self.found_edges as f64 / self.expected_edges as f64
        }
    }

    pub fn missed_relation_types(&self) -> Vec<String> {
        self.expected_relation_types
            .iter()
            .filter(|t| !self.found_relation_types.contains(t))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendReport {
    pub backend: String,
    pub base: GenerationReport,
    pub extended_frozen: GenerationReport,
    pub extended_repaired: GenerationReport,
    pub repair_loc: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaEvolutionReport {
    pub scale: String,
    pub seed: u64,
    pub backends: Vec<BackendReport>,
}

pub async fn run_schema_evolution(
    backend: crate::BackendArg,
    scale: Scale,
    seed: u64,
    out: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(out)?;
    let mut backends = Vec::new();

    if matches!(
        backend,
        crate::BackendArg::Postgres | crate::BackendArg::Both
    ) {
        match run_postgres(scale, seed).await {
            Ok(r) => backends.push(r),
            Err(e) => tracing::error!("postgres schema-evolution failed: {e:#}"),
        }
    }

    if matches!(backend, crate::BackendArg::Typedb | crate::BackendArg::Both) {
        match run_typedb(scale, seed).await {
            Ok(r) => backends.push(r),
            Err(e) => tracing::error!("typedb schema-evolution failed: {e:#}"),
        }
    }

    let report = SchemaEvolutionReport {
        scale: format!("{scale:?}"),
        seed,
        backends,
    };

    let json_path = out.join("schema_evolution.json");
    std::fs::write(&json_path, serde_json::to_string_pretty(&report)?)?;
    tracing::info!("Wrote {}", json_path.display());

    let md_path = out.join("SCHEMA_EVOLUTION.md");
    std::fs::write(&md_path, render_markdown(&report))?;
    tracing::info!("Wrote {}", md_path.display());

    Ok(())
}

async fn run_postgres(scale: Scale, seed: u64) -> anyhow::Result<BackendReport> {
    use benchmark_postgres::PostgresStore;
    let mut store = PostgresStore::connect(POSTGRES_URL).await?;
    store.migrate().await?;
    measure(&mut store, "postgres", scale, seed).await
}

async fn run_typedb(scale: Scale, seed: u64) -> anyhow::Result<BackendReport> {
    use benchmark_typedb::TypeDbStore;
    let mut store = TypeDbStore::connect(TYPEDB_ADDRESS, TYPEDB_DATABASE).await?;
    store.setup_database().await?;
    measure(&mut store, "typedb", scale, seed).await
}

async fn measure<S: ComplianceStore>(
    store: &mut S,
    name: &str,
    scale: Scale,
    seed: u64,
) -> anyhow::Result<BackendReport> {
    tracing::info!("[{name}] loading base ontology");
    let base_bundle = generate_fixtures_with(seed, scale, OntologyGeneration::Base);
    load(store, &base_bundle).await?;
    let base = evaluate(store, &base_bundle, false).await?;

    tracing::info!("[{name}] loading extended ontology");
    let ext_bundle = generate_fixtures_with(seed, scale, OntologyGeneration::Extended);
    load(store, &ext_bundle).await?;
    let extended_frozen = evaluate(store, &ext_bundle, false).await?;
    let extended_repaired = evaluate(store, &ext_bundle, true).await?;

    Ok(BackendReport {
        backend: name.to_string(),
        base,
        extended_frozen,
        extended_repaired,
        repair_loc: store.query_repair_loc(),
    })
}

async fn load<S: ComplianceStore>(
    store: &mut S,
    bundle: &benchmark_core::FixtureBundle,
) -> anyhow::Result<()> {
    store.reset().await?;
    for event in &bundle.events {
        store.ingest(event.clone()).await?;
    }
    Ok(())
}

async fn evaluate<S: ComplianceStore>(
    store: &S,
    bundle: &benchmark_core::FixtureBundle,
    repaired: bool,
) -> anyhow::Result<GenerationReport> {
    let oracle = benchmark_core::oracle::Oracle::from_events(&bundle.events);
    let probes: Vec<&QueryProbe> = bundle
        .probes
        .iter()
        .filter(|p| p.family == QueryFamily::Q9RoleAgnosticTraversal)
        .collect();

    let mut expected_edges = 0u64;
    let mut found_edges = 0u64;
    let mut exact_probes = 0u64;
    let mut expected_types = Vec::new();
    let mut found_types = Vec::new();

    for probe in &probes {
        let expected: Neighborhood =
            oracle.neighborhood(probe.entity, probe.valid_at, probe.known_at);
        let actual = if repaired {
            store
                .neighborhood_repaired(probe.entity, probe.valid_at, probe.known_at)
                .await?
        } else {
            store
                .neighborhood(probe.entity, probe.valid_at, probe.known_at)
                .await?
        };

        let actual_set: std::collections::HashSet<_> = actual.edges.iter().collect();
        expected_edges += expected.edges.len() as u64;
        found_edges += expected
            .edges
            .iter()
            .filter(|e| actual_set.contains(e))
            .count() as u64;
        if expected.edges == actual.edges {
            exact_probes += 1;
        } else {
            let expected_set: std::collections::HashSet<_> = expected.edges.iter().collect();
            let extra: Vec<_> = actual
                .edges
                .iter()
                .filter(|e| !expected_set.contains(e))
                .collect();
            let missing: Vec<_> = expected
                .edges
                .iter()
                .filter(|e| !actual_set.contains(e))
                .collect();
            tracing::debug!(
                entity = %probe.entity,
                ?extra,
                ?missing,
                "Q9 answer differs from oracle"
            );
        }

        for t in expected.relation_types() {
            if !expected_types.contains(&t) {
                expected_types.push(t);
            }
        }
        for t in actual.relation_types() {
            if !found_types.contains(&t) {
                found_types.push(t);
            }
        }
    }

    expected_types.sort();
    found_types.sort();

    Ok(GenerationReport {
        generation: bundle.generation.name().to_string(),
        expected_edges,
        found_edges,
        expected_relation_types: expected_types,
        found_relation_types: found_types,
        exact_probes,
        total_probes: probes.len() as u64,
    })
}

fn render_markdown(report: &SchemaEvolutionReport) -> String {
    let mut s = String::new();
    s.push_str("# Schema evolution — query-surface churn\n\n");
    s.push_str(
        "Q9 (role-agnostic traversal) is implemented once against the base ontology and then \
         frozen. The ontology is extended with a `trust` party kind and a 4-ary \
         `control-via-nominee` relation, and the same unmodified query is re-run.\n\n\
         Both backends pay a schema cost for the extension. Only one pays a query cost.\n\n",
    );
    s.push_str(&format!(
        "Scale: {} · seed: {}\n\n",
        report.scale, report.seed
    ));

    s.push_str("| Backend | Recall (base) | Recall (extended, frozen query) | Recall (after repair) | Repair LOC |\n");
    s.push_str("|---------|---------------|----------------------------------|------------------------|------------|\n");
    for b in &report.backends {
        s.push_str(&format!(
            "| {} | {:.1}% | {:.1}% | {:.1}% | {} |\n",
            b.backend,
            b.base.recall() * 100.0,
            b.extended_frozen.recall() * 100.0,
            b.extended_repaired.recall() * 100.0,
            b.repair_loc,
        ));
    }

    s.push_str("\n## Relation types visible to the frozen query\n\n");
    for b in &report.backends {
        let missed = b.extended_frozen.missed_relation_types();
        s.push_str(&format!(
            "**{}** — expected `{}`, saw `{}`.\n",
            b.backend,
            b.extended_frozen.expected_relation_types.join("`, `"),
            b.extended_frozen.found_relation_types.join("`, `"),
        ));
        if missed.is_empty() {
            s.push_str("No relation type was lost; the query needed no edit.\n\n");
        } else {
            s.push_str(&format!(
                "Lost `{}` silently: the query returned a smaller answer with no error.\n\n",
                missed.join("`, `")
            ));
        }
    }

    s.push_str("## Exact-answer probes\n\n");
    s.push_str("| Backend | Base | Extended (frozen) | Extended (repaired) |\n");
    s.push_str("|---------|------|-------------------|---------------------|\n");
    for b in &report.backends {
        s.push_str(&format!(
            "| {} | {}/{} | {}/{} | {}/{} |\n",
            b.backend,
            b.base.exact_probes,
            b.base.total_probes,
            b.extended_frozen.exact_probes,
            b.extended_frozen.total_probes,
            b.extended_repaired.exact_probes,
            b.extended_repaired.total_probes,
        ));
    }

    s.push_str("\n---\n*Generated by `runner schema-evolution`.*\n");
    s
}
