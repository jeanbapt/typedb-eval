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

## Pourquoi TypeDB méritait l'épreuve

Qu'est-ce qui justifierait de renoncer à PostgreSQL — trente ans de maturité, d'outillage, et d'exploitants capables de le réparer à trois heures du matin ? Une seule chose : une inadéquation structurelle entre le modèle de données et le problème posé. TypeDB rendait cette hypothèse plausible.

Le modèle PERA (*Polymorphic Entity-Relation-Attribute*) déplace une frontière. Dans le monde relationnel, une relation se réduit à une clé étrangère : un artefact de stockage que l'application doit réinterpréter à chaque lecture. Dans TypeDB, elle constitue un type de premier rang, doté de rôles nommés, d'une arité quelconque, et susceptible de participer elle-même à d'autres relations. Le schéma cesse d'être un plan de rangement pour devenir une ontologie.

Quatre propriétés, en particulier, épousaient les besoins de SGRS :

- **Rôles polymorphes.** Un rôle `owner` peut être tenu par une personne, une société, un trust ou un fonds souverain — déclaré une fois, vérifié par le schéma. SQL ignore la notion de supertype : on hérite d'une colonne discriminante et d'une clé étrangère qu'il faut abandonner.
- **Traversée agnostique aux rôles.** Interroger « tout ce à quoi cette entité participe » tient en une ligne de TypeQL. La même question exige en SQL une `UNION` sur chaque table concernée, réécrite à chaque enrichissement de l'ontologie.
- **Récursion déclarée dans le schéma.** Les fonctions TypeQL portent la logique transitive une fois pour toutes, là où le CTE récursif se recopie de requête en requête.
- **Invariants typés.** Qui peut tenir quel rôle, dans quelle relation : autant de garanties qui relèvent du schéma plutôt que de la discipline applicative.

Pour un substrat dont la vocation est de représenter des états sémantiques gouvernés, l'argument porte. SGRS maintient une ontologie vivante, appelée à s'enrichir sans relâche — nouveaux types de parties, nouvelles formes de contrôle, nouvelles juridictions. Or le coût d'une ontologie se paie à chaque extension, bien davantage qu'à sa rédaction initiale.

Reste que l'élégance conceptuelle constitue un piège classique. Un modèle qui se lit bien peut coûter trois fois la latence, renoncer à la bitemporalité native, et rendre au code applicatif ce qu'il prétendait absorber. Trois questions demandaient donc à être tranchées. La promesse survit-elle à la mesure ? Le confort de modélisation se paie-t-il en performance ? Et l'avantage subsiste-t-il face à un PostgreSQL correctement modélisé, plutôt que naïvement ?

D'où ce dépôt. L'expérience d'évolution de schéma (Q9, voir plus bas) opérationnalise directement la deuxième promesse : une requête écrite contre l'ontologie de base, gelée, puis rejouée après extension. Ce que chaque backend cesse de voir se mesure alors en rappel perdu et en lignes de requête à réécrire.

Une technologie séduisante exige la mesure avant l'adoption.

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
| `results/DECISION.md` | Verdict final (`KEEP POSTGRES` / `INVESTIGATE HYBRID` / `PURSUE TYPEDB` / `INCONCLUSIVE`) |

**Domaine métier du benchmark** : scénario simplifié KYB / sanctions / beneficial ownership. Ce n'est pas une implémentation réglementaire — c'est un générateur de problèmes structurels représentatifs (ownership indirect, identité incertaine, sources contradictoires, corrections rétroactives, etc.).

**Hors scope** : UI, API publique, GraphQL, LLM, embeddings, diffusion sheaf / Laplacian / H⁰ (reste dans `sgrs-core`), moteur réglementaire.

**Étude future** : persistance et requêtage de stalks et restriction maps en base (Q10–Q12) — évaluer si TypeDB apporte un avantage structurel sur un Postgres fort pour représenter l'état cross-context avant d'investir dans une deuxième couche de stockage.

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

# Régénérer summary.csv et DECISION.md depuis results/raw/ sans relancer le benchmark
cargo run -p benchmark-runner -- report --out results/

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
| Q9 | À quoi cette entité participe-t-elle, quel que soit le type de relation ou le rôle tenu ? |

---

## Évolution de schéma (Q9)

Q9 mesure la propriété la plus difficile à obtenir en SQL : interroger les relations d'une entité sans énumérer les types de relations.

Le protocole tient en trois temps. Chaque backend implémente Q9 une fois, contre l'ontologie de base. L'ontologie gagne ensuite un type de partie (`trust`) et une relation 4-aire à rôle polymorphe (`control-via-nominee`, dont le `controller` peut être une personne, une société ou un trust). La requête initiale, inchangée, est enfin rejouée.

Les deux backends paient un coût de schéma. Un seul paie un coût de requête.

```bash
cargo run -p benchmark-runner -- schema-evolution --backend both --scale S --seed 42 --out results/
```

Deux mesures en sortent, dans `results/SCHEMA_EVOLUTION.md` :

- **rappel après extension** — la fraction des relations réelles que la requête gelée retourne encore. En deçà de 100 %, le backend sous-déclare silencieusement après une évolution d'ontologie, ce qui relève de la correction plutôt que de la performance ;
- **LOC de réparation** — le volume de requête à écrire pour revenir à 100 %.

---

## Scales et critères d'arrêt

| Scale | Événements | Usage |
|-------|------------|-------|
| **S** | 1 000 | Validation rapide, CI |
| **M** | 20 000 | Run décisionnel principal |
| **L** | 200 000 | Conditionnel — seulement si M montre un signal ≥2× |

Après M, si aucun avantage TypeDB significatif n'est détecté, le benchmark s'arrête et produit `results/DECISION.md` avec verdict **KEEP POSTGRES**. C'est un résultat valide et attendu.

Un verdict comparatif exige que les deux backends aient terminé. Si un seul a tourné, le document porte la mention **INCONCLUSIVE** plutôt qu'une conclusion tirée d'une moitié de mesure.

---

## Métriques collectées

- **Performance** : p50 / p95 / p99, throughput, round-trips par requête (ingest séparé)
- **Correctness** : comparaison systématique vs oracle (false allow/block/review, false merge/split, missed contradiction)
- **Semantic churn** : `physical_mutations / semantic_changes`
- **Complexité** : LOC schéma, LOC requêtes, LOC backend Rust, nombre d'objets DB
- **Évolution de schéma** : rappel conservé par une requête gelée après extension de l'ontologie, LOC de réparation
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

Voir [results/DECISION.md](results/DECISION.md) après exécution du benchmark. Le document couvre :

1. Hypothèse testée
2. Périmètre
3. Résultats
4. Évolution de schéma (churn de la surface de requête)
5. Où TypeDB gagne / où PostgreSQL gagne
6. Coût architectural de TypeDB
7. Verdict explicite

---

## Licence

MIT — voir [LICENSE](LICENSE).

Copyright (c) 2026 Deal ex Machina SAS.

## Liens

- [TypeDB](https://typedb.com/) — base de graphe typée (PERA model)
- **SGRS** — produit cible (algèbre de lattice pour états sémantiques gouvernés)
