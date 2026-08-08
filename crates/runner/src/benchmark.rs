use std::path::Path;
use std::time::Instant;

use benchmark_core::{
    oracle::Oracle, AblationDimension, BenchmarkMetrics, ChurnMetrics, ComplianceStore,
    CorrectnessMetrics, ExpectedAnswer, FixtureBundle, QueryFamily, Scale, StateDelta,
};
use benchmark_fixtures::generate_fixtures as build_fixtures;
use benchmark_postgres::PostgresStore;
use benchmark_typedb::TypeDbStore;
use hdrhistogram::Histogram;

use crate::complexity::measure_complexity;
use crate::correctness::{compare_answers, run_correctness_checks};
use crate::export::{write_decision_md, write_raw_json, write_summary_csv};
use crate::signal::{detect_signal, SignalReport};

pub const POSTGRES_URL: &str = "postgres://benchmark:benchmark@localhost:5432/benchmark";
pub const TYPEDB_ADDRESS: &str = "localhost:1729";
pub const TYPEDB_DATABASE: &str = "benchmark";

pub fn write_fixtures_to_disk(scale: Scale, seed: u64, out: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(out)?;
    let bundle = build_fixtures(seed, scale);
    let events_path = out.join("events.jsonl");
    let expected_path = out.join("expected.json");

    let mut events_file = std::fs::File::create(&events_path)?;
    for event in &bundle.events {
        use std::io::Write;
        writeln!(events_file, "{}", serde_json::to_string(event)?)?;
    }
    std::fs::write(&expected_path, serde_json::to_string_pretty(&bundle)?)?;
    tracing::info!(
        "Generated {} events, {} probes -> {}",
        bundle.events.len(),
        bundle.probes.len(),
        out.display()
    );
    Ok(())
}

pub async fn run_benchmark(
    backend: crate::BackendArg,
    scale: Scale,
    seed: u64,
    ablation: AblationDimension,
    out: &Path,
    cold: bool,
    skip_l: bool,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(out.join("raw"))?;
    let bundle = build_fixtures(seed, scale);
    let mut all_metrics = Vec::new();

    match backend {
        crate::BackendArg::Postgres | crate::BackendArg::Both => {
            match run_postgres(&bundle, scale, seed, ablation, cold).await {
                Ok(metrics) => {
                    write_raw_json(out, &metrics)?;
                    all_metrics.push(metrics);
                }
                Err(e) => tracing::error!("postgres benchmark failed: {e:#}"),
            }
        }
        _ => {}
    }

    match backend {
        crate::BackendArg::Typedb | crate::BackendArg::Both => {
            match run_typedb(&bundle, scale, seed, ablation, cold).await {
                Ok(metrics) => {
                    write_raw_json(out, &metrics)?;
                    all_metrics.push(metrics);
                }
                Err(e) => tracing::error!("typedb benchmark failed: {e:#}"),
            }
        }
        _ => {}
    }

    write_summary_csv(out, &all_metrics)?;

    // Conditional scale L
    if !skip_l && scale == Scale::M {
        let signal = detect_signal(&all_metrics);
        if signal.should_run_l {
            tracing::info!("Signal detected, running scale L");
            let bundle_l = build_fixtures(seed, Scale::L);
            if matches!(backend, crate::BackendArg::Postgres | crate::BackendArg::Both) {
                if let Ok(m) = run_postgres(&bundle_l, Scale::L, seed, ablation, cold).await {
                    write_raw_json(out, &m)?;
                    all_metrics.push(m);
                }
            }
            if matches!(backend, crate::BackendArg::Typedb | crate::BackendArg::Both) {
                if let Ok(m) = run_typedb(&bundle_l, Scale::L, seed, ablation, cold).await {
                    write_raw_json(out, &m)?;
                    all_metrics.push(m);
                }
            }
            write_summary_csv(out, &all_metrics)?;
        }
    }

    let schema_evolution = crate::signal::load_schema_evolution_signal(out);
    let signal = crate::signal::detect_signal_with(&all_metrics, schema_evolution.as_ref());
    write_decision_md(out, &all_metrics, &signal)?;

    Ok(())
}

pub async fn run_ablation(
    backend: crate::BackendArg,
    scale: Scale,
    seed: u64,
    out: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(out.join("raw"))?;
    let mut all_metrics = Vec::new();

    // Baseline
    run_benchmark(backend.clone(), scale, seed, AblationDimension::None, out, false, true).await?;

    for dim in AblationDimension::all_ablatable() {
        tracing::info!("Ablation: {}", dim.name());
        match backend {
            crate::BackendArg::Postgres | crate::BackendArg::Both => {
                let bundle = build_fixtures(seed, scale);
                if let Ok(m) = run_postgres(&bundle, scale, seed, *dim, false).await {
                    all_metrics.push(m);
                }
            }
            _ => {}
        }
        match backend {
            crate::BackendArg::Typedb | crate::BackendArg::Both => {
                let bundle = build_fixtures(seed, scale);
                if let Ok(m) = run_typedb(&bundle, scale, seed, *dim, false).await {
                    all_metrics.push(m);
                }
            }
            _ => {}
        }
    }

    write_summary_csv(out, &all_metrics)?;
    Ok(())
}

pub async fn run_postgres(
    bundle: &FixtureBundle,
    scale: Scale,
    seed: u64,
    ablation: AblationDimension,
    cold: bool,
) -> anyhow::Result<BenchmarkMetrics> {
    let store = PostgresStore::connect_with_ablation(POSTGRES_URL, ablation).await?;
    store.migrate().await?;
    run_store_benchmark(store, bundle, "postgres", scale, seed, ablation, cold).await
}

pub async fn run_typedb(
    bundle: &FixtureBundle,
    scale: Scale,
    seed: u64,
    ablation: AblationDimension,
    cold: bool,
) -> anyhow::Result<BenchmarkMetrics> {
    let mut store = TypeDbStore::connect_with_ablation(TYPEDB_ADDRESS, TYPEDB_DATABASE, ablation).await?;
    store.setup_database().await?;
    run_store_benchmark(store, bundle, "typedb", scale, seed, ablation, cold).await
}

async fn run_store_benchmark<S: ComplianceStore>(
    mut store: S,
    bundle: &FixtureBundle,
    backend_name: &str,
    scale: Scale,
    seed: u64,
    ablation: AblationDimension,
    cold: bool,
) -> anyhow::Result<BenchmarkMetrics> {
    if cold {
        store.reset().await?;
    }

    // Ingest
    let ingest_start = Instant::now();
    let mut total_churn = StateDelta::default();
    for event in &bundle.events {
        let delta = store.ingest(event.clone()).await?;
        total_churn.physical_mutations += delta.physical_mutations;
        total_churn.semantic_changes += delta.semantic_changes;
    }
    let ingest_ms = ingest_start.elapsed().as_millis() as u64;

    // Query benchmarks
    let mut query_metrics = Vec::new();
    for family in QueryFamily::all() {
        let probes: Vec<_> = bundle
            .probes
            .iter()
            .filter(|p| p.family == *family)
            .collect();
        if probes.is_empty() {
            continue;
        }

        let mut hist = Histogram::<u64>::new(3).unwrap();
        let iterations = probes.len().max(1) as u64;
        let query_start = Instant::now();

        for probe in &probes {
            let t0 = Instant::now();
            execute_probe(&store, probe).await?;
            hist.record(t0.elapsed().as_micros() as u64).ok();
        }

        let elapsed = query_start.elapsed();
        query_metrics.push(benchmark_core::QueryMetrics {
            family: *family,
            p50_us: hist.value_at_quantile(0.5),
            p95_us: hist.value_at_quantile(0.95),
            p99_us: hist.value_at_quantile(0.99),
            throughput_qps: iterations as f64 / elapsed.as_secs_f64().max(0.001),
            round_trips: iterations,
            iterations,
        });
    }

    // Correctness
    let oracle = Oracle::from_events(&bundle.events);
    let correctness = run_correctness_checks(&store, &oracle, &bundle.probes, &bundle.expected).await;

    let churn_ratio = if total_churn.semantic_changes > 0 {
        total_churn.physical_mutations as f64 / total_churn.semantic_changes as f64
    } else {
        0.0
    };

    let complexity = measure_complexity(backend_name);

    Ok(BenchmarkMetrics {
        backend: backend_name.to_string(),
        scale: format!("{scale:?}"),
        ablation: ablation.name().to_string(),
        seed,
        cold,
        ingest_ms,
        queries: query_metrics,
        churn: ChurnMetrics {
            physical_mutations: total_churn.physical_mutations,
            semantic_changes: total_churn.semantic_changes,
            ratio: churn_ratio,
        },
        correctness,
        complexity,
        memory_bytes: 0,
    })
}

async fn execute_probe<S: ComplianceStore>(
    store: &S,
    probe: &benchmark_core::QueryProbe,
) -> anyhow::Result<()> {
    use benchmark_core::QueryFamily;
    match probe.family {
        QueryFamily::Q1BeneficialOwner => {
            store
                .beneficial_owners(probe.entity, probe.valid_at, probe.known_at)
                .await?;
        }
        QueryFamily::Q2BitemporalLookup => {
            store
                .state_at(probe.entity, probe.valid_at, probe.known_at)
                .await?;
        }
        QueryFamily::Q3Contradictions => {
            store
                .contradictions(probe.entity, probe.valid_at, probe.known_at)
                .await?;
        }
        QueryFamily::Q4IdentityDiscrimination => {
            if let (Some(a), Some(b)) = (probe.person_a, probe.person_b) {
                store
                    .identity_action(a, b, probe.valid_at, probe.known_at)
                    .await?;
            }
        }
        QueryFamily::Q5OwnershipExposure => {
            store
                .ownership_exposure(probe.entity, probe.valid_at, probe.known_at)
                .await?;
        }
        QueryFamily::Q6ContextCompatibility => {
            store
                .context_compatibility(probe.entity, probe.valid_at, probe.known_at)
                .await?;
        }
        QueryFamily::Q7HistoricalReplay | QueryFamily::Q8RetrospectiveView => {
            store
                .compliance_decision(probe.entity, probe.valid_at, probe.known_at)
                .await?;
        }
        QueryFamily::Q9RoleAgnosticTraversal => {
            store
                .neighborhood(probe.entity, probe.valid_at, probe.known_at)
                .await?;
        }
    }
    Ok(())
}
