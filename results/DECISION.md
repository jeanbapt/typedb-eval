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
| postgres | S | 1501 | 1.00 | 88.89% | 129 | 1036 | 1889 |
| typedb | S | 4065 | 1.00 | 83.11% | 135 | 1257 | 7681 |
| postgres | M | 38530 | 1.00 | 88.22% | 129 | 1036 | 9684 |
| typedb | M | 178914 | 1.00 | 88.44% | 135 | 1257 | 27161 |

## 4. Schema evolution (query-surface churn)

Q9 (traversée agnostique aux rôles) est écrite une fois contre l'ontologie de base puis gelée. L'ontologie gagne ensuite un type de partie (`trust`) et une relation 4-aire (`control-via-nominee`), et la même requête inchangée est rejouée.

| Backend | Rappel (base) | Rappel (étendu, requête gelée) | Rappel (après réparation) | LOC de réparation |
|---------|---------------|--------------------------------|---------------------------|-------------------|
| postgres | 100.0% | 28.7% | 100.0% | 20 |
| typedb | 100.0% | 100.0% | 100.0% | 0 |

`postgres` perd `control-via-nominee` silencieusement : réponse plus petite, aucune erreur levée.


## 5. Where TypeDB wins

- Schema evolution: postgres silently drops to 28.7% recall after an ontology extension and needs 20 LOC of query repair
- Schema evolution: TypeDB keeps 100% recall through the extension with zero query edits (role-agnostic traversal)

## 6. Where PostgreSQL wins

- Performance: no significant TypeDB advantage (<3x threshold)
- Correctness: comparable (PG 88.2%, TDB 88.4%) — no structural advantage
- Semantic churn: comparable (PG 1.00, TDB 1.00)
- Maturity: PostgreSQL schema 129 LOC, proven GiST/recursive CTE patterns

## 7. Architectural cost of TypeDB

- Nouveau datastore à opérer (TypeDB Server 3.12)
- Driver async Rust (`typedb-driver` 3.12)
- Écosystème plus restreint que PostgreSQL
- Complexité schema TypeQL: 135 LOC vs PostgreSQL: 129 LOC

## 8. Verdict

**INVESTIGATE HYBRID**

TypeDB apporte un avantage net sur 2 dimension(s) mais insuffisant pour remplacer PostgreSQL. Investiguer un modèle hybride ciblé.

---
*Généré automatiquement par le benchmark runner. Seed: 42. Voir results/summary.csv pour les métriques complètes.*
