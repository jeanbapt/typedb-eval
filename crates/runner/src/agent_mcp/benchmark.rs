use std::path::Path;
use std::time::Instant;

use benchmark_core::oracle::Oracle;
use benchmark_core::{FixtureBundle, QueryFamily};
use benchmark_fixtures::generate_fixtures as build_fixtures;
use hdrhistogram::Histogram;
use serde::Serialize;

use super::client::{tool_text, McpHttpClient};
use super::prompts::{nl_prompt, postgres_sql, typedb_typeql};
use super::tokens::TokenBudget;
use crate::benchmark::{run_postgres, run_typedb, TYPEDB_DATABASE};
use benchmark_core::{AblationDimension, Scale};

const POSTGRES_MCP_URL: &str = "http://localhost:8899";
const TYPEDB_MCP_URL: &str = "http://localhost:8001";

#[derive(Debug, Clone, Serialize)]
pub struct AgentRetrievalMetrics {
    pub backend: String,
    pub mcp_url: String,
    pub scale: String,
    pub seed: u64,
    pub probes: u64,
    pub schema_introspection_ms: u64,
    pub avg_retrieval_p50_us: u64,
    pub avg_retrieval_p95_us: u64,
    pub mcp_round_trips: u64,
    pub success_rate: f64,
    pub errors: u64,
    pub schema_context_tokens: u64,
    pub nl_prompt_tokens: u64,
    pub query_generation_tokens: u64,
    pub tool_response_tokens: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_estimated_tokens: u64,
}

pub async fn run_agent_retrieval(
    scale: Scale,
    seed: u64,
    out: &Path,
    preload: bool,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(out.join("raw"))?;

    if preload {
        tracing::info!("Preloading data via native backends before MCP retrieval tests");
        let bundle = build_fixtures(seed, scale);
        let _ = run_postgres(&bundle, scale, seed, AblationDimension::None, false).await;
        let _ = run_typedb(&bundle, scale, seed, AblationDimension::None, false).await;
    }

    let bundle = build_fixtures(seed, scale);
    let oracle = Oracle::from_events(&bundle.events);

    let pg_metrics = run_mcp_backend(
        "postgres-mcp",
        POSTGRES_MCP_URL,
        &bundle,
        &oracle,
        scale,
        seed,
        BackendKind::Postgres,
    )
    .await;
    let tdb_metrics = run_mcp_backend(
        "typedb-mcp",
        TYPEDB_MCP_URL,
        &bundle,
        &oracle,
        scale,
        seed,
        BackendKind::TypeDb,
    )
    .await;

    write_agent_json(out, "postgres-mcp", &pg_metrics)?;
    write_agent_json(out, "typedb-mcp", &tdb_metrics)?;
    write_agent_csv(out, &[pg_metrics, tdb_metrics])?;

    Ok(())
}

#[derive(Clone, Copy)]
enum BackendKind {
    Postgres,
    TypeDb,
}

async fn run_mcp_backend(
    name: &str,
    url: &str,
    bundle: &FixtureBundle,
    oracle: &Oracle,
    scale: Scale,
    seed: u64,
    kind: BackendKind,
) -> AgentRetrievalMetrics {
    let mut client = McpHttpClient::new(url);
    let mut errors = 0u64;
    let mut successes = 0u64;

    if client.initialize().await.is_err() {
        tracing::error!("{name}: MCP initialize failed at {url}");
        return empty_metrics(name, url, scale, seed, 1);
    }

    let mut tokens = TokenBudget::default();

    // Agent workflow step 1: schema introspection via MCP
    let schema_start = Instant::now();
    let mut round_trips = 1u64;
    match kind {
        BackendKind::Postgres => {
            match client.call_tool("list_tables", serde_json::json!({})).await {
                Ok(r) => {
                    tokens.record_schema(&tool_text(&r));
                    round_trips += 1;
                }
                Err(_) => errors += 1,
            }
            match client
                .call_tool(
                    "describe_table",
                    serde_json::json!({ "table": "ownership_assertion" }),
                )
                .await
            {
                Ok(r) => {
                    tokens.record_schema(&tool_text(&r));
                    round_trips += 1;
                }
                Err(_) => errors += 1,
            }
        }
        BackendKind::TypeDb => {
            match client.list_tools().await {
                Ok(names) => {
                    tokens.record_schema(&names.join("\n"));
                    round_trips += 1;
                }
                Err(_) => errors += 1,
            }
            match client
                .call_tool(
                    "query",
                    serde_json::json!({
                        "query": "match $x isa person; select $x;",
                        "database": TYPEDB_DATABASE,
                        "transaction_type": "read"
                    }),
                )
                .await
            {
                Ok(r) => {
                    tokens.record_schema(&tool_text(&r));
                    round_trips += 1;
                }
                Err(_) => errors += 1,
            }
        }
    }
    let schema_introspection_ms = schema_start.elapsed().as_millis() as u64;

    // Agent workflow step 2: retrieval probes (NL prompt -> generated query -> MCP execute)
    let mut hist = Histogram::<u64>::new(3).unwrap();
    let probes: Vec<_> = bundle
        .probes
        .iter()
        .filter(|p| matches!(p.family, QueryFamily::Q1BeneficialOwner | QueryFamily::Q5OwnershipExposure))
        .take(20)
        .collect();

    for probe in &probes {
        let nl = nl_prompt(probe);
        let query = match kind {
            BackendKind::Postgres => postgres_sql(probe),
            BackendKind::TypeDb => typedb_typeql(probe),
        };
        let _expected = oracle.answer_probe(probe);

        let t0 = Instant::now();
        let result = match kind {
            BackendKind::Postgres => {
                client
                    .call_tool(
                        "execute_sql",
                        serde_json::json!({ "sql": query }),
                    )
                    .await
            }
            BackendKind::TypeDb => {
                client
                    .call_tool(
                        "query",
                        serde_json::json!({
                            "query": query,
                            "database": TYPEDB_DATABASE,
                            "transaction_type": "read"
                        }),
                    )
                    .await
            }
        };
        round_trips += 1;

        let response_text = result
            .as_ref()
            .ok()
            .filter(|r| !r.is_error)
            .map(tool_text)
            .unwrap_or_default();

        match result {
            Ok(r) if !r.is_error => successes += 1,
            _ => errors += 1,
        }
        tokens.record_probe(&nl, &query, &response_text);
        hist.record(t0.elapsed().as_micros() as u64).ok();
    }

    let total = probes.len() as u64;
    build_metrics(
        name,
        url,
        scale,
        seed,
        total,
        schema_introspection_ms,
        &hist,
        round_trips,
        successes,
        errors,
        &tokens,
    )
}

fn empty_metrics(name: &str, url: &str, scale: Scale, seed: u64, errors: u64) -> AgentRetrievalMetrics {
    AgentRetrievalMetrics {
        backend: name.into(),
        mcp_url: url.into(),
        scale: format!("{scale:?}"),
        seed,
        probes: 0,
        schema_introspection_ms: 0,
        avg_retrieval_p50_us: 0,
        avg_retrieval_p95_us: 0,
        mcp_round_trips: 0,
        success_rate: 0.0,
        errors,
        schema_context_tokens: 0,
        nl_prompt_tokens: 0,
        query_generation_tokens: 0,
        tool_response_tokens: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_estimated_tokens: 0,
    }
}

fn build_metrics(
    name: &str,
    url: &str,
    scale: Scale,
    seed: u64,
    probes: u64,
    schema_introspection_ms: u64,
    hist: &Histogram<u64>,
    round_trips: u64,
    successes: u64,
    errors: u64,
    tokens: &TokenBudget,
) -> AgentRetrievalMetrics {
    AgentRetrievalMetrics {
        backend: name.into(),
        mcp_url: url.into(),
        scale: format!("{scale:?}"),
        seed,
        probes,
        schema_introspection_ms,
        avg_retrieval_p50_us: hist.value_at_quantile(0.5),
        avg_retrieval_p95_us: hist.value_at_quantile(0.95),
        mcp_round_trips: round_trips,
        success_rate: if probes == 0 {
            0.0
        } else {
            successes as f64 / probes as f64
        },
        errors,
        schema_context_tokens: tokens.schema_context,
        nl_prompt_tokens: tokens.nl_prompts,
        query_generation_tokens: tokens.query_generation,
        tool_response_tokens: tokens.tool_responses,
        total_input_tokens: tokens.input_tokens(),
        total_output_tokens: tokens.output_tokens(),
        total_estimated_tokens: tokens.total_tokens(),
    }
}

fn write_agent_csv(out: &Path, metrics: &[AgentRetrievalMetrics]) -> anyhow::Result<()> {
    let path = out.join("agent_retrieval_summary.csv");
    let mut wtr = csv::Writer::from_path(&path)?;
    wtr.write_record([
        "backend",
        "mcp_url",
        "scale",
        "seed",
        "probes",
        "schema_introspection_ms",
        "avg_p50_us",
        "avg_p95_us",
        "mcp_round_trips",
        "success_rate",
        "errors",
        "schema_context_tokens",
        "nl_prompt_tokens",
        "query_generation_tokens",
        "tool_response_tokens",
        "total_input_tokens",
        "total_output_tokens",
        "total_estimated_tokens",
    ])?;
    for m in metrics {
        wtr.write_record([
            &m.backend,
            &m.mcp_url,
            &m.scale,
            &m.seed.to_string(),
            &m.probes.to_string(),
            &m.schema_introspection_ms.to_string(),
            &m.avg_retrieval_p50_us.to_string(),
            &m.avg_retrieval_p95_us.to_string(),
            &m.mcp_round_trips.to_string(),
            &format!("{:.4}", m.success_rate),
            &m.errors.to_string(),
            &m.schema_context_tokens.to_string(),
            &m.nl_prompt_tokens.to_string(),
            &m.query_generation_tokens.to_string(),
            &m.tool_response_tokens.to_string(),
            &m.total_input_tokens.to_string(),
            &m.total_output_tokens.to_string(),
            &m.total_estimated_tokens.to_string(),
        ])?;
    }
    wtr.flush()?;
    tracing::info!("Wrote {}", path.display());
    Ok(())
}

fn write_agent_json(out: &Path, name: &str, m: &AgentRetrievalMetrics) -> anyhow::Result<()> {
    let path = out.join(format!("raw/agent_retrieval_{name}_{}.json", m.scale));
    std::fs::write(&path, serde_json::to_string_pretty(m)?)?;
    tracing::info!("Wrote {}", path.display());
    Ok(())
}
