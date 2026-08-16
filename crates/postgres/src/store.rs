use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use benchmark_core::error::Result;
use benchmark_core::{
    AblationDimension, AssertionId, Bitemporal, Compatibility, ComplianceStore,
    Conflict, Context, Decision, EntityId, EntityState, Event, EvidenceState, Exposure,
    GovernanceLevel, IdentityAction, NeighborEdge, Neighborhood, PartyId, PersonId, Role,
    StateDelta, Timestamp,
};

pub struct PostgresStore {
    pool: PgPool,
    ablation: AblationDimension,
    churn: StateDelta,
}

impl PostgresStore {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(Self {
            pool,
            ablation: AblationDimension::None,
            churn: StateDelta::default(),
        })
    }

    pub async fn connect_with_ablation(
        database_url: &str,
        ablation: AblationDimension,
    ) -> Result<Self> {
        let mut store = Self::connect(database_url).await?;
        store.ablation = ablation;
        Ok(store)
    }

    pub async fn migrate(&self) -> Result<()> {
        let schema = include_str!("../schema.sql");
        sqlx::raw_sql(schema)
            .execute(&self.pool)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn churn(&self) -> &StateDelta {
        &self.churn
    }

    /// Frozen Q9: role-agnostic traversal via the participation index (no per-relation UNION).
    const NEIGHBORHOOD_SQL: &'static str = r#"
        SELECT rel, role, other_id AS other
          FROM entity_participation
         WHERE entity_id = $1
           AND valid_range @> $2::timestamptz
           AND known_range @> $3::timestamptz
    "#;

    fn tsrange(from: Timestamp, to: Option<Timestamp>) -> String {
        let from_str = from.to_rfc3339();
        match to {
            Some(t) => format!("[\"{from_str}\",\"{}\")", t.to_rfc3339()),
            None => format!("[\"{from_str}\",)"),
        }
    }

    fn evidence_str(e: EvidenceState) -> &'static str {
        match e {
            EvidenceState::Unknown => "UNKNOWN",
            EvidenceState::Supported => "SUPPORTED",
            EvidenceState::Refuted => "REFUTED",
            EvidenceState::Contradictory => "CONTRADICTORY",
        }
    }

    fn governance_str(g: GovernanceLevel) -> &'static str {
        match g {
            GovernanceLevel::Observed => "OBSERVED",
            GovernanceLevel::Corroborated => "CORROBORATED",
            GovernanceLevel::Reviewed => "REVIEWED",
            GovernanceLevel::Final => "FINAL",
        }
    }

    fn context_str(c: Context) -> &'static str {
        match c {
            Context::CorporateRegistry => "CORPORATE_REGISTRY",
            Context::Kyc => "KYC",
            Context::Sanctions => "SANCTIONS",
            Context::Regulatory => "REGULATORY",
        }
    }

    fn role_str(r: Role) -> &'static str {
        match r {
            Role::BeneficialOwner => "BENEFICIAL_OWNER",
            Role::Director => "DIRECTOR",
            Role::Shareholder => "SHAREHOLDER",
            Role::Controller => "CONTROLLER",
        }
    }

    fn parse_evidence(s: &str) -> EvidenceState {
        match s {
            "SUPPORTED" => EvidenceState::Supported,
            "REFUTED" => EvidenceState::Refuted,
            "CONTRADICTORY" => EvidenceState::Contradictory,
            _ => EvidenceState::Unknown,
        }
    }

    fn parse_governance(s: &str) -> GovernanceLevel {
        match s {
            "CORROBORATED" => GovernanceLevel::Corroborated,
            "REVIEWED" => GovernanceLevel::Reviewed,
            "FINAL" => GovernanceLevel::Final,
            _ => GovernanceLevel::Observed,
        }
    }

    fn parse_context(s: &str) -> Context {
        match s {
            "KYC" => Context::Kyc,
            "SANCTIONS" => Context::Sanctions,
            _ => Context::CorporateRegistry,
        }
    }

    async fn ensure_source(&self, source_id: &str, authority: f32) -> Result<()> {
        sqlx::query(
            "INSERT INTO source (id, authority) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(source_id)
        .bind(authority)
        .execute(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(())
    }

    async fn insert_participation(
        &self,
        entity_id: Uuid,
        rel: &str,
        role: &str,
        other_id: Option<Uuid>,
        valid_range: &str,
        known_range: &str,
        source_id: Uuid,
        source_table: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO entity_participation
               (entity_id, rel, role, other_id, valid_range, known_range, source_id, source_table)
               VALUES ($1, $2, $3, $4, $5::tstzrange, $6::tstzrange, $7, $8)"#,
        )
        .bind(entity_id)
        .bind(rel)
        .bind(role)
        .bind(other_id)
        .bind(valid_range)
        .bind(known_range)
        .bind(source_id)
        .bind(source_table)
        .execute(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(())
    }

    async fn record_ownership_participation(
        &self,
        owner_id: Uuid,
        owned_id: Uuid,
        source_id: Uuid,
        valid_range: &str,
        known_range: &str,
    ) -> Result<()> {
        self.insert_participation(
            owner_id,
            "ownership",
            "owner",
            Some(owned_id),
            valid_range,
            known_range,
            source_id,
            "ownership_assertion",
        )
        .await?;
        self.insert_participation(
            owned_id,
            "ownership",
            "owned",
            Some(owner_id),
            valid_range,
            known_range,
            source_id,
            "ownership_assertion",
        )
        .await?;
        Ok(())
    }

    async fn record_assertion_participation(
        &self,
        subject_id: Uuid,
        object_id: Uuid,
        predicate: &str,
        source_id: Uuid,
        valid_range: &str,
        known_range: &str,
    ) -> Result<()> {
        let (rel, subject_role, object_role) = if predicate.starts_with("owns_") {
            ("ownership", "owner", "owned")
        } else {
            ("generic-assertion", "subject", "object")
        };
        self.insert_participation(
            subject_id,
            rel,
            subject_role,
            Some(object_id),
            valid_range,
            known_range,
            source_id,
            "assertion",
        )
        .await?;
        self.insert_participation(
            object_id,
            rel,
            object_role,
            Some(subject_id),
            valid_range,
            known_range,
            source_id,
            "assertion",
        )
        .await?;
        Ok(())
    }

    async fn record_sanction_participation(
        &self,
        person_id: Uuid,
        listing_id: Uuid,
        valid_range: &str,
        known_range: &str,
    ) -> Result<()> {
        self.insert_participation(
            person_id,
            "sanction-listing",
            "sanctioned-person",
            None,
            valid_range,
            known_range,
            listing_id,
            "sanction_listing",
        )
        .await?;
        Ok(())
    }

    async fn record_control_participation(
        &self,
        control_id: Uuid,
        controller: Uuid,
        controlled: Uuid,
        nominee: Uuid,
        instrument: Uuid,
        valid_range: &str,
        known_range: &str,
    ) -> Result<()> {
        let players: [(&str, Uuid); 4] = [
            ("controller", controller),
            ("controlled", controlled),
            ("nominee", nominee),
            ("instrument", instrument),
        ];
        for (role, entity) in players {
            for (other_role, other) in players {
                if other_role != role {
                    self.insert_participation(
                        entity,
                        "control-via-nominee",
                        role,
                        Some(other),
                        valid_range,
                        known_range,
                        control_id,
                        "control_via_nominee",
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn close_participation_knowledge(&self, source_id: Uuid, known_to: Timestamp) -> Result<()> {
        sqlx::query(
            r#"UPDATE entity_participation
               SET known_range = tstzrange(lower(known_range), $1::timestamptz, '[)')
               WHERE source_id = $2"#,
        )
        .bind(known_to)
        .bind(source_id)
        .execute(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(())
    }

    async fn log_churn(&mut self, physical: u64, semantic: u64, event_type: &str) -> Result<()> {
        self.churn.physical_mutations += physical;
        self.churn.semantic_changes += semantic;
        sqlx::query(
            "INSERT INTO churn_log (event_type, physical_mutations, semantic_changes) VALUES ($1, $2, $3)",
        )
        .bind(event_type)
        .bind(physical as i32)
        .bind(semantic as i32)
        .execute(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl ComplianceStore for PostgresStore {
    async fn ingest(&mut self, event: Event) -> Result<StateDelta> {
        self.ingest_async(event).await
    }

    async fn state_at(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<EntityState> {
        self.state_at_async(entity, valid_at, known_at).await
    }

    async fn contradictions(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<Conflict>> {
        self.contradictions_async(entity, valid_at, known_at).await
    }

    async fn ownership_exposure(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Exposure> {
        self.ownership_exposure_async(entity, valid_at, known_at)
            .await
    }

    async fn compliance_decision(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Decision> {
        self.compliance_decision_async(entity, valid_at, known_at)
            .await
    }

    async fn identity_action(
        &self,
        person_a: PersonId,
        person_b: PersonId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<IdentityAction> {
        self.identity_action_async(person_a, person_b, valid_at, known_at)
            .await
    }

    async fn context_compatibility(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Compatibility> {
        self.context_compatibility_async(entity, valid_at, known_at)
            .await
    }

    async fn neighborhood(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Neighborhood> {
        self.neighborhood_async(entity, valid_at, known_at).await
    }

    async fn neighborhood_repaired(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Neighborhood> {
        self.neighborhood_repaired_async(entity, valid_at, known_at)
            .await
    }

    fn query_repair_loc(&self) -> u64 {
        0
    }

    async fn reset(&mut self) -> Result<()> {
        self.reset_async().await
    }
}

impl PostgresStore {
    async fn reset_async(&mut self) -> Result<()> {
        let tables = [
            "churn_log",
            "entity_participation",
            "sanction_listing",
            "assertion",
            "ownership_assertion",
            "identity_alias",
            "control_via_nominee",
            "compliance_rule",
            "person",
            "company",
            "trust",
        ];
        for t in tables {
            sqlx::query(&format!("TRUNCATE TABLE {t} CASCADE"))
                .execute(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        }
        self.churn = StateDelta::default();
        Ok(())
    }

    async fn ingest_async(&mut self, event: Event) -> Result<StateDelta> {
        let mut delta = StateDelta::default();

        match event {
            Event::RegisterPerson {
                id,
                name,
                canonical_name,
                jurisdiction,
                ..
            } => {
                let canonical = if self.ablation == AblationDimension::Identity {
                    name.clone()
                } else {
                    canonical_name
                };
                let result = sqlx::query(
                    "INSERT INTO person (id, name, canonical_name, jurisdiction) VALUES ($1, $2, $3, $4) ON CONFLICT (id) DO NOTHING",
                )
                .bind(id.0)
                .bind(&name)
                .bind(&canonical)
                .bind(&jurisdiction)
                .execute(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                delta.physical_mutations = 1;
                delta.semantic_changes = if result.rows_affected() > 0 { 1 } else { 0 };
            }
            Event::RegisterCompany {
                id,
                name,
                jurisdiction,
                ..
            } => {
                let result = sqlx::query(
                    "INSERT INTO company (id, name, jurisdiction) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
                )
                .bind(id.0)
                .bind(&name)
                .bind(&jurisdiction)
                .execute(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                delta.physical_mutations = 1;
                delta.semantic_changes = if result.rows_affected() > 0 { 1 } else { 0 };
            }
            Event::AssertOwnership {
                owner,
                owned,
                share_pct,
                evidence,
                governance,
                context,
                role,
                jurisdiction,
                provenance,
                bitemporal,
            } => {
                let (valid_range, known_range) = self.apply_ablation_bitemporal(&bitemporal);
                let evidence = if self.ablation == AblationDimension::Evidence {
                    EvidenceState::Unknown
                } else {
                    evidence
                };
                let governance = if self.ablation == AblationDimension::Governance {
                    GovernanceLevel::Observed
                } else {
                    governance
                };
                let jurisdiction = if self.ablation == AblationDimension::Jurisdiction {
                    "GLOBAL".to_string()
                } else {
                    jurisdiction
                };
                let role_str = Some(Self::role_str(role).to_string());
                let source_id = if self.ablation == AblationDimension::Source {
                    "unknown".to_string()
                } else {
                    provenance.source_id.clone()
                };
                let source_authority = if self.ablation == AblationDimension::SourceAuthority {
                    0.5f32
                } else {
                    provenance.source_authority
                };
                self.ensure_source(&source_id, source_authority).await?;
                let predicate = format!("owns_{share_pct}");
                let assertion_id = AssertionId::deterministic(
                    owner.entity(),
                    &predicate,
                    owned.entity(),
                    &bitemporal,
                    0,
                    &format!("{}@{}", source_id, provenance.observed_at.timestamp()),
                );
                sqlx::query(
                    r#"INSERT INTO ownership_assertion
                    (id, owner_id, owner_kind, owned_id, share_pct, evidence, governance, context, role, jurisdiction,
                     source_id, source_authority, observed_at, valid_range, known_range, predicate)
                    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14::tstzrange,$15::tstzrange,$16)"#,
                )
                .bind(assertion_id.0)
                .bind(owner.entity().0)
                .bind(owner.kind_str())
                .bind(owned.0)
                .bind(share_pct)
                .bind(Self::evidence_str(evidence))
                .bind(Self::governance_str(governance))
                .bind(Self::context_str(context))
                .bind(role_str)
                .bind(&jurisdiction)
                .bind(&source_id)
                .bind(source_authority)
                .bind(provenance.observed_at)
                .bind(&valid_range)
                .bind(&known_range)
                .bind(&predicate)
                .execute(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                self.record_ownership_participation(
                    owner.entity().0,
                    owned.0,
                    assertion_id.0,
                    &valid_range,
                    &known_range,
                )
                .await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::IdentityAlias {
                person_a,
                alias,
                canonical,
                merge,
                context,
                at,
            } => {
                sqlx::query(
                    "INSERT INTO identity_alias (person_id, alias, canonical, merge, context, observed_at) VALUES ($1,$2,$3,$4,$5,$6)",
                )
                .bind(person_a.0)
                .bind(&alias)
                .bind(&canonical)
                .bind(merge)
                .bind(Self::context_str(context))
                .bind(at)
                .execute(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::SanctionListing {
                person,
                list_name,
                listed,
                context,
                bitemporal,
            } => {
                let (valid_range, known_range) = self.apply_ablation_bitemporal(&bitemporal);
                let listing_id: Uuid = sqlx::query_scalar(
                    r#"INSERT INTO sanction_listing (person_id, list_name, listed, context, valid_range, known_range)
                       VALUES ($1,$2,$3,$4,$5::tstzrange,$6::tstzrange)
                       RETURNING id"#,
                )
                .bind(person.0)
                .bind(&list_name)
                .bind(listed)
                .bind(Self::context_str(context))
                .bind(&valid_range)
                .bind(&known_range)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                self.record_sanction_participation(
                    person.0,
                    listing_id,
                    &valid_range,
                    &known_range,
                )
                .await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::ContradictorySource {
                subject,
                predicate,
                object,
                supporting,
                refuting,
                context,
                provenance,
                bitemporal,
            } => {
                let (valid_range, known_range) = self.apply_ablation_bitemporal(&bitemporal);
                self.ensure_source(&provenance.source_id, provenance.source_authority).await?;
                for (ev, disc) in [(supporting, 0u32), (refuting, 1u32)] {
                    let assertion_id = AssertionId::deterministic(
                        subject,
                        &predicate,
                        object,
                        &bitemporal,
                        disc,
                        &format!("{}@{}", provenance.source_id, provenance.observed_at.timestamp()),
                    );
                    sqlx::query(
                        r#"INSERT INTO assertion (id, subject_id, predicate, object_id, evidence, context,
                         source_id, source_authority, observed_at, valid_range, known_range, jurisdiction, governance)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::tstzrange,$11::tstzrange,'GLOBAL','OBSERVED')"#,
                    )
                    .bind(assertion_id.0)
                    .bind(subject.0)
                    .bind(&predicate)
                    .bind(object.0)
                    .bind(Self::evidence_str(ev))
                    .bind(Self::context_str(context))
                    .bind(&provenance.source_id)
                    .bind(provenance.source_authority)
                    .bind(provenance.observed_at)
                    .bind(&valid_range)
                    .bind(&known_range)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                    self.record_assertion_participation(
                        subject.0,
                        object.0,
                        &predicate,
                        assertion_id.0,
                        &valid_range,
                        &known_range,
                    )
                    .await?;
                }
                delta.physical_mutations = 2;
                delta.semantic_changes = 1;
            }
            Event::LateArrival {
                subject,
                predicate,
                object,
                evidence,
                context,
                provenance,
                bitemporal,
            } => {
                let (valid_range, known_range) = self.apply_ablation_bitemporal(&bitemporal);
                self.ensure_source(&provenance.source_id, provenance.source_authority).await?;
                let assertion_id = AssertionId::deterministic(
                    subject,
                    &predicate,
                    object,
                    &bitemporal,
                    1,
                    &format!("{}@{}", provenance.source_id, provenance.observed_at.timestamp()),
                );
                if predicate.starts_with("owns_") {
                    let share_pct: f32 = predicate
                        .strip_prefix("owns_")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0.0);
                    sqlx::query(
                        r#"INSERT INTO ownership_assertion
                        (id, owner_id, owner_kind, owned_id, share_pct, evidence, governance, context, role, jurisdiction,
                         source_id, source_authority, observed_at, valid_range, known_range, predicate)
                        VALUES ($1,$2,'person',$3,$4,$5,'OBSERVED',$6,'SHAREHOLDER','GLOBAL',$7,$8,$9,$10::tstzrange,$11::tstzrange,$12)"#,
                    )
                    .bind(assertion_id.0)
                    .bind(subject.0)
                    .bind(object.0)
                    .bind(share_pct)
                    .bind(Self::evidence_str(evidence))
                    .bind(Self::context_str(context))
                    .bind(&provenance.source_id)
                    .bind(provenance.source_authority)
                    .bind(provenance.observed_at)
                    .bind(&valid_range)
                    .bind(&known_range)
                    .bind(&predicate)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                    self.record_ownership_participation(
                        subject.0,
                        object.0,
                        assertion_id.0,
                        &valid_range,
                        &known_range,
                    )
                    .await?;
                } else {
                    sqlx::query(
                        r#"INSERT INTO assertion (id, subject_id, predicate, object_id, evidence, context,
                         source_id, source_authority, observed_at, valid_range, known_range, jurisdiction, governance)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::tstzrange,$11::tstzrange,'GLOBAL','OBSERVED')"#,
                    )
                    .bind(assertion_id.0)
                    .bind(subject.0)
                    .bind(&predicate)
                    .bind(object.0)
                    .bind(Self::evidence_str(evidence))
                    .bind(Self::context_str(context))
                    .bind(&provenance.source_id)
                    .bind(provenance.source_authority)
                    .bind(provenance.observed_at)
                    .bind(&valid_range)
                    .bind(&known_range)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                    self.record_assertion_participation(
                        subject.0,
                        object.0,
                        &predicate,
                        assertion_id.0,
                        &valid_range,
                        &known_range,
                    )
                    .await?;
                }
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::ComplianceRule {
                rule_id,
                description,
                threshold_pct,
            } => {
                sqlx::query(
                    "INSERT INTO compliance_rule (rule_id, description, threshold_pct) VALUES ($1,$2,$3) ON CONFLICT (rule_id) DO NOTHING",
                )
                .bind(&rule_id)
                .bind(&description)
                .bind(threshold_pct)
                .execute(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::RetroactiveCorrection {
                assertion_id,
                new_valid_from,
                corrected_at,
            } => {
                if self
                    .retroactive_correct_ownership(assertion_id, new_valid_from, corrected_at)
                    .await?
                {
                    delta.physical_mutations = 1;
                    delta.semantic_changes = 1;
                } else if self
                    .retroactive_correct_generic(assertion_id, new_valid_from, corrected_at)
                    .await?
                {
                    delta.physical_mutations = 1;
                    delta.semantic_changes = 1;
                }
            }
            Event::CloseAssertionKnowledge {
                assertion_id,
                known_to,
            } => {
                let bound = known_to.to_rfc3339();
                for table in ["ownership_assertion", "assertion"] {
                    sqlx::query(&format!(
                        "UPDATE {table} SET known_range = tstzrange(lower(known_range), $1::timestamptz, '[)') WHERE id = $2"
                    ))
                    .bind(known_to)
                    .bind(assertion_id.0)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                }
                self.close_participation_knowledge(assertion_id.0, known_to).await?;
                delta.physical_mutations += 1;
                let _ = bound;
            }
            Event::RegisterTrust {
                id,
                name,
                jurisdiction,
                ..
            } => {
                let result = sqlx::query(
                    "INSERT INTO trust (id, name, jurisdiction) VALUES ($1,$2,$3) ON CONFLICT (id) DO NOTHING",
                )
                .bind(id.0)
                .bind(&name)
                .bind(&jurisdiction)
                .execute(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                delta.physical_mutations = 1;
                delta.semantic_changes = if result.rows_affected() > 0 { 1 } else { 0 };
            }
            Event::ControlViaNominee {
                controller,
                controlled,
                nominee,
                instrument,
                context,
                jurisdiction,
                provenance,
                bitemporal,
            } => {
                self.ensure_source(&provenance.source_id, provenance.source_authority)
                    .await?;
                let (valid_range, known_range) = self.apply_ablation_bitemporal(&bitemporal);
                // The discriminator must be resolved by probing each party table, because
                // SQL cannot express "this UUID refers to some party".
                let kind = self.party_kind(controller).await?;
                let control_id: Uuid = sqlx::query_scalar(
                    r#"INSERT INTO control_via_nominee
                       (controller_id, controller_kind, controlled_id, nominee_id, instrument_id,
                        context, jurisdiction, source_id, source_authority, observed_at,
                        valid_range, known_range)
                       VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::tstzrange,$12::tstzrange)
                       RETURNING id"#,
                )
                .bind(controller.0)
                .bind(kind)
                .bind(controlled.0)
                .bind(nominee.0)
                .bind(instrument.0)
                .bind(Self::context_str(context))
                .bind(&jurisdiction)
                .bind(&provenance.source_id)
                .bind(provenance.source_authority)
                .bind(provenance.observed_at)
                .bind(&valid_range)
                .bind(&known_range)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
                self.record_control_participation(
                    control_id,
                    controller.0,
                    controlled.0,
                    nominee.0,
                    instrument.0,
                    &valid_range,
                    &known_range,
                )
                .await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
        }

        self.log_churn(
            delta.physical_mutations,
            delta.semantic_changes,
            "ingest",
        )
        .await?;
        Ok(delta)
    }

    fn apply_ablation_bitemporal(&self, b: &Bitemporal) -> (String, String) {
        let valid_from = if self.ablation == AblationDimension::ValidTime {
            DateTime::<Utc>::from_str("2020-01-01T00:00:00Z").unwrap()
        } else {
            b.valid_from
        };
        let valid_to = if self.ablation == AblationDimension::ValidTime {
            None
        } else {
            b.valid_to
        };
        let known_from = if self.ablation == AblationDimension::KnowledgeTime {
            DateTime::<Utc>::from_str("2020-01-01T00:00:00Z").unwrap()
        } else {
            b.known_from
        };
        let known_to = if self.ablation == AblationDimension::KnowledgeTime {
            None
        } else {
            b.known_to
        };
        (
            Self::tsrange(valid_from, valid_to),
            Self::tsrange(known_from, known_to),
        )
    }

    async fn state_at_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<EntityState> {
        let owners = self
            .beneficial_owners_async(entity, valid_at, known_at)
            .await?;
        let sanctioned = self.is_sanctioned_async(entity, valid_at, known_at).await?;
        Ok(EntityState {
            entity,
            assertions: vec![],
            beneficial_owners: owners,
            sanctioned,
        })
    }

    async fn beneficial_owners_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<PersonId>> {
        let rows = sqlx::query(
            r#"
            WITH RECURSIVE ownership_chain AS (
                SELECT o.owner_id, o.owner_kind, o.owned_id, 1 AS depth
                FROM ownership_assertion o
                WHERE o.owned_id = $1
                  AND o.valid_range @> $2::timestamptz
                  AND o.known_range @> $3::timestamptz
                  AND o.evidence != 'REFUTED'
                UNION
                SELECT o.owner_id, o.owner_kind, o.owned_id, oc.depth + 1
                FROM ownership_assertion o
                INNER JOIN ownership_chain oc
                    ON o.owned_id = oc.owner_id AND oc.owner_kind = 'company'
                WHERE o.valid_range @> $2::timestamptz
                  AND o.known_range @> $3::timestamptz
                  AND o.evidence != 'REFUTED'
                  AND oc.depth < 10
            )
            SELECT DISTINCT owner_id FROM ownership_chain WHERE owner_kind = 'person'
            "#,
        )
        .bind(entity.0)
        .bind(valid_at)
        .bind(known_at)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        let mut owners: Vec<PersonId> = rows
            .iter()
            .filter_map(|r| r.try_get::<Uuid, _>("owner_id").ok())
            .map(PersonId)
            .collect();
        owners.sort_by_key(|p| p.0);
        owners.dedup();
        Ok(owners)
    }

    async fn is_sanctioned_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<bool> {
        let row = sqlx::query(
            r#"SELECT listed FROM sanction_listing
               WHERE person_id = $1
                 AND valid_range @> $2::timestamptz
                 AND known_range @> $3::timestamptz
               ORDER BY lower(valid_range) DESC LIMIT 1"#,
        )
        .bind(entity.0)
        .bind(valid_at)
        .bind(known_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(row.and_then(|r| r.try_get("listed").ok()).unwrap_or(false))
    }

    async fn contradictions_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<Conflict>> {
        let rows = sqlx::query(
            r#"
            SELECT a1.id AS id_a, a2.id AS id_b, a1.predicate
            FROM assertion a1
            JOIN assertion a2 ON a1.subject_id = a2.subject_id
                AND a1.predicate = a2.predicate
                AND a1.object_id = a2.object_id
                AND a1.id < a2.id
            WHERE a1.subject_id = $1
              AND a1.valid_range @> $2::timestamptz
              AND a1.known_range @> $3::timestamptz
              AND a2.valid_range @> $2::timestamptz
              AND a2.known_range @> $3::timestamptz
              AND (
                (a1.evidence = 'SUPPORTED' AND a2.evidence = 'REFUTED')
                OR (a1.evidence = 'REFUTED' AND a2.evidence = 'SUPPORTED')
              )
            "#,
        )
        .bind(entity.0)
        .bind(valid_at)
        .bind(known_at)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        Ok(rows
            .iter()
            .map(|r| Conflict {
                assertion_a: AssertionId(r.try_get("id_a").unwrap()),
                assertion_b: AssertionId(r.try_get("id_b").unwrap()),
                reason: "evidence_contradiction".into(),
            })
            .collect())
    }

    async fn ownership_exposure_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Exposure> {
        let direct_rows = sqlx::query(
            r#"SELECT owner_id FROM ownership_assertion
               WHERE owned_id = $1 AND valid_range @> $2::timestamptz
                 AND known_range @> $3::timestamptz AND evidence != 'REFUTED'"#,
        )
        .bind(entity.0)
        .bind(valid_at)
        .bind(known_at)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        let direct = !direct_rows.is_empty();
        let mut path: Vec<EntityId> = direct_rows
            .iter()
            .filter_map(|r| r.try_get::<Uuid, _>("owner_id").ok())
            .map(|u| EntityId(u))
            .collect();

        let indirect_rows = sqlx::query(
            r#"
            WITH RECURSIVE chain AS (
                SELECT owner_id, owner_kind, owned_id, 1 AS depth FROM ownership_assertion
                WHERE owned_id = $1 AND valid_range @> $2::timestamptz
                  AND known_range @> $3::timestamptz AND evidence != 'REFUTED'
                UNION
                SELECT o.owner_id, o.owner_kind, o.owned_id, c.depth + 1
                FROM ownership_assertion o
                INNER JOIN chain c ON o.owned_id = c.owner_id AND c.owner_kind = 'company'
                WHERE o.valid_range @> $2::timestamptz AND o.known_range @> $3::timestamptz
                  AND o.evidence != 'REFUTED' AND c.depth < 10
            )
            SELECT DISTINCT owner_id FROM chain WHERE depth > 1
            "#,
        )
        .bind(entity.0)
        .bind(valid_at)
        .bind(known_at)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        let indirect = !indirect_rows.is_empty();
        path.extend(
            indirect_rows
                .iter()
                .filter_map(|r| r.try_get::<Uuid, _>("owner_id").ok())
                .map(EntityId),
        );

        let mut sanctioned_controller = None;
        for eid in &path {
            if self.is_sanctioned_async(*eid, valid_at, known_at).await? {
                sanctioned_controller = Some(PersonId(eid.0));
                break;
            }
        }

        Ok(Exposure {
            entity,
            direct,
            indirect,
            path,
            sanctioned_controller,
        })
    }

    async fn compliance_decision_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Decision> {
        let exposure = self
            .ownership_exposure_async(entity, valid_at, known_at)
            .await?;
        let conflicts = self.contradictions_async(entity, valid_at, known_at).await?;

        if exposure.sanctioned_controller.is_some() {
            return Ok(Decision::Block);
        }
        if !conflicts.is_empty() {
            return Ok(Decision::Review);
        }
        let threshold: f32 = sqlx::query_scalar(
            "SELECT threshold_pct FROM compliance_rule ORDER BY rule_id LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?
        .unwrap_or(25.0);
        let owners = self
            .beneficial_owners_async(entity, valid_at, known_at)
            .await?;
        if owners.is_empty() {
            Ok(Decision::Review)
        } else if exposure.direct || exposure.indirect {
            if threshold >= 25.0 {
                Ok(Decision::Allow)
            } else {
                Ok(Decision::Review)
            }
        } else {
            Ok(Decision::Allow)
        }
    }

    async fn identity_action_async(
        &self,
        person_a: PersonId,
        person_b: PersonId,
        _valid_at: Timestamp,
        _known_at: Timestamp,
    ) -> Result<IdentityAction> {
        let row_a = sqlx::query("SELECT canonical_name FROM person WHERE id = $1")
            .bind(person_a.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let row_b = sqlx::query("SELECT canonical_name FROM person WHERE id = $1")
            .bind(person_b.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        let canon_a: String = row_a
            .and_then(|r| r.try_get("canonical_name").ok())
            .unwrap_or_default();
        let canon_b: String = row_b
            .and_then(|r| r.try_get("canonical_name").ok())
            .unwrap_or_default();

        let merge_link = sqlx::query(
            "SELECT 1 FROM identity_alias WHERE person_id IN ($1, $2) AND merge = true LIMIT 1",
        )
        .bind(person_a.0)
        .bind(person_b.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        if merge_link.is_some() || (canon_a == canon_b && !canon_a.is_empty()) {
            Ok(IdentityAction::Merge)
        } else {
            Ok(IdentityAction::KeepSeparate)
        }
    }

    /// Resolve which party table a UUID lives in. A typed engine gets this from the
    /// instance itself; here it costs a three-way UNION on every polymorphic write.
    async fn party_kind(&self, id: EntityId) -> Result<&'static str> {
        let row = sqlx::query(
            r#"SELECT 'person' AS kind FROM person WHERE id = $1
               UNION ALL SELECT 'company' FROM company WHERE id = $1
               UNION ALL SELECT 'trust' FROM trust WHERE id = $1
               LIMIT 1"#,
        )
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        Ok(match row.map(|r| r.get::<String, _>("kind")).as_deref() {
            Some("company") => "company",
            Some("trust") => "trust",
            Some("person") => "person",
            _ => "unknown",
        })
    }

    /// Q9 against the base ontology.
    ///
    /// Frozen on purpose: the schema-evolution experiment measures what this query stops
    /// seeing once the ontology grows. Do not add the extension tables here — that is what
    /// `neighborhood_repaired_async` is for.
    async fn neighborhood_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Neighborhood> {
        let rows = sqlx::query(Self::NEIGHBORHOOD_SQL)
            .bind(entity.0)
            .bind(valid_at)
            .bind(known_at)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(Self::rows_to_neighborhood(entity, rows))
    }

    /// Same frozen query as [`Self::neighborhood_async`]; repair LOC is zero with the participation index.
    async fn neighborhood_repaired_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Neighborhood> {
        self.neighborhood_async(entity, valid_at, known_at).await
    }

    fn rows_to_neighborhood(entity: EntityId, rows: Vec<sqlx::postgres::PgRow>) -> Neighborhood {
        let edges = rows
            .into_iter()
            .map(|r| NeighborEdge {
                relation_type: r.get::<String, _>("rel"),
                role: r.get::<String, _>("role"),
                counterparty: r.get::<Option<Uuid>, _>("other").map(EntityId::from_uuid),
            })
            .collect();
        Neighborhood::new(entity, edges)
    }

    async fn context_compatibility_async(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Compatibility> {
        let rows = sqlx::query(
            r#"SELECT context, evidence FROM assertion
               WHERE subject_id = $1 AND valid_range @> $2::timestamptz AND known_range @> $3::timestamptz
               UNION ALL
               SELECT context, evidence FROM ownership_assertion
               WHERE (owner_id = $1 OR owned_id = $1)
                 AND valid_range @> $2::timestamptz AND known_range @> $3::timestamptz"#,
        )
        .bind(entity.0)
        .bind(valid_at)
        .bind(known_at)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        if rows.is_empty() {
            return Ok(Compatibility::Indeterminate);
        }

        let mut has_contradiction = false;
        let evidences: Vec<(String, String)> = rows
            .iter()
            .filter_map(|r| {
                Some((
                    r.try_get("context").ok()?,
                    r.try_get("evidence").ok()?,
                ))
            })
            .collect();

        for i in 0..evidences.len() {
            for j in (i + 1)..evidences.len() {
                let (_, e1) = &evidences[i];
                let (_, e2) = &evidences[j];
                if (e1 == "SUPPORTED" && e2 == "REFUTED") || (e1 == "REFUTED" && e2 == "SUPPORTED") {
                    has_contradiction = true;
                }
            }
            if evidences[i].0 == "SANCTIONS" && evidences[i].1 == "SUPPORTED" {
                has_contradiction = true;
            }
        }

        if has_contradiction {
            Ok(Compatibility::Contradictory)
        } else {
            Ok(Compatibility::Consistent)
        }
    }

    async fn retroactive_correct_ownership(
        &self,
        assertion_id: AssertionId,
        new_valid_from: Timestamp,
        corrected_at: Timestamp,
    ) -> Result<bool> {
        let row = sqlx::query(
            r#"SELECT owner_id, owner_kind, owned_id, share_pct, evidence, governance, context, role,
                      jurisdiction, source_id, source_authority, observed_at, valid_range, known_range, predicate
               FROM ownership_assertion WHERE id = $1"#,
        )
        .bind(assertion_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        let Some(row) = row else {
            return Ok(false);
        };

        sqlx::query(
            "UPDATE ownership_assertion SET known_range = tstzrange(lower(known_range), $1::timestamptz, '[)') WHERE id = $2",
        )
        .bind(corrected_at)
        .bind(assertion_id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        self.close_participation_knowledge(assertion_id.0, corrected_at).await?;

        let owner_id: Uuid = row.get("owner_id");
        let owned_id: Uuid = row.get("owned_id");
        let predicate: String = row.get("predicate");
        let valid_upper: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT upper(valid_range) FROM ownership_assertion WHERE id = $1",
        )
        .bind(assertion_id.0)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        let valid_to = valid_upper.filter(|t| *t > new_valid_from);
        let new_bitemporal = Bitemporal {
            valid_from: new_valid_from,
            valid_to,
            known_from: corrected_at,
            known_to: None,
        };
        let new_id = AssertionId::deterministic(
            EntityId::from_uuid(owner_id),
            &predicate,
            EntityId::from_uuid(owned_id),
            &new_bitemporal,
            0,
            &format!("retro:{}@{}", assertion_id.0, corrected_at.timestamp()),
        );
        let (valid_range, known_range) = self.apply_ablation_bitemporal(&new_bitemporal);

        sqlx::query(
            r#"INSERT INTO ownership_assertion
            (id, owner_id, owner_kind, owned_id, share_pct, evidence, governance, context, role, jurisdiction,
             source_id, source_authority, observed_at, valid_range, known_range, predicate)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14::tstzrange,$15::tstzrange,$16)"#,
        )
        .bind(new_id.0)
        .bind(owner_id)
        .bind(row.get::<String, _>("owner_kind"))
        .bind(owned_id)
        .bind(row.get::<f32, _>("share_pct"))
        .bind(row.get::<String, _>("evidence"))
        .bind(row.get::<String, _>("governance"))
        .bind(row.get::<String, _>("context"))
        .bind(row.get::<Option<String>, _>("role"))
        .bind(row.get::<String, _>("jurisdiction"))
        .bind(row.get::<String, _>("source_id"))
        .bind(row.get::<f32, _>("source_authority"))
        .bind(row.get::<DateTime<Utc>, _>("observed_at"))
        .bind(&valid_range)
        .bind(&known_range)
        .bind(&predicate)
        .execute(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        self.record_ownership_participation(owner_id, owned_id, new_id.0, &valid_range, &known_range)
            .await?;
        Ok(true)
    }

    async fn retroactive_correct_generic(
        &self,
        assertion_id: AssertionId,
        new_valid_from: Timestamp,
        corrected_at: Timestamp,
    ) -> Result<bool> {
        let row = sqlx::query(
            r#"SELECT subject_id, predicate, object_id, evidence, context, source_id, source_authority,
                      observed_at, valid_range, known_range, jurisdiction, governance
               FROM assertion WHERE id = $1"#,
        )
        .bind(assertion_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        let Some(row) = row else {
            return Ok(false);
        };

        sqlx::query(
            "UPDATE assertion SET known_range = tstzrange(lower(known_range), $1::timestamptz, '[)') WHERE id = $2",
        )
        .bind(corrected_at)
        .bind(assertion_id.0)
        .execute(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        self.close_participation_knowledge(assertion_id.0, corrected_at).await?;

        let subject_id: Uuid = row.get("subject_id");
        let object_id: Uuid = row.get("object_id");
        let predicate: String = row.get("predicate");
        let new_bitemporal = Bitemporal {
            valid_from: new_valid_from,
            valid_to: None,
            known_from: corrected_at,
            known_to: None,
        };
        let new_id = AssertionId::deterministic(
            EntityId::from_uuid(subject_id),
            &predicate,
            EntityId::from_uuid(object_id),
            &new_bitemporal,
            0,
            &format!("retro:{}@{}", assertion_id.0, corrected_at.timestamp()),
        );
        let (valid_range, known_range) = self.apply_ablation_bitemporal(&new_bitemporal);

        sqlx::query(
            r#"INSERT INTO assertion (id, subject_id, predicate, object_id, evidence, context,
             source_id, source_authority, observed_at, valid_range, known_range, jurisdiction, governance)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::tstzrange,$11::tstzrange,$12,$13)"#,
        )
        .bind(new_id.0)
        .bind(subject_id)
        .bind(&predicate)
        .bind(object_id)
        .bind(row.get::<String, _>("evidence"))
        .bind(row.get::<String, _>("context"))
        .bind(row.get::<String, _>("source_id"))
        .bind(row.get::<f32, _>("source_authority"))
        .bind(row.get::<DateTime<Utc>, _>("observed_at"))
        .bind(&valid_range)
        .bind(&known_range)
        .bind(row.get::<String, _>("jurisdiction"))
        .bind(row.get::<String, _>("governance"))
        .execute(&self.pool)
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        self.record_assertion_participation(
            subject_id,
            object_id,
            &predicate,
            new_id.0,
            &valid_range,
            &known_range,
        )
        .await?;
        Ok(true)
    }
}
