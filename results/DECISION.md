# DECISION — TypeDB vs PostgreSQL

## 1. Hypothesis

H0: A well-designed PostgreSQL can implement the governed semantic model with acceptable performance and complexity.

H1: TypeDB provides a structural advantage on operations combining n-ary relations, polymorphism, contradiction, temporality, provenance, and contextual reconstruction.

## 2. What we tested

- Domain: simplified KYB / sanctions / beneficial ownership
- Queries: Q1–Q9 (lookup, bitemporal, contradictions, identity, ownership, context compatibility, historical replay, retrospective view, role-agnostic traversal)
- Scales: S (1k), M (20k), L (200k conditional)
- Ablation: 10 candidate semantic dimensions
- Backends: PostgreSQL 16 (typed relational + GiST + recursive CTE) vs TypeDB 3.12 (PERA schema + TypeQL functions)

## 3. Results

| Backend | Scale | Ingest (ms) | Churn ratio | Pass rate | Schema LOC | Rust LOC | Avg p50 (µs) |
|---------|-------|-------------|-------------|-----------|------------|----------|--------------|
| postgres | S | 2332 | 1.16 | 94.72% | 149 | 1270 | 4024 |
| typedb | S | 6363 | 1.01 | 98.33% | 138 | 1821 | 294558 |

Correctness breakdown (TypeDB, scale S):

- Q7/Q8 (bitemporal compliance replay): **100%** after backend alignment with oracle
- Remaining gaps: 5 probes (Q1/Q2/Q3/Q6×2) + 2 missed Q9 edges — not bitemporal semantics
- Zero false allow / false block on either backend

## 4. Schema evolution (query-surface churn)

Q9 (role-agnostic traversal) is written once against the base ontology and then frozen. The ontology gains a party kind (`trust`) and a 4-ary relation (`control-via-nominee`), and the same unchanged query is replayed.

| Backend | Recall (base) | Recall (extended, frozen query) | Recall (after repair) | Repair LOC |
|---------|---------------|----------------------------------|------------------------|------------|
| postgres | 100.0% | 28.7% | 100.0% | 20 |
| typedb | 100.0% | 100.0% | 100.0% | 0 |

Postgres silently loses `control-via-nominee`: smaller answer, no error raised.

## 5. Bitemporality — interpretation

This benchmark does **not** show that TypeDB is weak at bitemporal management. Early failures (~59% TypeDB pass rate) were primarily **implementation bugs** in the Rust/TypeQL backend:

- Deterministic assertion IDs required for knowledge-closure events to apply
- `beneficial_owners` initially required a direct person→company edge, missing ownership chains
- Incomplete bitemporal lower bounds (`valid_from`, `known_from`) in some TypeQL fetches
- Invalid TypeQL for closing knowledge windows (fixed with `update $r has known-to …`)

After fixes, **Q7 and Q8 pass at 100%** — the engine can represent and query bitemporal compliance state correctly.

Where TypeDB is weaker in this eval:

1. **Ergonomics** — recursive ownership + bitemporal filters are natural as a Postgres recursive CTE with `tstzrange @> $t`; TypeQL required fetch-and-filter in Rust for parity with the oracle
2. **Pushdown** — Postgres keeps interval filtering in SQL (GiST); TypeDB backend often filters client-side
3. **Latency** — ~73× slower avg p50 on scale S, partly due to the above pattern

Conclusion: bitemporal **semantics** work on TypeDB; bitemporal **query ergonomics and performance** favor Postgres in this harness.

## 6. Where TypeDB wins

- Schema evolution: Postgres silently drops to 28.7% recall after an ontology extension and needs 20 LOC of query repair
- Schema evolution: TypeDB keeps 100% recall through the extension with zero query edits (role-agnostic traversal)
- Correctness (scale S): 98.3% vs 94.7% after alignment fixes — slight edge, not decisive alone
- Semantic churn: lower ratio (1.01 vs 1.16)

## 7. Where PostgreSQL wins

- Performance: PostgreSQL ~73× faster on avg p50 (4024µs vs 294558µs)
- Bitemporal query patterns: native `tstzrange` + GiST containment; mature recursive CTE idiom
- Operational maturity: tooling, hiring, on-call familiarity
- Q9 repair cost: 20 LOC vs 0 (though repair restores 100% recall)

## 8. Architectural cost of TypeDB

- New datastore to operate (TypeDB Server 3.12)
- Async Rust driver (`typedb-driver` 3.12)
- Smaller ecosystem than PostgreSQL
- Larger Rust backend (1821 LOC vs 1270) due to client-side graph/bitemporal logic

## 9. Verdict

**INVESTIGATE HYBRID**

TypeDB has a clear advantage on ontology evolution (Q9) and a slight correctness edge after fixes, but remains ~73× slower on scale S. PostgreSQL remains the default for performant bitemporal queries and operations. Investigate a targeted hybrid: Postgres for the bitemporal hot path + TypeDB for polymorphic ontology surfaces if schema evolution is frequent.

---
*Updated after scale S run, seed 42. See results/summary.csv for full metrics.*
