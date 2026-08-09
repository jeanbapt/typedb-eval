use async_trait::async_trait;
use typedb_driver::{
    Addresses, Credentials, DriverOptions, DriverTlsConfig, TransactionType, TypeDBDriver,
};

use benchmark_core::error::Result;
use benchmark_core::{
    AblationDimension, Compatibility, ComplianceStore, Conflict, Decision, EntityId, EntityState,
    Event, Exposure, IdentityAction, Neighborhood, PersonId, StateDelta, Timestamp,
};

use crate::reads::TypeDbReads;

pub struct TypeDbStore {
    driver: TypeDBDriver,
    database: String,
    ablation: AblationDimension,
    churn: StateDelta,
}

impl TypeDbStore {
    pub async fn connect(address: &str, database: &str) -> Result<Self> {
        let credentials = Credentials::new("admin", "password");
        let driver = TypeDBDriver::new(
            Addresses::try_from_address_str(address)
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?,
            credentials.clone(),
            DriverOptions::new(DriverTlsConfig::disabled()),
        )
        .await
        .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        Ok(Self {
            driver,
            database: database.to_string(),
            ablation: AblationDimension::None,
            churn: StateDelta::default(),
        })
    }

    pub async fn connect_with_ablation(
        address: &str,
        database: &str,
        ablation: AblationDimension,
    ) -> Result<Self> {
        let mut store = Self::connect(address, database).await?;
        store.ablation = ablation;
        Ok(store)
    }

    pub async fn setup_database(&self) -> Result<()> {
        let dbs = self.driver.databases();
        let exists = dbs
            .contains(&self.database)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        if exists {
            let db = dbs
                .get(&self.database)
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
            db.delete()
                .await
                .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        }
        dbs.create(&self.database)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;

        let schema = include_str!("../schema.tql");
        let tx = self
            .driver
            .transaction(&self.database, TransactionType::Schema)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        tx.query(schema)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn churn(&self) -> &StateDelta {
        &self.churn
    }

    fn dt(t: Timestamp) -> String {
        t.format("%Y-%m-%dT%H:%M:%S").to_string()
    }

    fn evidence_str(e: benchmark_core::EvidenceState) -> &'static str {
        match e {
            benchmark_core::EvidenceState::Unknown => "UNKNOWN",
            benchmark_core::EvidenceState::Supported => "SUPPORTED",
            benchmark_core::EvidenceState::Refuted => "REFUTED",
            benchmark_core::EvidenceState::Contradictory => "CONTRADICTORY",
        }
    }

    fn governance_str(g: benchmark_core::GovernanceLevel) -> &'static str {
        match g {
            benchmark_core::GovernanceLevel::Observed => "OBSERVED",
            benchmark_core::GovernanceLevel::Corroborated => "CORROBORATED",
            benchmark_core::GovernanceLevel::Reviewed => "REVIEWED",
            benchmark_core::GovernanceLevel::Final => "FINAL",
        }
    }

    fn context_str(c: benchmark_core::Context) -> &'static str {
        match c {
            benchmark_core::Context::CorporateRegistry => "CORPORATE_REGISTRY",
            benchmark_core::Context::Kyc => "KYC",
            benchmark_core::Context::Sanctions => "SANCTIONS",
            benchmark_core::Context::Regulatory => "REGULATORY",
        }
    }

    fn party_match(owner: benchmark_core::PartyId) -> String {
        match owner {
            benchmark_core::PartyId::Person(p) => {
                format!(r#"$owner isa person, has entity-id "{}";"#, p.0)
            }
            benchmark_core::PartyId::Company(c) => {
                format!(r#"$owner isa company, has entity-id "{}";"#, c.0)
            }
            benchmark_core::PartyId::Trust(t) => {
                format!(r#"$owner isa trust, has entity-id "{}";"#, t.0)
            }
        }
    }

    fn role_str(r: benchmark_core::Role) -> &'static str {
        match r {
            benchmark_core::Role::BeneficialOwner => "BENEFICIAL_OWNER",
            benchmark_core::Role::Director => "DIRECTOR",
            benchmark_core::Role::Shareholder => "SHAREHOLDER",
            benchmark_core::Role::Controller => "CONTROLLER",
        }
    }

    async fn run_write(&self, query: &str) -> Result<()> {
        let tx = self
            .driver
            .transaction(&self.database, TransactionType::Write)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        tx.query(query)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        Ok(())
    }

    fn reads(&self) -> TypeDbReads<'_> {
        TypeDbReads {
            driver: &self.driver,
            database: &self.database,
        }
    }
}

#[async_trait]
impl ComplianceStore for TypeDbStore {
    async fn ingest(&mut self, event: Event) -> Result<StateDelta> {
        self.ingest_async(event).await
    }

    async fn state_at(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<EntityState> {
        self.reads().state_at(entity, valid_at, known_at).await
    }

    async fn contradictions(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<Conflict>> {
        self.reads()
            .contradictions(entity, valid_at, known_at)
            .await
    }

    async fn ownership_exposure(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Exposure> {
        self.reads()
            .ownership_exposure(entity, valid_at, known_at)
            .await
    }

    async fn compliance_decision(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Decision> {
        self.reads()
            .compliance_decision(entity, valid_at, known_at)
            .await
    }

    async fn identity_action(
        &self,
        person_a: PersonId,
        person_b: PersonId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<IdentityAction> {
        self.reads()
            .identity_action(person_a, person_b, valid_at, known_at)
            .await
    }

    async fn context_compatibility(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Compatibility> {
        self.reads()
            .context_compatibility(entity, valid_at, known_at)
            .await
    }

    async fn neighborhood(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Neighborhood> {
        self.reads().neighborhood(entity, valid_at, known_at).await
    }

    async fn reset(&mut self) -> Result<()> {
        self.churn = StateDelta::default();
        self.setup_database().await
    }
}

impl TypeDbStore {
    async fn ingest_async(&mut self, event: Event) -> Result<StateDelta> {
        let mut delta = StateDelta::default();

        match event {
            Event::RegisterPerson {
                id,
                name,
                canonical_name,
                jurisdiction,
                at,
                ..
            } => {
                let canonical = if self.ablation == AblationDimension::Identity {
                    name.clone()
                } else {
                    canonical_name
                };
                let q = format!(
                    r#"insert $p isa person,
                        has entity-id "{id}",
                        has person-name "{name}",
                        has canonical-name "{canonical}",
                        has jurisdiction-code "{jurisdiction}";"#,
                    id = id.0,
                    name = name.replace('"', ""),
                    canonical = canonical.replace('"', ""),
                    jurisdiction = jurisdiction,
                );
                self.run_write(&q).await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
                let _ = at;
            }
            Event::RegisterCompany {
                id,
                name,
                jurisdiction,
                ..
            } => {
                let q = format!(
                    r#"insert $c isa company,
                        has entity-id "{id}",
                        has company-name "{name}",
                        has jurisdiction-code "{jurisdiction}";"#,
                    id = id.0,
                    name = name.replace('"', ""),
                    jurisdiction = jurisdiction,
                );
                self.run_write(&q).await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
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
                let evidence = if self.ablation == AblationDimension::Evidence {
                    benchmark_core::EvidenceState::Unknown
                } else {
                    evidence
                };
                let valid_to = bitemporal
                    .valid_to
                    .map(|t| format!(r#", has valid-to {}"#, Self::dt(t)))
                    .unwrap_or_default();
                let known_to = bitemporal
                    .known_to
                    .map(|t| format!(r#", has known-to {}"#, Self::dt(t)))
                    .unwrap_or_default();
                let q = format!(
                    r#"match
                        {owner_match}
                        $owned isa company, has entity-id "{owned_id}";
                    insert
                        (owner: $owner, owned: $owned) isa ownership,
                        has share-pct {share_pct},
                        has evidence-state "{evidence}",
                        has governance-level "{governance}",
                        has context-type "{context}",
                        has role-type "{role}",
                        has jurisdiction-code "{jurisdiction}",
                        has source-id "{source}",
                        has source-authority {authority},
                        has valid-from {valid_from}{valid_to},
                        has known-from {known_from}{known_to},
                        has observed-at {observed};"#,
                    owner_match = Self::party_match(owner),
                    owned_id = owned.0,
                    share_pct = share_pct,
                    evidence = Self::evidence_str(evidence),
                    governance = Self::governance_str(governance),
                    context = Self::context_str(context),
                    role = Self::role_str(role),
                    jurisdiction = jurisdiction,
                    source = provenance.source_id.replace('"', ""),
                    authority = provenance.source_authority,
                    valid_from = Self::dt(bitemporal.valid_from),
                    valid_to = valid_to,
                    known_from = Self::dt(bitemporal.known_from),
                    known_to = known_to,
                    observed = Self::dt(provenance.observed_at),
                );
                self.run_write(&q).await?;
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
                let q = format!(
                    r#"match $p isa person, has entity-id "{pid}";
                    insert (linked-person: $p) isa identity-link,
                        has alias-text "{alias}",
                        has canonical-name "{canonical}",
                        has merge-flag {merge},
                        has context-type "{context}",
                        has observed-at {at};"#,
                    pid = person_a.0,
                    alias = alias.replace('"', ""),
                    canonical = canonical.replace('"', ""),
                    merge = merge,
                    context = Self::context_str(context),
                    at = Self::dt(at),
                );
                self.run_write(&q).await?;
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
                let valid_to = bitemporal
                    .valid_to
                    .map(|t| format!(r#", has valid-to {}"#, Self::dt(t)))
                    .unwrap_or_default();
                let known_to = bitemporal
                    .known_to
                    .map(|t| format!(r#", has known-to {}"#, Self::dt(t)))
                    .unwrap_or_default();
                let q = format!(
                    r#"match $p isa person, has entity-id "{pid}";
                    insert (sanctioned-person: $p) isa sanction-listing,
                        has list-name "{list}",
                        has listed-flag {listed},
                        has context-type "{context}",
                        has valid-from {vf}{valid_to},
                        has known-from {kf}{known_to};"#,
                    pid = person.0,
                    list = list_name.replace('"', ""),
                    listed = listed,
                    context = Self::context_str(context),
                    vf = Self::dt(bitemporal.valid_from),
                    valid_to = valid_to,
                    kf = Self::dt(bitemporal.known_from),
                    known_to = known_to,
                );
                self.run_write(&q).await?;
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
                let valid_to = bitemporal
                    .valid_to
                    .map(|t| format!(r#", has valid-to {}"#, Self::dt(t)))
                    .unwrap_or_default();
                let known_to = bitemporal
                    .known_to
                    .map(|t| format!(r#", has known-to {}"#, Self::dt(t)))
                    .unwrap_or_default();
                for ev in [supporting, refuting] {
                    let q = format!(
                        r#"match
                            {{ $s isa person, has entity-id "{subj}"; }} or {{ $s isa company, has entity-id "{subj}"; }};
                            {{ $o isa person, has entity-id "{obj}"; }} or {{ $o isa company, has entity-id "{obj}"; }};
                        insert
                            (subject: $s, object: $o) isa generic-assertion,
                            has predicate-name "{pred}",
                            has evidence-state "{evidence}",
                            has context-type "{context}",
                            has source-id "{source}",
                            has source-authority {auth},
                            has valid-from {vf}{valid_to},
                            has known-from {kf}{known_to},
                            has observed-at {obs},
                            has jurisdiction-code "GLOBAL",
                            has governance-level "OBSERVED";"#,
                        subj = subject.0,
                        obj = object.0,
                        pred = predicate.replace('"', ""),
                        evidence = Self::evidence_str(ev),
                        context = Self::context_str(context),
                        source = provenance.source_id.replace('"', ""),
                        auth = provenance.source_authority,
                        vf = Self::dt(bitemporal.valid_from),
                        valid_to = valid_to,
                        kf = Self::dt(bitemporal.known_from),
                        known_to = known_to,
                        obs = Self::dt(provenance.observed_at),
                    );
                    self.run_write(&q).await?;
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
                ..
            } => {
                let valid_to = bitemporal
                    .valid_to
                    .map(|t| format!(r#", has valid-to {}"#, Self::dt(t)))
                    .unwrap_or_default();
                let known_to = bitemporal
                    .known_to
                    .map(|t| format!(r#", has known-to {}"#, Self::dt(t)))
                    .unwrap_or_default();
                let q = format!(
                    r#"match
                        {{ $s isa person, has entity-id "{subj}"; }} or {{ $s isa company, has entity-id "{subj}"; }};
                        {{ $o isa person, has entity-id "{obj}"; }} or {{ $o isa company, has entity-id "{obj}"; }};
                    insert
                        (subject: $s, object: $o) isa generic-assertion,
                        has predicate-name "{pred}",
                        has evidence-state "{evidence}",
                        has context-type "{context}",
                        has source-id "{source}",
                        has source-authority {auth},
                        has valid-from {vf}{valid_to},
                        has known-from {kf}{known_to},
                        has observed-at {obs},
                        has jurisdiction-code "GLOBAL",
                        has governance-level "OBSERVED";"#,
                    subj = subject.0,
                    obj = object.0,
                    pred = predicate.replace('"', ""),
                    evidence = Self::evidence_str(evidence),
                    context = Self::context_str(context),
                    source = provenance.source_id.replace('"', ""),
                    auth = provenance.source_authority,
                    vf = Self::dt(bitemporal.valid_from),
                    valid_to = valid_to,
                    kf = Self::dt(bitemporal.known_from),
                    known_to = known_to,
                    obs = Self::dt(provenance.observed_at),
                );
                self.run_write(&q).await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::ComplianceRule {
                rule_id,
                threshold_pct,
                ..
            } => {
                let q = format!(
                    r#"insert $r isa compliance-rule,
                        has rule-id "{rid}",
                        has threshold-pct {threshold};"#,
                    rid = rule_id.replace('"', ""),
                    threshold = threshold_pct,
                );
                self.run_write(&q).await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::RetroactiveCorrection { .. } | Event::CloseAssertionKnowledge { .. } => {
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::RegisterTrust {
                id,
                name,
                jurisdiction,
                ..
            } => {
                let q = format!(
                    r#"insert $t isa trust,
                        has entity-id "{id}",
                        has trust-name "{name}",
                        has jurisdiction-code "{jurisdiction}";"#,
                    id = id.0,
                    name = name.replace('"', ""),
                );
                self.run_write(&q).await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
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
                // The controller is matched without naming its type: whichever party kind
                // it is, the schema already says it can fill the role.
                let mut q = format!(
                    r#"match
                        $ctrl has entity-id "{controller}";
                        $cd isa company, has entity-id "{controlled}";
                        $nom isa person, has entity-id "{nominee}";
                        $inst isa trust, has entity-id "{instrument}";
                    insert
                        $r isa control-via-nominee (controller: $ctrl, controlled: $cd, nominee: $nom, instrument: $inst),
                        has context-type "{context}",
                        has jurisdiction-code "{jurisdiction}",
                        has source-id "{source}",
                        has source-authority {authority},
                        has observed-at {observed},
                        has valid-from {valid_from},
                        has known-from {known_from}"#,
                    controller = controller.0,
                    controlled = controlled.0,
                    nominee = nominee.0,
                    instrument = instrument.0,
                    context = Self::context_str(context),
                    source = provenance.source_id.replace('"', ""),
                    authority = provenance.source_authority,
                    observed = Self::dt(provenance.observed_at),
                    valid_from = Self::dt(bitemporal.valid_from),
                    known_from = Self::dt(bitemporal.known_from),
                );
                if let Some(vt) = bitemporal.valid_to {
                    q.push_str(&format!(", has valid-to {}", Self::dt(vt)));
                }
                if let Some(kt) = bitemporal.known_to {
                    q.push_str(&format!(", has known-to {}", Self::dt(kt)));
                }
                q.push(';');
                self.run_write(&q).await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
        }

        self.churn.physical_mutations += delta.physical_mutations;
        self.churn.semantic_changes += delta.semantic_changes;
        Ok(delta)
    }
}
