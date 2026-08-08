use benchmark_core::BenchmarkMetrics;

pub struct SignalReport {
    pub should_run_l: bool,
    pub verdict: String,
    pub typedb_wins: String,
    pub pg_wins: String,
    pub rationale: String,
}

pub fn detect_signal(metrics: &[BenchmarkMetrics]) -> SignalReport {
    let pg = metrics.iter().find(|m| m.backend == "postgres" && m.ablation == "none");
    let tdb = metrics.iter().find(|m| m.backend == "typedb" && m.ablation == "none");

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
        } else if pg.correctness.pass_rate() >= tdb.correctness.pass_rate() {
            pg_wins.push(format!(
                "Correctness: PostgreSQL pass rate {:.1}% vs TypeDB {:.1}%",
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

    let verdict = determine_verdict(&typedb_wins, &pg_wins, should_run_l);
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
