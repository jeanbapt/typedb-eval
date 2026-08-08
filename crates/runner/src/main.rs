mod agent_mcp;
mod benchmark;
mod complexity;
mod correctness;
mod export;
mod schema_evolution;
mod signal;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use benchmark_core::AblationDimension;
use benchmark_core::Scale;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackendArg {
    Postgres,
    Typedb,
    Both,
}

#[derive(Debug, Parser)]
#[command(name = "runner", about = "TypeDB vs PostgreSQL benchmark")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long, value_enum, default_value = "both")]
    backend: BackendArg,

    #[arg(long, default_value = "M")]
    scale: String,

    #[arg(long, default_value = "42")]
    seed: u64,

    #[arg(long, default_value = "none")]
    ablate: String,

    #[arg(long, default_value = "results")]
    out: PathBuf,

    #[arg(long, default_value_t = false)]
    cold: bool,

    #[arg(long, default_value_t = false)]
    skip_l: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run the full benchmark
    Run,
    /// Generate fixtures only
    Generate {
        #[arg(long, default_value = "M")]
        scale: String,
        #[arg(long, default_value = "42")]
        seed: u64,
        #[arg(long, default_value = "fixtures/out")]
        out: PathBuf,
    },
    /// Run ablation study across all dimensions
    Ablation {
        #[arg(long, value_enum, default_value = "both")]
        backend: BackendArg,
        #[arg(long, default_value = "M")]
        scale: String,
        #[arg(long, default_value = "42")]
        seed: u64,
        #[arg(long, default_value = "results")]
        out: PathBuf,
    },
    /// Measure query-surface churn when the ontology is extended
    SchemaEvolution {
        #[arg(long, value_enum, default_value = "both")]
        backend: BackendArg,
        #[arg(long, default_value = "S")]
        scale: String,
        #[arg(long, default_value = "42")]
        seed: u64,
        #[arg(long, default_value = "results")]
        out: PathBuf,
    },
    /// Run agent retrieval benchmark via Postgres + TypeDB MCP servers
    AgentRetrieval {
        #[arg(long, default_value = "S")]
        scale: String,
        #[arg(long, default_value = "42")]
        seed: u64,
        #[arg(long, default_value = "results")]
        out: PathBuf,
        /// Preload dataset via native backends before MCP retrieval
        #[arg(long, default_value_t = true)]
        preload: bool,
    },
}

fn parse_scale(s: &str) -> Scale {
    match s.to_uppercase().as_str() {
        "S" => Scale::S,
        "L" => Scale::L,
        _ => Scale::M,
    }
}

fn parse_ablation(s: &str) -> AblationDimension {
    match s {
        "identity" => AblationDimension::Identity,
        "evidence" => AblationDimension::Evidence,
        "valid_time" => AblationDimension::ValidTime,
        "knowledge_time" => AblationDimension::KnowledgeTime,
        "jurisdiction" => AblationDimension::Jurisdiction,
        "role" => AblationDimension::Role,
        "source" => AblationDimension::Source,
        "source_authority" => AblationDimension::SourceAuthority,
        "provenance" => AblationDimension::Provenance,
        "governance" => AblationDimension::Governance,
        _ => AblationDimension::None,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Generate { scale, seed, out }) => {
            benchmark::write_fixtures_to_disk(parse_scale(&scale), seed, &out)?;
        }
        Some(Commands::Ablation {
            backend,
            scale,
            seed,
            out,
        }) => {
            benchmark::run_ablation(backend, parse_scale(&scale), seed, &out).await?;
        }
        Some(Commands::SchemaEvolution {
            backend,
            scale,
            seed,
            out,
        }) => {
            schema_evolution::run_schema_evolution(backend, parse_scale(&scale), seed, &out)
                .await?;
        }
        Some(Commands::AgentRetrieval {
            scale,
            seed,
            out,
            preload,
        }) => {
            agent_mcp::run_agent_retrieval(parse_scale(&scale), seed, &out, preload).await?;
        }
        Some(Commands::Run) | None => {
            let scale = parse_scale(&cli.scale);
            let ablation = parse_ablation(&cli.ablate);
            benchmark::run_benchmark(
                cli.backend,
                scale,
                cli.seed,
                ablation,
                &cli.out,
                cli.cold,
                cli.skip_l,
            )
            .await?;
        }
    }

    Ok(())
}
