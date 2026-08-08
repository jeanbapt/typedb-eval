-- PostgreSQL schema for semantic compliance benchmark
-- Bitemporal model with typed relational tables (not triple store)

CREATE EXTENSION IF NOT EXISTS btree_gist;

-- Sources
CREATE TABLE IF NOT EXISTS source (
    id          TEXT PRIMARY KEY,
    authority   REAL NOT NULL DEFAULT 0.5
);

-- Jurisdictions
CREATE TABLE IF NOT EXISTS jurisdiction (
    code TEXT PRIMARY KEY
);

-- Persons
CREATE TABLE IF NOT EXISTS person (
    id              UUID PRIMARY KEY,
    name            TEXT NOT NULL,
    canonical_name  TEXT NOT NULL,
    jurisdiction    TEXT NOT NULL REFERENCES jurisdiction(code)
);

CREATE INDEX IF NOT EXISTS idx_person_canonical ON person (canonical_name);

-- Companies
CREATE TABLE IF NOT EXISTS company (
    id           UUID PRIMARY KEY,
    name         TEXT NOT NULL,
    jurisdiction TEXT NOT NULL REFERENCES jurisdiction(code)
);

-- Identity aliases (for merge/split discrimination)
CREATE TABLE IF NOT EXISTS identity_alias (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id    UUID NOT NULL REFERENCES person(id),
    alias        TEXT NOT NULL,
    canonical    TEXT NOT NULL,
    merge        BOOLEAN NOT NULL DEFAULT false,
    context      TEXT NOT NULL,
    observed_at  TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_identity_alias_person ON identity_alias (person_id);

-- Ownership assertions with bitemporal columns
CREATE TABLE IF NOT EXISTS ownership_assertion (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id         UUID NOT NULL REFERENCES person(id),
    owned_id         UUID NOT NULL REFERENCES company(id),
    share_pct        REAL NOT NULL,
    evidence         TEXT NOT NULL,
    governance       TEXT NOT NULL,
    context          TEXT NOT NULL,
    role             TEXT,
    jurisdiction     TEXT NOT NULL,
    source_id        TEXT NOT NULL REFERENCES source(id),
    source_authority REAL NOT NULL,
    observed_at      TIMESTAMPTZ NOT NULL,
    valid_range      TSTZRANGE NOT NULL,
    known_range      TSTZRANGE NOT NULL,
    predicate        TEXT NOT NULL DEFAULT 'owns'
);

CREATE INDEX IF NOT EXISTS idx_ownership_owner ON ownership_assertion (owner_id);
CREATE INDEX IF NOT EXISTS idx_ownership_owned ON ownership_assertion (owned_id);
CREATE INDEX IF NOT EXISTS idx_ownership_valid ON ownership_assertion USING GIST (valid_range);
CREATE INDEX IF NOT EXISTS idx_ownership_known ON ownership_assertion USING GIST (known_range);

-- Generic assertions (contradictions, late arrivals)
CREATE TABLE IF NOT EXISTS assertion (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id       UUID NOT NULL,
    predicate        TEXT NOT NULL,
    object_id        UUID NOT NULL,
    evidence         TEXT NOT NULL,
    governance       TEXT NOT NULL DEFAULT 'OBSERVED',
    context          TEXT NOT NULL,
    role             TEXT,
    jurisdiction     TEXT NOT NULL DEFAULT 'GLOBAL',
    source_id        TEXT NOT NULL,
    source_authority REAL NOT NULL DEFAULT 0.5,
    observed_at      TIMESTAMPTZ NOT NULL,
    valid_range      TSTZRANGE NOT NULL,
    known_range      TSTZRANGE NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_assertion_subject ON assertion (subject_id);
CREATE INDEX IF NOT EXISTS idx_assertion_object ON assertion (object_id);
CREATE INDEX IF NOT EXISTS idx_assertion_valid ON assertion USING GIST (valid_range);
CREATE INDEX IF NOT EXISTS idx_assertion_known ON assertion USING GIST (known_range);

-- Sanction listings
CREATE TABLE IF NOT EXISTS sanction_listing (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    person_id   UUID NOT NULL REFERENCES person(id),
    list_name   TEXT NOT NULL,
    listed      BOOLEAN NOT NULL,
    context     TEXT NOT NULL,
    valid_range TSTZRANGE NOT NULL,
    known_range TSTZRANGE NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sanction_person ON sanction_listing (person_id);
CREATE INDEX IF NOT EXISTS idx_sanction_valid ON sanction_listing USING GIST (valid_range);

-- Compliance rules
CREATE TABLE IF NOT EXISTS compliance_rule (
    rule_id       TEXT PRIMARY KEY,
    description   TEXT NOT NULL,
    threshold_pct REAL NOT NULL
);

-- === Ontology extension ===
-- New party kind. Note that person/company/trust remain three unrelated tables: SQL has
-- no supertype, so "a party" exists only as a convention enforced in application code.
CREATE TABLE IF NOT EXISTS trust (
    id           UUID PRIMARY KEY,
    name         TEXT NOT NULL,
    jurisdiction TEXT NOT NULL REFERENCES jurisdiction(code)
);

-- 4-ary relation. `controller_id` is polymorphic (person | company | trust) and therefore
-- cannot carry a foreign key; `controller_kind` is the discriminator that a typed engine
-- would not need.
CREATE TABLE IF NOT EXISTS control_via_nominee (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    controller_id    UUID NOT NULL,
    controller_kind  TEXT NOT NULL,
    controlled_id    UUID NOT NULL REFERENCES company(id),
    nominee_id       UUID NOT NULL REFERENCES person(id),
    instrument_id    UUID NOT NULL REFERENCES trust(id),
    context          TEXT NOT NULL,
    jurisdiction     TEXT NOT NULL,
    source_id        TEXT NOT NULL,
    source_authority REAL NOT NULL DEFAULT 0.5,
    observed_at      TIMESTAMPTZ NOT NULL,
    valid_range      TSTZRANGE NOT NULL,
    known_range      TSTZRANGE NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_cvn_controller ON control_via_nominee (controller_id);
CREATE INDEX IF NOT EXISTS idx_cvn_controlled ON control_via_nominee (controlled_id);
CREATE INDEX IF NOT EXISTS idx_cvn_nominee ON control_via_nominee (nominee_id);
CREATE INDEX IF NOT EXISTS idx_cvn_instrument ON control_via_nominee (instrument_id);
CREATE INDEX IF NOT EXISTS idx_cvn_valid ON control_via_nominee USING GIST (valid_range);
CREATE INDEX IF NOT EXISTS idx_cvn_known ON control_via_nominee USING GIST (known_range);

-- Churn tracking
CREATE TABLE IF NOT EXISTS churn_log (
    id                  BIGSERIAL PRIMARY KEY,
    event_type          TEXT NOT NULL,
    physical_mutations  INT NOT NULL DEFAULT 1,
    semantic_changes    INT NOT NULL DEFAULT 0,
    recorded_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Seed jurisdictions and sources
INSERT INTO jurisdiction (code) VALUES ('US'), ('UK'), ('EU'), ('SG'), ('GLOBAL')
ON CONFLICT DO NOTHING;

INSERT INTO source (id, authority) VALUES
    ('registrar', 0.95),
    ('kyc_vendor', 0.7),
    ('sanctions_db', 0.99),
    ('news_feed', 0.4),
    ('manual_review', 0.8)
ON CONFLICT DO NOTHING;
