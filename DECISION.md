# DECISION — TypeDB vs PostgreSQL

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
| postgres | S | 3864 | 1.00 | 88.89% | 129 | 1036 | 3128 |
| typedb | S | 9978 | 1.00 | 83.11% | 133 | 1257 | 13889 |

## 4. Where TypeDB wins

- Aucun avantage structurel identifié

## 5. Where PostgreSQL wins

- Performance: PostgreSQL ~4.4x faster on avg p50 (3128µs vs 13889µs)
- Correctness: PostgreSQL pass rate 88.9% vs TypeDB 83.1%
- Semantic churn: comparable (PG 1.00, TDB 1.00)
- Maturity: PostgreSQL schema 129 LOC, proven GiST/recursive CTE patterns

## 6. Architectural cost of TypeDB

- Nouveau datastore à opérer (TypeDB Server 3.12)
- Driver async Rust (`typedb-driver` 3.12)
- Écosystème plus restreint que PostgreSQL
- Complexité schema TypeQL: 133 LOC vs PostgreSQL: 129 LOC

## 7. Verdict

**KEEP POSTGRES**

Après tests M, aucune opération importante ne dépasse ~2x de gain TypeDB. PostgreSQL 4 avantage(s) identifié(s). Le coût technologique de TypeDB n'est pas justifié pour ce substrat sémantique gouverné.

---
*Généré automatiquement par le benchmark runner. Seed: 42. Voir results/summary.csv pour les métriques complètes.*
