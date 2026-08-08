# typedb-eval

Benchmark décisionnel **TypeDB vs PostgreSQL** pour évaluer si une base de graphe typée peut servir de **représentation opérationnelle** au produit **SGRS** — un moteur de représentation et de gouvernance d'états sémantiques fondé sur une **algèbre de lattice**.

Ce dépôt ne cherche pas à prouver que « TypeDB est meilleur ». Il cherche à répondre à une question d'architecture concrète : **PostgreSQL bien modélisé suffit-il, ou TypeDB réduit-il suffisamment le coût computationnel et conceptuel de notre algèbre sémantique pour changer le choix de persistance ?**

---

## Contexte : pourquoi ce benchmark existe

SGRS modélise des états complexes avec notamment :

- relations fortement typées et n-aires ;
- contradictions explicites (evidence lattice : `UNKNOWN`, `SUPPORTED`, `REFUTED`, `CONTRADICTORY`) ;
- identité et résolution d'entités ;
- provenance et gouvernance ;
- bitemporalité (valid time + knowledge time) ;
- contextes locaux (corporate registry, KYC, sanctions) et leur compatibilité ;
- mesure du *semantic churn* (mutations physiques inutiles).

Avant de construire toute la stack SGRS, nous devons savoir si **TypeDB** — avec son modèle PERA (polymorphisme, rôles, relations n-aires natives) — apporte un avantage **structurel** par rapport à **PostgreSQL** correctement optimisé (schéma relationnel typé, `tstzrange`, GiST, recursive CTE).

Un gain marginal (15–30 %) ne justifie pas une nouvelle technologie de persistance. Seuls comptent des signaux forts : ≥3× performance sur des requêtes critiques, ≥3× réduction de complexité d'implémentation, ou invariants typés impossibles à garantir proprement en SQL.

---

## Ce que contient ce dépôt

Un prototype minimal, volontairement resserré :

| Composant | Rôle |
|-----------|------|
| `crates/core` | Types domaine, lattice, trait `ComplianceStore`, oracle de vérité |
| `crates/fixtures` | Générateur déterministe (seed fixe) + ground truth |
| `crates/postgres` | Schéma SQL bitemporel + implémentation PostgreSQL 16 |
| `crates/typedb` | Schéma TypeQL + implémentation TypeDB 3.12 |
| `crates/runner` | CLI benchmark, métriques, ablation, export |
| `results/` | Mesures brutes + `summary.csv` |
| `DECISION.md` | Verdict final (`KEEP POSTGRES` / `INVESTIGATE HYBRID` / `PURSUE TYPEDB`) |

**Domaine métier du benchmark** : scénario simplifié KYB / sanctions / beneficial ownership. Ce n'est pas une implémentation réglementaire — c'est un générateur de problèmes structurels représentatifs (ownership indirect, identité incertaine, sources contradictoires, corrections rétroactives, etc.).

**Hors scope** : UI, API publique, GraphQL, LLM, embeddings, framework sheaf générique, moteur réglementaire.

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

Les deux backends implémentent la même interface `ComplianceStore`. La logique métier n'est pas volontairement déplacée vers l'un ou l'autre pour favoriser un candidat — chaque backend utilise ses primitives naturelles lorsqu'elles remplacent raisonnablement du code applicatif.

---

## Prérequis

- [Rust](https://rustup.rs/) 1.75+
- [Docker](https://docs.docker.com/get-docker/) & Docker Compose

## Démarrage rapide

```bash
# Lancer PostgreSQL 16 et TypeDB 3.12
docker compose up -d

# Attendre que les services soient prêts, puis :
cargo run -p benchmark-runner -- --backend both --scale S --seed 42 --out results/
```

Pour un run complet (scale M, ~20k événements) :

```bash
cargo run -p benchmark-runner -- \
  --backend both \
  --scale M \
  --seed 42 \
  --out results/
```

## Commandes utiles

```bash
# Aide
cargo run -p benchmark-runner -- --help

# Générer les fixtures seulement
cargo run -p benchmark-runner -- generate --scale M --seed 42 --out fixtures/out/

# Ablation d'une dimension sémantique
cargo run -p benchmark-runner -- \
  --backend both \
  --scale M \
  --seed 42 \
  --ablate identity \
  --out results/

# Ablation complète (toutes les dimensions)
cargo run -p benchmark-runner -- ablation --backend both --scale M --seed 42 --out results/

# Tests unitaires
cargo test
```

---

## Requêtes benchmarkées (Q1–Q8)

| ID | Question |
|----|----------|
| Q1 | Quel est le beneficial owner connu actuellement ? |
| Q2 | Que pensions-nous à une date T concernant une situation valable à une date V ? (bitemporal) |
| Q3 | Quelles assertions actives se contredisent pour cette entité ? |
| Q4 | Deux identités (ex. John Smith / Jonathan Smith) doivent-elles être fusionnées ? |
| Q5 | Une personne sanctionnée contrôle-t-elle directement ou indirectement cette société ? |
| Q6 | Les contextes Corporate Registry / KYC / Sanctions sont-ils compatibles ? |
| Q7 | Avec les infos connues à une date, la décision aurait-elle été ALLOW / REVIEW / BLOCK ? |
| Q8 | Avec les infos d'aujourd'hui, comment qualifier la même situation passée ? (Q7 ≠ Q8) |

---

## Scales et critères d'arrêt

| Scale | Événements | Usage |
|-------|------------|-------|
| **S** | 1 000 | Validation rapide, CI |
| **M** | 20 000 | Run décisionnel principal |
| **L** | 200 000 | Conditionnel — seulement si M montre un signal ≥2× |

Après M, si aucun avantage TypeDB significatif n'est détecté, le benchmark s'arrête et produit `DECISION.md` avec verdict **KEEP POSTGRES**. C'est un résultat valide et attendu.

---

## Métriques collectées

- **Performance** : p50 / p95 / p99, throughput, round-trips par requête (ingest séparé)
- **Correctness** : comparaison systématique vs oracle (false allow/block/review, false merge/split, missed contradiction)
- **Semantic churn** : `physical_mutations / semantic_changes`
- **Complexité** : LOC schéma, LOC requêtes, LOC backend Rust, nombre d'objets DB
- **Ablation** : impact de chaque dimension sémantique (identity, evidence, valid_time, knowledge_time, jurisdiction, role, source, provenance, governance…)

Résultats dans `results/summary.csv` et `results/raw/*.json`.

---

## Benchmark agent (retrieval via MCP)

En plus du benchmark direct (drivers Rust), ce dépôt mesure **séparément** la retrieval telle qu'un agent LLM la ferait via les serveurs MCP — le chemin réaliste pour SGRS quand un agent interroge la base en langage naturel.

| Serveur | URL | Rôle |
|---------|-----|------|
| **postgres-mcp** | `http://localhost:8899/mcp` | `list_tables`, `describe_table`, `execute_sql` |
| **typedb-mcp** | `http://localhost:8001/mcp` | `query`, gestion bases TypeDB |

```bash
# Démarrer Postgres, TypeDB et les deux serveurs MCP
docker compose up -d

# Benchmark retrieval agent (précharge les données, puis teste via MCP)
cargo run -p benchmark-runner -- agent-retrieval --scale S --seed 42 --out results/
```

Le workflow simule un agent :
1. **Introspection schéma** via MCP (`list_tables` / `describe_table` ou probe TypeQL)
2. **Prompt NL** (ex. « Who are the beneficial owners? »)
3. **Exécution** de la requête générée via `execute_sql` ou `query`
4. Mesures : latence p50/p95, round-trips MCP, taux de succès

Résultats dans `results/agent_retrieval_summary.csv`.

Pour Cursor, copier [`.cursor/mcp.json.example`](.cursor/mcp.json.example) vers `.cursor/mcp.json`.

---

## Verdict

Voir [DECISION.md](DECISION.md) après exécution du benchmark. Le document couvre :

1. Hypothèse testée
2. Périmètre
3. Résultats
4. Où TypeDB gagne / où PostgreSQL gagne
5. Coût architectural de TypeDB
6. Verdict explicite

---

## Licence

MIT — voir le dépôt pour les détails.

## Liens

- [TypeDB](https://typedb.com/) — base de graphe typée (PERA model)
- **SGRS** — produit cible (algèbre de lattice pour états sémantiques gouvernés)
