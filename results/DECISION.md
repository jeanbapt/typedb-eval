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
| postgres | S | 430 | 1.16 | 94.72% | 149 | 1270 | 1097 |
| typedb | S | 2612 | 1.01 | 98.61% | 213 | 1716 | 6520 |
| postgres | M | 24309 | 1.17 | 86.44% | 151 | 1440 | 46033 |
| typedb | M | 68391 | 1.00 | 98.22% | 213 | 1716 | 132697 |

## 4. Schema evolution (query-surface churn)

Q9 (traversée agnostique aux rôles) est écrite une fois contre l'ontologie de base puis gelée. L'ontologie gagne ensuite un type de partie (`trust`) et une relation 4-aire (`control-via-nominee`), et la même requête inchangée est rejouée.

| Backend | Rappel (base) | Rappel (étendu, requête gelée) | Rappel (après réparation) | LOC de réparation |
|---------|---------------|--------------------------------|---------------------------|-------------------|
| postgres | 100.0% | 100.0% | 100.0% | 0 |
| typedb | 100.0% | 100.0% | 100.0% | 0 |


## 5. Where TypeDB wins

- Correctness: TypeDB pass rate 98.2% vs PostgreSQL 86.4%

## 6. Where PostgreSQL wins

- Performance: no significant TypeDB advantage (<3x threshold)
- Semantic churn: comparable (PG 1.17, TDB 1.00)
- Maturity: PostgreSQL schema 151 LOC, proven GiST/recursive CTE patterns

## 7. Architectural cost of TypeDB

- Nouveau datastore à opérer (TypeDB Server 3.12)
- Driver async Rust (`typedb-driver` 3.12)
- Écosystème plus restreint que PostgreSQL
- Complexité schema TypeQL: 213 LOC vs PostgreSQL: 151 LOC

## 8. Verdict

**KEEP POSTGRES**

Après tests M, aucune opération importante ne dépasse ~2x de gain TypeDB. PostgreSQL 3 avantage(s) identifié(s). Le coût technologique de TypeDB n'est pas justifié pour ce substrat sémantique gouverné.

---
*Généré automatiquement par le benchmark runner. Seed: 42. Voir results/summary.csv pour les métriques complètes.*
