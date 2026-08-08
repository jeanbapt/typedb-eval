use benchmark_core::ComplexityMetrics;
use std::path::PathBuf;

pub fn measure_complexity(backend: &str) -> ComplexityMetrics {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    match backend {
        "postgres" => ComplexityMetrics {
            schema_loc: count_lines(&root.join("crates/postgres/schema.sql")),
            query_loc: count_lines_in_dir(&root.join("crates/postgres/src")),
            rust_backend_loc: count_lines_in_dir(&root.join("crates/postgres/src")),
            db_objects: 10,
            indexes: 12,
            triggers_functions: 0,
        },
        "typedb" => ComplexityMetrics {
            schema_loc: count_lines(&root.join("crates/typedb/schema.tql")),
            query_loc: count_lines_in_dir(&root.join("crates/typedb/src")),
            rust_backend_loc: count_lines_in_dir(&root.join("crates/typedb/src")),
            db_objects: 8,
            indexes: 0,
            triggers_functions: 0,
        },
        _ => ComplexityMetrics::default(),
    }
}

fn count_lines(path: &std::path::Path) -> u64 {
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty() && !l.trim().starts_with("--"))
                .count() as u64
        })
        .unwrap_or(0)
}

fn count_lines_in_dir(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    total += content.lines().filter(|l| !l.trim().is_empty()).count() as u64;
                }
            }
        }
    }
    total
}
