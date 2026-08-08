use std::path::Path;

use benchmark_core::BenchmarkMetrics;

pub fn write_raw_json(out: &Path, metrics: &BenchmarkMetrics) -> anyhow::Result<()> {
    let filename = format!(
        "raw/{}_{:?}_{}_{}.json",
        metrics.backend, metrics.scale, metrics.ablation, metrics.seed
    );
    let path = out.join(filename);
    std::fs::write(&path, serde_json::to_string_pretty(metrics)?)?;
    tracing::info!("Wrote {}", path.display());
    Ok(())
}

pub fn write_summary_csv(out: &Path, all_metrics: &[BenchmarkMetrics]) -> anyhow::Result<()> {
    let path = out.join("summary.csv");
    let mut wtr = csv::Writer::from_path(&path)?;

    wtr.write_record([
        "backend",
        "scale",
        "ablation",
        "seed",
        "cold",
        "ingest_ms",
        "churn_ratio",
        "pass_rate",
        "false_allow",
        "false_block",
        "schema_loc",
        "rust_loc",
        "avg_p50_us",
        "avg_p95_us",
    ])?;

    for m in all_metrics {
        let avg_p50 = if m.queries.is_empty() {
            0
        } else {
            m.queries.iter().map(|q| q.p50_us).sum::<u64>() / m.queries.len() as u64
        };
        let avg_p95 = if m.queries.is_empty() {
            0
        } else {
            m.queries.iter().map(|q| q.p95_us).sum::<u64>() / m.queries.len() as u64
        };

        wtr.write_record([
            &m.backend,
            &m.scale,
            &m.ablation,
            &m.seed.to_string(),
            &m.cold.to_string(),
            &m.ingest_ms.to_string(),
            &format!("{:.2}", m.churn.ratio),
            &format!("{:.4}", m.correctness.pass_rate()),
            &m.correctness.false_allow.to_string(),
            &m.correctness.false_block.to_string(),
            &m.complexity.schema_loc.to_string(),
            &m.complexity.rust_backend_loc.to_string(),
            &avg_p50.to_string(),
            &avg_p95.to_string(),
        ])?;
    }

    wtr.flush()?;
    tracing::info!("Wrote {}", path.display());
    Ok(())
}

pub fn write_decision_md(
    out: &Path,
    metrics: &[BenchmarkMetrics],
    signal: &crate::signal::SignalReport,
) -> anyhow::Result<()> {
    let path = Path::new("DECISION.md");
    let verdict = &signal.verdict;

    let pg = metrics.iter().find(|m| m.backend == "postgres");
    let tdb = metrics.iter().find(|m| m.backend == "typedb");

    let content = format!(
        r#"# DECISION — TypeDB vs PostgreSQL

## 1. Hypothesis

H0: PostgreSQL bien conçu permet d'implémenter le modèle sémantique gouverné avec performance et complexité acceptables.

H1: TypeDB apporte un avantage structurel sur les opérations combinant relations n-aires, polymorphisme, contradiction, temporalité, provenance et reconstruction contextuelle.

## 2. What we tested

- Domaine: KYB / sanctions / beneficial ownership simplifié
- Requêtes: Q1–Q8 (lookup, bitemporal, contradictions, identité, ownership, compatibilité contextuelle, replay historique, vue rétrospective)
- Scales: S (1k), M (20k), L (200k conditionnel)
- Ablation: 10 dimensions sémantiques candidates
- Backends: PostgreSQL 16 (relationnel typé + GiST + recursive CTE) vs TypeDB 3.12 (schema PERA + TypeQL functions)

## 3. Results

| Backend | Scale | Ingest (ms) | Churn ratio | Pass rate | Schema LOC | Rust LOC | Avg p50 (µs) |
|---------|-------|-------------|-------------|-----------|------------|----------|--------------|
{pg_row}
{tdb_row}

## 4. Where TypeDB wins

{typedb_wins}

## 5. Where PostgreSQL wins

{pg_wins}

## 6. Architectural cost of TypeDB

- Nouveau datastore à opérer (TypeDB Server 3.12)
- Driver async Rust (`typedb-driver` 3.12)
- Écosystème plus restreint que PostgreSQL
- Complexité schema TypeQL: {tdb_schema_loc} LOC vs PostgreSQL: {pg_schema_loc} LOC

## 7. Verdict

**{verdict}**

{verdict_rationale}

---
*Généré automatiquement par le benchmark runner. Seed: {seed}. Voir results/summary.csv pour les métriques complètes.*
"#,
        pg_row = format_metrics_row(pg),
        tdb_row = format_metrics_row(tdb),
        typedb_wins = signal.typedb_wins,
        pg_wins = signal.pg_wins,
        tdb_schema_loc = tdb.map(|m| m.complexity.schema_loc).unwrap_or(0),
        pg_schema_loc = pg.map(|m| m.complexity.schema_loc).unwrap_or(0),
        verdict = verdict,
        verdict_rationale = signal.rationale,
        seed = metrics.first().map(|m| m.seed).unwrap_or(42),
    );

    std::fs::write(path, content)?;
    tracing::info!("Wrote {}", path.display());
    let _ = out;
    Ok(())
}

fn format_metrics_row(m: Option<&BenchmarkMetrics>) -> String {
    match m {
        Some(m) => {
            let avg_p50 = if m.queries.is_empty() {
                0
            } else {
                m.queries.iter().map(|q| q.p50_us).sum::<u64>() / m.queries.len() as u64
            };
            format!(
                "| {} | {} | {} | {:.2} | {:.2}% | {} | {} | {} |",
                m.backend,
                m.scale,
                m.ingest_ms,
                m.churn.ratio,
                m.correctness.pass_rate() * 100.0,
                m.complexity.schema_loc,
                m.complexity.rust_backend_loc,
                avg_p50,
            )
        }
        None => "| — | — | — | — | — | — | — | — |".into(),
    }
}
