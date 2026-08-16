# typedb-eval

Decision benchmark **TypeDB vs PostgreSQL** to evaluate whether a typed graph database can serve as the **operational representation layer** for **SGRS** — a semantic state representation and governance engine built on a **lattice algebra**.

This repo does not try to prove that "TypeDB is better." It answers a concrete architecture question: **Is well-modeled PostgreSQL enough, or does TypeDB reduce the computational and conceptual cost of our semantic algebra enough to change the persistence choice?**

---

## Context: why this benchmark exists

SGRS models complex state including:

- strongly typed n-ary relations;
- explicit contradictions (evidence lattice: `UNKNOWN`, `SUPPORTED`, `REFUTED`, `CONTRADICTORY`);
- identity and entity resolution;
- provenance and governance;
- bitemporality (valid time + knowledge time);
- local contexts (corporate registry, KYC, sanctions) and their compatibility;
- *semantic churn* measurement (unnecessary physical mutations).

Before building the full SGRS stack, we need to know whether **TypeDB** — with its PERA model (polymorphism, roles, native n-ary relations) — provides a **structural** advantage over a properly optimized **PostgreSQL** (typed relational schema, `tstzrange`, GiST, recursive CTE).

A marginal gain (15–30%) does not justify a new persistence technology. Only strong signals count: ≥3× performance on critical queries, ≥3× reduction in implementation complexity, or typed invariants that SQL cannot enforce cleanly.

---

## Why TypeDB deserved a fair trial

What would justify giving up PostgreSQL — thirty years of maturity, tooling, and operators who can fix it at 3 a.m.? One thing: a structural mismatch between the data model and the problem. TypeDB made that hypothesis plausible.

The PERA (*Polymorphic Entity-Relation-Attribute*) model moves a boundary. In the relational world, a relation reduces to a foreign key: a storage artifact the application must reinterpret on every read. In TypeDB, it is a first-class type with named roles, arbitrary arity, and the ability to participate in other relations. The schema stops being a filing plan and becomes an ontology.

Four properties in particular matched SGRS needs:

- **Polymorphic roles.** An `owner` role can be played by a person, company, trust, or sovereign fund — declared once, enforced by the schema. SQL has no supertype notion: you get a discriminator column and a foreign key you must work around.
- **Role-agnostic traversal.** Asking "everything this entity participates in" is one line of TypeQL. In SQL it is a `UNION` over every relevant table, rewritten on every ontology extension.
- **Recursion in the schema.** TypeQL functions carry transitive logic once; recursive CTEs are copied query by query.
- **Typed invariants.** Who can play which role in which relation: guarantees live in the schema, not in application discipline.

For a substrate meant to represent governed semantic state, the argument holds. SGRS maintains a living ontology that keeps growing — new party kinds, control structures, jurisdictions. The cost of an ontology is paid on every extension, far more than at initial design.

Conceptual elegance is a classic trap. A model that reads well can cost 3× latency, give up native bitemporality, and push back to application code what it claimed to absorb. Three questions had to be settled: Does the promise survive measurement? Does modeling comfort cost performance? Does the advantage hold against a *well-modeled* PostgreSQL, not a naive one?

Hence this repo. The schema-evolution experiment (Q9, below) operationalizes the second promise: write a query against the base ontology, freeze it, replay after extension. What each backend stops seeing is measured as lost recall and lines of query repair.

An attractive technology demands measurement before adoption.

---

## What this repo contains

A minimal, deliberately focused prototype:

| Component | Role |
|-----------|------|
| `crates/core` | Domain types, lattice, `ComplianceStore` trait, ground-truth oracle |
| `crates/fixtures` | Deterministic generator (fixed seed) + expected answers |
| `crates/postgres` | Bitemporal SQL schema + PostgreSQL 16 implementation |
| `crates/typedb` | TypeQL schema + TypeDB 3.12 implementation |
| `crates/runner` | Benchmark CLI, metrics, ablation, export |
| `results/` | Raw measurements + `summary.csv` |
| `results/DECISION.md` | Final verdict (`KEEP POSTGRES` / `INVESTIGATE HYBRID` / `PURSUE TYPEDB` / `INCONCLUSIVE`) |

**Benchmark domain**: simplified KYB / sanctions / beneficial ownership. Not a regulatory implementation — a generator of representative structural problems (indirect ownership, uncertain identity, contradictory sources, retroactive corrections, etc.).

**Out of scope**: UI, public API, GraphQL, LLM, embeddings, sheaf diffusion / Laplacian / H⁰ (stays in `sgrs-core`), rules engine.

**Future work**: persisting and querying stalks and restriction maps in-database (Q10–Q12) — evaluate whether TypeDB adds structural advantage over a strong Postgres for cross-context state before investing in a second storage layer.

---

## Architecture

```
                    Benchmark runner
                           |
                    semantic fixtures
                           |
                  +--------v--------+
                  |    Rust core    |
                  |  lattice, oracle|
                  +--------+--------+
                           |
                      Store trait
                   +-------+-------+
                   |               |
             PostgreSQL         TypeDB
                SQL             TypeQL
```

Both backends implement the same `ComplianceStore` interface. Business logic is not deliberately shifted to favor one candidate — each backend uses its natural primitives when they reasonably replace application code.

---

## Prerequisites

- [Rust](https://rustup.rs/) 1.75+
- [Docker](https://docs.docker.com/get-docker/) & Docker Compose

## Quick start

```bash
# Start PostgreSQL 16 and TypeDB 3.12.2
docker compose up -d

# Wait for services to be ready, then:
cargo run -p benchmark-runner -- --backend both --scale S --seed 42 --out results/
```

Full run (scale M, ~20k events):

```bash
cargo run -p benchmark-runner -- \
  --backend both \
  --scale M \
  --seed 42 \
  --out results/
```

## Useful commands

```bash
# Help
cargo run -p benchmark-runner -- --help

# Generate fixtures only
cargo run -p benchmark-runner -- generate --scale M --seed 42 --out fixtures/out/

# Ablation of one semantic dimension
cargo run -p benchmark-runner -- \
  --backend both \
  --scale M \
  --seed 42 \
  --ablate identity \
  --out results/

# Full ablation (all dimensions)
cargo run -p benchmark-runner -- ablation --backend both --scale M --seed 42 --out results/

# Regenerate summary.csv and DECISION.md from results/raw/ without re-running the benchmark
cargo run -p benchmark-runner -- report --out results/

# Unit tests
cargo test
```

---

## Benchmark queries (Q1–Q9)

| ID | Question |
|----|----------|
| Q1 | Who is the currently known beneficial owner? |
| Q2 | What did we believe at time T about a situation valid at time V? (bitemporal) |
| Q3 | Which active assertions contradict each other for this entity? |
| Q4 | Should two identities (e.g. John Smith / Jonathan Smith) be merged? |
| Q5 | Does a sanctioned person directly or indirectly control this company? |
| Q6 | Are Corporate Registry / KYC / Sanctions contexts compatible? |
| Q7 | Given knowledge at a past date, would the decision have been ALLOW / REVIEW / BLOCK? |
| Q8 | Given today's knowledge, how should the same past situation be classified? (Q7 ≠ Q8) |
| Q9 | What does this entity participate in, regardless of relation type or role? |

---

## Schema evolution (Q9)

Q9 measures the property hardest to get in SQL: query an entity's relations without enumerating relation types.

The protocol has three steps. Each backend implements Q9 once against the base ontology. The ontology then gains a party kind (`trust`) and a 4-ary polymorphic relation (`control-via-nominee`, where `controller` can be a person, company, or trust). The original unchanged query is replayed.

Both backends pay a schema cost. Only one pays a query cost.

```bash
cargo run -p benchmark-runner -- schema-evolution --backend both --scale S --seed 42 --out results/
```

Two metrics are written to `results/SCHEMA_EVOLUTION.md`:

- **recall after extension** — fraction of real relations the frozen query still returns. Below 100%, the backend silently under-reports after an ontology change (a correctness issue, not performance);
- **repair LOC** — query code needed to get back to 100%.

---

## Scales and stopping criteria

| Scale | Events | Usage |
|-------|--------|-------|
| **S** | 1,000 | Fast validation, CI |
| **M** | 20,000 | Main decision run |
| **L** | 200,000 | Conditional — only if M shows a ≥2× signal |

After M, if no significant TypeDB advantage is detected, the benchmark stops and writes `results/DECISION.md` with verdict **KEEP POSTGRES**. That is a valid, expected outcome.

A comparative verdict requires both backends to finish. If only one ran, the document is marked **INCONCLUSIVE** rather than drawing conclusions from half the data.

---

## Collected metrics

- **Performance**: p50 / p95 / p99, throughput, round-trips per query (ingest reported separately)
- **Correctness**: systematic comparison vs oracle (false allow/block/review, false merge/split, missed contradiction)
- **Semantic churn**: `physical_mutations / semantic_changes`
- **Complexity**: schema LOC, query LOC, Rust backend LOC, DB object count
- **Schema evolution**: recall preserved by a frozen query after ontology extension, repair LOC
- **Ablation**: impact of each semantic dimension (identity, evidence, valid_time, knowledge_time, jurisdiction, role, source, provenance, governance…)

Results in `results/summary.csv` and `results/raw/*.json`.

---

## Agent benchmark (retrieval via MCP)

In addition to the direct benchmark (Rust drivers), this repo separately measures **retrieval as an LLM agent would do it via MCP servers** — the realistic path for SGRS when an agent queries the database in natural language.

| Server | URL | Role |
|--------|-----|------|
| **postgres-mcp** | `http://localhost:8899/mcp` | `list_tables`, `describe_table`, `execute_sql` |
| **typedb-mcp** | `http://localhost:8001/mcp` | `query`, TypeDB database management |

```bash
# Start Postgres, TypeDB, and both MCP servers
docker compose up -d

# Agent retrieval benchmark (preloads data, then tests via MCP)
cargo run -p benchmark-runner -- agent-retrieval --scale S --seed 42 --out results/
```

The workflow simulates an agent:
1. **Schema introspection** via MCP (`list_tables` / `describe_table` or TypeQL probe)
2. **NL prompt** (e.g. "Who are the beneficial owners?")
3. **Execute** generated query via `execute_sql` or `query`
4. Metrics: p50/p95 latency, MCP round-trips, success rate

Results in `results/agent_retrieval_summary.csv`.

For Cursor, copy [`.cursor/mcp.json.example`](.cursor/mcp.json.example) to `.cursor/mcp.json`.

---

## Latest results (seed 42)

After TypeDB backend optimizations (server-side recursion, bitemporal pushdown, given-parameterized queries) and a Postgres Q9 fix (`entity_participation` index — frozen traversal survives ontology extension without query repair). TypeDB CE 3.12.2.

### Scale S (~1k events, warm)

| Backend | Pass rate | Ingest | Churn | Avg p50 | Missed relations | False allow/block |
|---------|-----------|--------|-------|---------|------------------|-------------------|
| **PostgreSQL** | 94.7% | 0.4 s | 1.16 | 1.1 ms | 0 | 0 |
| **TypeDB** | **98.6%** | 2.6 s | 1.01 | 6.5 ms | 0 | 0 |

### Scale M (~20k events, cold)

| Backend | Pass rate | Ingest | Churn | Avg p50 | Missed relations | False allow/block |
|---------|-----------|--------|-------|---------|------------------|-------------------|
| **PostgreSQL** | 86.4% | 24.3 s | 1.17 | 46 ms | 0 | 0 |
| **TypeDB** | **98.2%** | 68.4 s | 1.00 | 133 ms | 0 | 0 |

At M, PostgreSQL is ~2.9× faster on avg p50; neither backend shows false allow/block. Remaining gaps are mostly Q7/Q8 historical-replay mismatches vs the oracle, not safety violations.

**Schema evolution (Q9, scale S)**: both backends keep **100%** recall with a frozen query and **0** repair LOC ([`results/SCHEMA_EVOLUTION.md`](results/SCHEMA_EVOLUTION.md)). Postgres uses a participation index written on ingest; TypeDB uses role-agnostic `$r links ($role: $x)` patterns.

**Verdict at M**: **[KEEP POSTGRES](results/DECISION.md)** — no structural TypeDB win on performance or schema evolution at this scale; TypeDB retains a correctness edge on pass rate. Scale S alone suggested INVESTIGATE HYBRID before the Postgres Q9 fix and the M run.

---

## Bitemporality: what we learned

This benchmark does **not** show that TypeDB is bad at managing time. Early failures (~59% TypeDB pass rate) came mainly from **implementation bugs** in the Rust/TypeQL backend, not an engine limitation:

| Fixed issue | Impact |
|-------------|--------|
| Assertion IDs not synced between oracle and backends | Q7/Q8 never saw `CloseAssertionKnowledge` / `RetroactiveCorrection` |
| `beneficial_owners` only followed direct person→company edges | Company→company→person chains ignored |
| Incomplete bitemporal filters (missing `valid_from` / `known_from` lower bounds) | Future assertions counted as visible |
| Invalid TypeQL for `known-to` (`update` vs broken `delete`/`insert`) | Ingest failed on knowledge closure |
| Q6 compared all evidence pairs without grouping by context | 28 false failures |

**Where TypeDB is still weaker in this evaluation**:

1. **Ergonomics** — even with schema functions and server-side filters, combining recursion, polymorphism, and bitemporal constraints in TypeQL is more verbose than a Postgres recursive CTE with `tstzrange @> timestamptz`.
2. **Pushdown** — Postgres filters intervals via GiST in SQL; the TypeDB backend now pushes bitemporal bounds into TypeQL, but interval indexing semantics still differ from native `tstzrange`.
3. **Performance** — ~6× slower avg p50 on scale S (~2.9× on scale M) after backend optimizations (down from ~73× on S before server-side recursion and query parameterization).

In short: TypeDB's **model** supports the bitemporality SGRS requires (Q7/Q8 pass). The **cost** is mostly latency and query ergonomics, not broken temporal semantics.

---

## Verdict

See [results/DECISION.md](results/DECISION.md) after running the benchmark. The document covers:

1. Hypothesis tested
2. Scope
3. Results
4. Schema evolution (query-surface churn)
5. Where TypeDB wins / where PostgreSQL wins
6. Architectural cost of TypeDB
7. Explicit verdict

---

## Contributors

- **Deal ex Machina SAS** — benchmark design, PostgreSQL backend, oracle, fixtures, runner
- **[Joshua Send](https://github.com/flyingsilverfin)** ([TypeDB](https://typedb.com/)) — TypeDB backend optimizations: server-side recursion via schema functions, bitemporal filtering in TypeQL, `given`-parameterized queries (driver 3.12.3), Q9 vocabulary alignment with the oracle, and schema-evolution signal fixes

---

## License

MIT — see [LICENSE](LICENSE).

Copyright (c) 2026 Deal ex Machina SAS.

## Links

- [TypeDB](https://typedb.com/) — typed graph database (PERA model)
- **SGRS** — target product (lattice algebra for governed semantic state)
