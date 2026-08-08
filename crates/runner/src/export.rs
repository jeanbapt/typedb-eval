use std::path::Path;

use benchmark_core::BenchmarkMetrics;

pub fn write_raw_json(out: &Path, metrics: &BenchmarkMetrics) -> anyhow::Result<()> {
    let filename = format!(
        "raw/{}_{}_{}_{}.json",
        metrics.backend, metrics.scale, metrics.ablation, metrics.seed
    );
    let path = out.join(filename);
    std::fs::write(&path, serde_json::to_string_pretty(metrics)?)?;
    tracing::info!("Wrote {}", path.display());
    Ok(())
}

const SUMMARY_HEADER: [&str; 15] = [
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
    "missed_relations",
    "schema_loc",
    "rust_loc",
    "avg_p50_us",
    "avg_p95_us",
];

fn summary_row(m: &BenchmarkMetrics) -> Vec<String> {
    let avg = |f: fn(&benchmark_core::QueryMetrics) -> u64| {
        if m.queries.is_empty() {
            0
        } else {
            m.queries.iter().map(f).sum::<u64>() / m.queries.len() as u64
        }
    };

    vec![
        m.backend.clone(),
        m.scale.clone(),
        m.ablation.clone(),
        m.seed.to_string(),
        m.cold.to_string(),
        m.ingest_ms.to_string(),
        format!("{:.2}", m.churn.ratio),
        format!("{:.4}", m.correctness.pass_rate()),
        m.correctness.false_allow.to_string(),
        m.correctness.false_block.to_string(),
        m.correctness.missed_relations.to_string(),
        m.complexity.schema_loc.to_string(),
        m.complexity.rust_backend_loc.to_string(),
        avg(|q| q.p50_us).to_string(),
        avg(|q| q.p95_us).to_string(),
    ]
}

/// Merge new measurements into `summary.csv` rather than truncating it.
///
/// A run only ever covers one backend/scale/ablation combination, so truncating meant a
/// single invocation erased the whole comparison history. Rows are keyed on
/// (backend, scale, ablation, seed); a re-run replaces its own row and leaves the rest.
pub fn write_summary_csv(out: &Path, all_metrics: &[BenchmarkMetrics]) -> anyhow::Result<()> {
    let path = out.join("summary.csv");

    let mut rows: std::collections::BTreeMap<(String, String, String, String), Vec<String>> =
        std::collections::BTreeMap::new();

    if path.exists() {
        let mut rdr = csv::Reader::from_path(&path)?;
        let existing_header: Vec<String> = rdr
            .headers()?
            .iter()
            .map(|h| h.to_string())
            .collect();
        // Silently drop rows written by an older schema; they cannot be aligned safely.
        if existing_header == SUMMARY_HEADER {
            for record in rdr.records() {
                let record = record?;
                let row: Vec<String> = record.iter().map(|f| f.to_string()).collect();
                if row.len() == SUMMARY_HEADER.len() {
                    rows.insert(key_of(&row), row);
                }
            }
        } else {
            tracing::warn!(
                "summary.csv has an outdated header; previous rows dropped on rewrite"
            );
        }
    }

    for m in all_metrics {
        let row = summary_row(m);
        rows.insert(key_of(&row), row);
    }

    let mut wtr = csv::Writer::from_path(&path)?;
    wtr.write_record(SUMMARY_HEADER)?;
    for row in rows.values() {
        wtr.write_record(row)?;
    }
    wtr.flush()?;

    tracing::info!("Wrote {} ({} rows)", path.display(), rows.len());
    Ok(())
}

fn key_of(row: &[String]) -> (String, String, String, String) {
    (
        row[0].clone(),
        row[1].clone(),
        row[2].clone(),
        row[3].clone(),
    )
}

pub fn write_decision_md(
    out: &Path,
    metrics: &[BenchmarkMetrics],
    signal: &crate::signal::SignalReport,
) -> anyhow::Result<()> {
    let path = out.join("DECISION.md");
    let verdict = &signal.verdict;

    // The decisional scale is the largest one measured; the LOC figures quoted further down
    // are scale-invariant, so any row of the right backend will do for those.
    let pg = largest_scale(metrics, "postgres");
    let tdb = largest_scale(metrics, "typedb");
    let schema_evolution = render_schema_evolution(out);

    let mut sorted: Vec<&BenchmarkMetrics> = metrics.iter().collect();
    sorted.sort_by_key(|m| (scale_rank(&m.scale), m.backend.clone()));
    let results_table = sorted
        .iter()
        .map(|m| format_metrics_row(Some(m)))
        .collect::<Vec<_>>()
        .join("\n");

    let content = format!(
        r#"# DECISION — TypeDB vs PostgreSQL

## 1. Hypothesis

H0: PostgreSQL bien conçu permet d'implémenter le modèle sémantique gouverné avec performance et complexité acceptables.

H1: TypeDB apporte un avantage structurel sur les opérations combinant relations n-aires, polymorphisme, contradiction, temporalité, provenance et reconstruction contextuelle.

## 2. What we tested

- Domaine: KYB / sanctions / beneficial ownership simplifié
- Requêtes: Q1–Q9 (lookup, bitemporal, contradictions, identité, ownership, compatibilité contextuelle, replay historique, vue rétrospective, traversée agnostique aux rôles)
- Scales: S (1k), M (20k), L (200k conditionnel)
- Ablation: 10 dimensions sémantiques candidates
- Backends: PostgreSQL 16 (relationnel typé + GiST + recursive CTE) vs TypeDB 3.12 (schema PERA + TypeQL functions)

## 3. Results

| Backend | Scale | Ingest (ms) | Churn ratio | Pass rate | Schema LOC | Rust LOC | Avg p50 (µs) |
|---------|-------|-------------|-------------|-----------|------------|----------|--------------|
{results_table}

## 4. Schema evolution (query-surface churn)

{schema_evolution}

## 5. Where TypeDB wins

{typedb_wins}

## 6. Where PostgreSQL wins

{pg_wins}

## 7. Architectural cost of TypeDB

- Nouveau datastore à opérer (TypeDB Server 3.12)
- Driver async Rust (`typedb-driver` 3.12)
- Écosystème plus restreint que PostgreSQL
- Complexité schema TypeQL: {tdb_schema_loc} LOC vs PostgreSQL: {pg_schema_loc} LOC

## 8. Verdict

**{verdict}**

{verdict_rationale}

---
*Généré automatiquement par le benchmark runner. Seed: {seed}. Voir results/summary.csv pour les métriques complètes.*
"#,
        results_table = results_table,
        schema_evolution = schema_evolution,
        typedb_wins = signal.typedb_wins,
        pg_wins = signal.pg_wins,
        tdb_schema_loc = tdb.map(|m| m.complexity.schema_loc).unwrap_or(0),
        pg_schema_loc = pg.map(|m| m.complexity.schema_loc).unwrap_or(0),
        verdict = verdict,
        verdict_rationale = signal.rationale,
        seed = metrics.first().map(|m| m.seed).unwrap_or(42),
    );

    std::fs::write(&path, content)?;
    tracing::info!("Wrote {}", path.display());
    let _ = out;
    Ok(())
}

pub fn scale_rank(scale: &str) -> u8 {
    match scale {
        "S" => 0,
        "M" => 1,
        "L" => 2,
        _ => 3,
    }
}

/// The decisional row for a backend: the biggest scale it completed, unablated.
pub fn largest_scale<'a>(
    metrics: &'a [BenchmarkMetrics],
    backend: &str,
) -> Option<&'a BenchmarkMetrics> {
    metrics
        .iter()
        .filter(|m| m.backend == backend && m.ablation == "none")
        .max_by_key(|m| scale_rank(&m.scale))
}

/// Rebuild the reporting artefacts from `raw/*.json` without re-running the benchmark.
///
/// A run only holds its own measurements in memory, so regenerating the decision document
/// after, say, a fresh schema-evolution pass previously meant re-running the whole matrix.
pub fn load_raw_metrics(out: &Path) -> anyhow::Result<Vec<BenchmarkMetrics>> {
    let raw_dir = out.join("raw");
    let mut metrics = Vec::new();
    if !raw_dir.exists() {
        return Ok(metrics);
    }
    for entry in std::fs::read_dir(&raw_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // The agent-retrieval benchmark writes into the same directory with its own shape.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with("agent_retrieval") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)?;
        match serde_json::from_str::<BenchmarkMetrics>(&raw) {
            Ok(m) => metrics.push(m),
            Err(e) => tracing::warn!("skipping {}: {e}", path.display()),
        }
    }
    Ok(metrics)
}

/// Fold the schema-evolution experiment into the decision document if it has been run.
fn render_schema_evolution(out: &Path) -> String {
    let path = out.join("schema_evolution.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return "_Non mesuré — lancer `runner schema-evolution --backend both`._".into();
    };
    let Ok(report) = serde_json::from_str::<crate::schema_evolution::SchemaEvolutionReport>(&raw)
    else {
        return "_Rapport schema-evolution illisible._".into();
    };

    let mut s = String::new();
    s.push_str(
        "Q9 (traversée agnostique aux rôles) est écrite une fois contre l'ontologie de base \
         puis gelée. L'ontologie gagne ensuite un type de partie (`trust`) et une relation \
         4-aire (`control-via-nominee`), et la même requête inchangée est rejouée.\n\n",
    );
    s.push_str("| Backend | Rappel (base) | Rappel (étendu, requête gelée) | Rappel (après réparation) | LOC de réparation |\n");
    s.push_str("|---------|---------------|--------------------------------|---------------------------|-------------------|\n");
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
    for b in &report.backends {
        let missed = b.extended_frozen.missed_relation_types();
        if !missed.is_empty() {
            s.push_str(&format!(
                "\n`{}` perd `{}` silencieusement : réponse plus petite, aucune erreur levée.\n",
                b.backend,
                missed.join("`, `")
            ));
        }
    }
    s
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
