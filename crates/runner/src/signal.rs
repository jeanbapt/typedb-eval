use benchmark_core::BenchmarkMetrics;

pub struct SignalReport {
    pub should_run_l: bool,
    pub verdict: String,
    pub typedb_wins: String,
    pub pg_wins: String,
    pub rationale: String,
}

/// Summary of the schema-evolution experiment, when it has been run.
pub struct SchemaEvolutionSignal {
    /// Backends whose frozen Q9 lost recall after the ontology was extended.
    pub lost_recall: Vec<(String, f64, u64)>,
    /// Backends whose frozen Q9 kept (or improved) their base recall with no query edit.
    pub held_recall: Vec<String>,
}

pub fn load_schema_evolution_signal(out: &std::path::Path) -> Option<SchemaEvolutionSignal> {
    let raw = std::fs::read_to_string(out.join("schema_evolution.json")).ok()?;
    let report: crate::schema_evolution::SchemaEvolutionReport =
        serde_json::from_str(&raw).ok()?;

    let mut lost_recall = Vec::new();
    let mut held_recall = Vec::new();
    for b in &report.backends {
        let recall = b.extended_frozen.recall();
        // A backend "loses" the experiment only if the extension made its frozen query
        // worse than its own base run, or it needed query repair. Comparing against an
        // absolute cutoff instead would misattribute pre-existing base-generation gaps
        // (unrelated to the extension) as evolution failures.
        if b.repair_loc > 0 || recall + 1e-9 < b.base.recall() {
            lost_recall.push((b.backend.clone(), recall, b.repair_loc));
        } else {
            held_recall.push(b.backend.clone());
        }
    }
    Some(SchemaEvolutionSignal {
        lost_recall,
        held_recall,
    })
}

pub fn detect_signal(metrics: &[BenchmarkMetrics]) -> SignalReport {
    detect_signal_with(metrics, None)
}

pub fn detect_signal_with(
    metrics: &[BenchmarkMetrics],
    schema_evolution: Option<&SchemaEvolutionSignal>,
) -> SignalReport {
    // Compare at the largest scale each backend completed, not at whichever row happens to
    // come first: a verdict drawn from the S run would not be the decisional one.
    let pg = crate::export::largest_scale(metrics, "postgres");
    let tdb = crate::export::largest_scale(metrics, "typedb");

    let mut should_run_l = false;
    let mut typedb_wins = Vec::new();
    let mut pg_wins = Vec::new();

    if let (Some(pg), Some(tdb)) = (pg, tdb) {
        // Performance comparison
        let pg_avg_p50 = avg_p50(pg);
        let tdb_avg_p50 = avg_p50(tdb);

        if tdb_avg_p50 > 0 && pg_avg_p50 as f64 / tdb_avg_p50 as f64 >= 3.0 {
            typedb_wins.push(format!(
                "Performance: TypeDB ~{:.1}x faster on avg p50 ({}µs vs {}µs)",
                pg_avg_p50 as f64 / tdb_avg_p50 as f64,
                tdb_avg_p50,
                pg_avg_p50
            ));
            should_run_l = true;
        } else if pg_avg_p50 > 0 && tdb_avg_p50 as f64 / pg_avg_p50 as f64 >= 3.0 {
            pg_wins.push(format!(
                "Performance: PostgreSQL ~{:.1}x faster on avg p50 ({}µs vs {}µs)",
                tdb_avg_p50 as f64 / pg_avg_p50 as f64,
                pg_avg_p50,
                tdb_avg_p50
            ));
        } else {
            pg_wins.push("Performance: no significant TypeDB advantage (<3x threshold)".into());
        }

        // Complexity comparison
        let complexity_ratio = pg.complexity.rust_backend_loc as f64
            / tdb.complexity.rust_backend_loc.max(1) as f64;
        if complexity_ratio >= 3.0 {
            typedb_wins.push(format!(
                "Complexity: PostgreSQL backend ~{:.1}x more Rust LOC ({} vs {})",
                complexity_ratio,
                pg.complexity.rust_backend_loc,
                tdb.complexity.rust_backend_loc
            ));
            should_run_l = true;
        } else if 1.0 / complexity_ratio >= 3.0 {
            pg_wins.push(format!(
                "Complexity: TypeDB backend ~{:.1}x more Rust LOC",
                1.0 / complexity_ratio
            ));
        }

        // Correctness
        if tdb.correctness.pass_rate() > pg.correctness.pass_rate() + 0.05 {
            typedb_wins.push(format!(
                "Correctness: TypeDB pass rate {:.1}% vs PostgreSQL {:.1}%",
                tdb.correctness.pass_rate() * 100.0,
                pg.correctness.pass_rate() * 100.0
            ));
            } else if pg.correctness.pass_rate() > tdb.correctness.pass_rate() + 0.05 {
                pg_wins.push(format!(
                    "Correctness: PostgreSQL pass rate {:.1}% vs TypeDB {:.1}%",
                    pg.correctness.pass_rate() * 100.0,
                    tdb.correctness.pass_rate() * 100.0
                ));
            } else {
                pg_wins.push(format!(
                    "Correctness: comparable (PG {:.1}%, TDB {:.1}%) — no structural advantage",
                    pg.correctness.pass_rate() * 100.0,
                    tdb.correctness.pass_rate() * 100.0
                ));
            }

        // Churn
        if pg.churn.ratio > tdb.churn.ratio * 1.5 {
            typedb_wins.push(format!(
                "Semantic churn: TypeDB ratio {:.2} vs PostgreSQL {:.2}",
                tdb.churn.ratio, pg.churn.ratio
            ));
        } else {
            pg_wins.push(format!(
                "Semantic churn: comparable (PG {:.2}, TDB {:.2})",
                pg.churn.ratio, tdb.churn.ratio
            ));
        }

        // Schema complexity
        pg_wins.push(format!(
            "Maturity: PostgreSQL schema {} LOC, proven GiST/recursive CTE patterns",
            pg.complexity.schema_loc
        ));
    } else {
        pg_wins.push("Only one backend ran successfully".into());
    }

    // Schema evolution: independent of scale, so it is folded in whenever available.
    if let Some(se) = schema_evolution {
        for (backend, recall, repair_loc) in &se.lost_recall {
            let line = format!(
                "Schema evolution: {backend} silently drops to {:.1}% recall after an ontology \
                 extension ({repair_loc} LOC of query repair)",
                recall * 100.0
            );
            if backend == "postgres" {
                typedb_wins.push(line);
            } else {
                pg_wins.push(line);
            }
        }
        if se.held_recall.iter().any(|b| b == "typedb") && !se.lost_recall.is_empty() {
            typedb_wins.push(
                "Schema evolution: TypeDB keeps its recall through the extension with zero \
                 query edits (role-agnostic traversal)"
                    .into(),
            );
        }
    }

    let both_backends_ran = metrics.iter().any(|m| m.backend == "postgres")
        && metrics.iter().any(|m| m.backend == "typedb");

    let verdict = if both_backends_ran {
        determine_verdict(&typedb_wins, &pg_wins, should_run_l)
    } else {
        // A comparative verdict from a single backend is not a verdict.
        "INCONCLUSIVE".into()
    };
    let rationale = build_rationale(&verdict, &typedb_wins, &pg_wins);

    SignalReport {
        should_run_l,
        verdict,
        typedb_wins: if typedb_wins.is_empty() {
            "- Aucun avantage structurel identifié".into()
        } else {
            typedb_wins
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        pg_wins: if pg_wins.is_empty() {
            "- Aucun avantage identifié".into()
        } else {
            pg_wins
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        rationale,
    }
}

fn avg_p50(m: &BenchmarkMetrics) -> u64 {
    if m.queries.is_empty() {
        0
    } else {
        m.queries.iter().map(|q| q.p50_us).sum::<u64>() / m.queries.len() as u64
    }
}

fn determine_verdict(typedb_wins: &[String], pg_wins: &[String], signal: bool) -> String {
    let strong_typedb = typedb_wins.len() >= 2 && signal;
    let no_typedb = typedb_wins.is_empty() || (typedb_wins.len() == 1 && pg_wins.len() >= 3);

    if strong_typedb {
        "PURSUE TYPEDB".into()
    } else if typedb_wins.len() >= 1 && !no_typedb {
        "INVESTIGATE HYBRID".into()
    } else {
        "KEEP POSTGRES".into()
    }
}

fn build_rationale(verdict: &str, typedb_wins: &[String], pg_wins: &[String]) -> String {
    match verdict {
        "PURSUE TYPEDB" => format!(
            "TypeDB montre {} signal(aux) structurel(s) dépassant les seuils PRD (≥3x performance ou complexité). \
             Une Phase 2 d'architecture est justifiée.",
            typedb_wins.len()
        ),
        "INVESTIGATE HYBRID" => format!(
            "TypeDB apporte un avantage net sur {} dimension(s) mais insuffisant pour remplacer PostgreSQL. \
             Investiguer un modèle hybride ciblé.",
            typedb_wins.len()
        ),
        _ => format!(
            "Après tests M, aucune opération importante ne dépasse ~2x de gain TypeDB. \
             PostgreSQL {} avantage(s) identifié(s). Le coût technologique de TypeDB n'est pas justifié \
             pour ce substrat sémantique gouverné.",
            pg_wins.len()
        ),
    }
}
