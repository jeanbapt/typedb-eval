# DECISION — TypeDB vs PostgreSQL

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
| postgres | S | 2332 | 1.16 | 94.72% | 149 | 1270 | 4024 |
| typedb | S | 6363 | 1.01 | 98.33% | 138 | 1821 | 294558 |

Correctness breakdown (TypeDB, scale S):

- Q7/Q8 (bitemporal compliance replay): **100%** after backend alignment with oracle
- Remaining gaps: 5 probes (Q1/Q2/Q3/Q6×2) + 2 missed Q9 edges — not bitemporal semantics
- Zero false allow / false block on either backend

## 4. Schema evolution (query-surface churn)

Q9 (traversée agnostique aux rôles) est écrite une fois contre l'ontologie de base puis gelée. L'ontologie gagne ensuite un type de partie (`trust`) et une relation 4-aire (`control-via-nominee`), et la même requête inchangée est rejouée.

| Backend | Rappel (base) | Rappel (étendu, requête gelée) | Rappel (après réparation) | LOC de réparation |
|---------|---------------|--------------------------------|---------------------------|-------------------|
| postgres | 100.0% | 28.7% | 100.0% | 20 |
| typedb | 100.0% | 100.0% | 100.0% | 0 |

`postgres` perd `control-via-nominee` silencieusement : réponse plus petite, aucune erreur levée.

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

- Schema evolution: postgres silently drops to 28.7% recall after an ontology extension and needs 20 LOC of query repair
- Schema evolution: TypeDB keeps 100% recall through the extension with zero query edits (role-agnostic traversal)
- Correctness (scale S): 98.3% vs 94.7% after alignment fixes — slight edge, not decisive alone
- Semantic churn: lower ratio (1.01 vs 1.16)

## 7. Where PostgreSQL wins

- Performance: PostgreSQL ~73× faster on avg p50 (4024µs vs 294558µs)
- Bitemporal query patterns: native `tstzrange` + GiST containment; mature recursive CTE idiom
- Operational maturity: tooling, hiring, on-call familiarity
- Q9 repair cost: 20 LOC vs 0 (though repair restores 100% recall)

## 8. Architectural cost of TypeDB

- Nouveau datastore à opérer (TypeDB Server 3.12)
- Driver async Rust (`typedb-driver` 3.12)
- Écosystème plus restreint que PostgreSQL
- Backend Rust plus volumineux (1821 LOC vs 1270) due to client-side graph/bitemporal logic

## 9. Verdict

**INVESTIGATE HYBRID**

TypeDB apporte un avantage net sur l'évolution d'ontologie (Q9) et une légère edge correctness après corrections, mais reste ~73× plus lent sur scale S. PostgreSQL reste le choix par défaut pour la bitemporalité performante et l'exploitation. Investiguer un modèle hybride ciblé : Postgres pour le hot path bitemporal + TypeDB pour les surfaces ontologiques polymorphes si l'évolution de schéma est fréquente.

---
*Généré et enrichi manuellement après run scale S seed 42. Voir results/summary.csv pour les métriques complètes.*
