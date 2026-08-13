use std::sync::LazyLock;

use async_trait::async_trait;
use typedb_driver::{
    given::{GivenRowEntry, GivenRows},
    Addresses, Credentials, DriverOptions, DriverTlsConfig, TransactionType, TypeDBDriver,
};

use benchmark_core::error::Result;
use benchmark_core::{
    AblationDimension, AssertionId, Bitemporal, Compatibility, ComplianceStore, Conflict, Decision,
    EntityId, EntityState, Event, Exposure, IdentityAction, Neighborhood, PersonId, StateDelta,
    Timestamp,
};

use crate::reads::TypeDbReads;

/// Builds a single-row `GivenRows` from (variable, entry) pairs, so every write
/// query keeps a constant text and passes its parameters out of band.
fn given_row(entries: Vec<(&str, GivenRowEntry)>) -> GivenRows {
    let (names, values): (Vec<String>, Vec<GivenRowEntry>) = entries
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .unzip();
    let mut rows = GivenRows::new(names, 1);
    rows.push_row(values).expect("given row width");
    rows
}

fn dt_entry(t: Timestamp) -> GivenRowEntry {
    GivenRowEntry::from(t.naive_utc())
}

fn opt_dt_entry(t: Option<Timestamp>) -> GivenRowEntry {
    t.map(dt_entry).unwrap_or(GivenRowEntry::Empty)
}

/// f32 → f64 through the decimal representation, so the stored double is identical
/// to what the legacy text-interpolated query produced (`25.5f32 as f64` would not be).
fn pct_entry(v: f32) -> GivenRowEntry {
    GivenRowEntry::from(v.to_string().parse::<f64>().unwrap_or(v as f64))
}

const REGISTER_PERSON_Q: &str = r#"given $id: string, $name: string, $canonical: string, $jur: string;
insert $p isa person,
    has entity-id == $id,
    has person-name == $name,
    has canonical-name == $canonical,
    has jurisdiction-code == $jur;"#;

const REGISTER_COMPANY_Q: &str = r#"given $id: string, $name: string, $jur: string;
insert $c isa company,
    has entity-id == $id,
    has company-name == $name,
    has jurisdiction-code == $jur;"#;

const REGISTER_TRUST_Q: &str = r#"given $id: string, $name: string, $jur: string;
insert $t isa trust,
    has entity-id == $id,
    has trust-name == $name,
    has jurisdiction-code == $jur;"#;

/// Shared `given` prologue for every ownership insert.
const OWNERSHIP_GIVEN: &str = "given $oid: string, $cid: string, $aid: string, $sp: double, \
$ev: string, $gov: string, $ctx: string, $role: string, $jur: string, $src: string, \
$auth: double, $vf: datetime, $vt: datetime?, $kf: datetime, $kt: datetime?, $obs: datetime;";

fn ownership_insert_q(owner_match: &str) -> String {
    format!(
        r#"{OWNERSHIP_GIVEN}
match
    {owner_match}
    $owned isa company, has entity-id == $cid;
insert
    $r isa ownership, links (owner: $owner, owned: $owned),
    has assertion-id == $aid,
    has share-pct == $sp,
    has evidence-state == $ev,
    has governance-level == $gov,
    has context-type == $ctx,
    has role-type == $role,
    has jurisdiction-code == $jur,
    has source-id == $src,
    has source-authority == $auth,
    has observed-at == $obs,
    has valid-from == $vf,
    has known-from == $kf;
    try {{ $r has valid-to == $vt; }};
    try {{ $r has known-to == $kt; }};"#
    )
}

static OWNERSHIP_INSERT_PERSON_Q: LazyLock<String> =
    LazyLock::new(|| ownership_insert_q("$owner isa person, has entity-id == $oid;"));
static OWNERSHIP_INSERT_COMPANY_Q: LazyLock<String> =
    LazyLock::new(|| ownership_insert_q("$owner isa company, has entity-id == $oid;"));
static OWNERSHIP_INSERT_TRUST_Q: LazyLock<String> =
    LazyLock::new(|| ownership_insert_q("$owner isa trust, has entity-id == $oid;"));
static OWNERSHIP_INSERT_ANY_Q: LazyLock<String> = LazyLock::new(|| {
    ownership_insert_q(
        "{ $owner isa person, has entity-id == $oid; } or { $owner isa company, has entity-id == $oid; };",
    )
});

const GENERIC_INSERT_Q: &str = r#"given $sid: string, $oid: string, $aid: string, $pred: string, $ev: string, $ctx: string, $src: string, $auth: double, $vf: datetime, $vt: datetime?, $kf: datetime, $kt: datetime?, $obs: datetime;
match
    { $s isa person, has entity-id == $sid; } or { $s isa company, has entity-id == $sid; };
    { $o isa person, has entity-id == $oid; } or { $o isa company, has entity-id == $oid; };
insert
    $r isa generic-assertion, links (subject: $s, object: $o),
    has assertion-id == $aid,
    has predicate-name == $pred,
    has evidence-state == $ev,
    has context-type == $ctx,
    has source-id == $src,
    has source-authority == $auth,
    has observed-at == $obs,
    has valid-from == $vf,
    has known-from == $kf,
    has jurisdiction-code "GLOBAL",
    has governance-level "OBSERVED";
    try { $r has valid-to == $vt; };
    try { $r has known-to == $kt; };"#;

const IDENTITY_ALIAS_Q: &str = r#"given $pid: string, $alias: string, $canonical: string, $merge: boolean, $ctx: string, $at: datetime;
match $p isa person, has entity-id == $pid;
insert (linked-person: $p) isa identity-link,
    has alias-text == $alias,
    has canonical-name == $canonical,
    has merge-flag == $merge,
    has context-type == $ctx,
    has observed-at == $at;"#;

const SANCTION_LISTING_Q: &str = r#"given $pid: string, $list: string, $listed: boolean, $ctx: string, $vf: datetime, $vt: datetime?, $kf: datetime, $kt: datetime?;
match $p isa person, has entity-id == $pid;
insert
    $s isa sanction-listing, links (sanctioned-person: $p),
    has list-name == $list,
    has listed-flag == $listed,
    has context-type == $ctx,
    has valid-from == $vf,
    has known-from == $kf;
    try { $s has valid-to == $vt; };
    try { $s has known-to == $kt; };"#;

const COMPLIANCE_RULE_Q: &str = r#"given $rid: string, $threshold: double;
insert $r isa compliance-rule, has rule-id == $rid, has threshold-pct == $threshold;"#;

const CLOSE_KNOWLEDGE_Q: &str = r#"given $id: string, $kt: datetime;
match $r has assertion-id == $id;
update $r has known-to == $kt;"#;

const CONTROL_NOMINEE_Q: &str = r#"given $controller: string, $controlled: string, $nominee: string, $instrument: string, $ctx: string, $jur: string, $src: string, $auth: double, $obs: datetime, $vf: datetime, $vt: datetime?, $kf: datetime, $kt: datetime?;
match
    $ctrl has entity-id == $controller;
    $cd isa company, has entity-id == $controlled;
    $nom isa person, has entity-id == $nominee;
    $inst isa trust, has entity-id == $instrument;
insert
    $r isa control-via-nominee, links (controller: $ctrl, controlled: $cd, nominee: $nom, instrument: $inst),
    has context-type == $ctx,
    has jurisdiction-code == $jur,
    has source-id == $src,
    has source-authority == $auth,
    has observed-at == $obs,
    has valid-from == $vf,
    has known-from == $kf;
    try { $r has valid-to == $vt; };
    try { $r has known-to == $kt; };"#;

const FETCH_OWNERSHIP_Q: &str = r#"given $id: string;
match
    $old isa ownership, has assertion-id == $id,
        has share-pct $sp, has evidence-state $ev, has governance-level $gov,
        has context-type $ctx, has role-type $role, has jurisdiction-code $jur,
        has source-id $src, has source-authority $auth, has observed-at $obs;
    $old links (owner: $owner, owned: $owned);
    $owner has entity-id $oid;
    $owned has entity-id $cid;
select $sp, $ev, $gov, $ctx, $role, $jur, $src, $auth, $obs, $oid, $cid;"#;

const FETCH_GENERIC_Q: &str = r#"given $id: string;
match
    $old isa generic-assertion, has assertion-id == $id,
        has predicate-name $pred, has evidence-state $ev, has context-type $ctx,
        has source-id $src, has source-authority $auth, has observed-at $obs;
    $old links (subject: $s, object: $o);
    $s has entity-id $sid;
    $o has entity-id $oid;
select $pred, $ev, $ctx, $src, $auth, $obs, $sid, $oid;"#;

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

    /// Parses a datetime rendered by `concept_string` back into a `Timestamp`.
    fn parse_dt(s: &str) -> Option<Timestamp> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .ok()
            .map(|t| t.and_utc())
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

    /// Ownership insert variant whose owner match names the concrete party type.
    fn ownership_insert_query(owner: benchmark_core::PartyId) -> &'static str {
        match owner {
            benchmark_core::PartyId::Person(_) => &OWNERSHIP_INSERT_PERSON_Q,
            benchmark_core::PartyId::Company(_) => &OWNERSHIP_INSERT_COMPANY_Q,
            benchmark_core::PartyId::Trust(_) => &OWNERSHIP_INSERT_TRUST_Q,
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

    /// Commits a constant-text write query with its parameters passed as `given` rows.
    async fn run_write_given(&self, query: &str, rows: GivenRows) -> Result<()> {
        let tx = self
            .driver
            .transaction(&self.database, TransactionType::Write)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        tx.query_with_rows(query, rows)
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

    async fn retroactive_correct(
        &self,
        assertion_id: AssertionId,
        new_valid_from: Timestamp,
        corrected_at: Timestamp,
    ) -> Result<()> {
        self.run_write_given(
            CLOSE_KNOWLEDGE_Q,
            given_row(vec![
                ("id", GivenRowEntry::from(assertion_id.0.to_string())),
                ("kt", dt_entry(corrected_at)),
            ]),
        )
        .await?;

        let id_row = || given_row(vec![("id", GivenRowEntry::from(assertion_id.0.to_string()))]);

        if let Ok(rows) = self
            .reads()
            .collect_named_rows_given(FETCH_OWNERSHIP_Q, id_row())
            .await
        {
            if let Some(row) = rows.first() {
                let owner = EntityId::from_uuid(uuid::Uuid::parse_str(row.get("oid").unwrap()).unwrap());
                let owned = EntityId::from_uuid(uuid::Uuid::parse_str(row.get("cid").unwrap()).unwrap());
                let share_pct: f32 = row.get("sp").and_then(|s| s.parse().ok()).unwrap_or(0.0);
                let predicate = format!("owns_{share_pct}");
                let new_bitemporal = Bitemporal {
                    valid_from: new_valid_from,
                    valid_to: None,
                    known_from: corrected_at,
                    known_to: None,
                };
                let new_id = AssertionId::deterministic(
                    owner,
                    &predicate,
                    owned,
                    &new_bitemporal,
                    0,
                    &format!("retro:{}@{}", assertion_id.0, corrected_at.timestamp()),
                );
                let observed = row
                    .get("obs")
                    .and_then(|s| Self::parse_dt(s))
                    .unwrap_or(corrected_at);
                return self
                    .run_write_given(
                        &OWNERSHIP_INSERT_ANY_Q,
                        given_row(vec![
                            ("oid", GivenRowEntry::from(owner.0.to_string())),
                            ("cid", GivenRowEntry::from(owned.0.to_string())),
                            ("aid", GivenRowEntry::from(new_id.0.to_string())),
                            ("sp", pct_entry(share_pct)),
                            ("ev", GivenRowEntry::from(row.get("ev").cloned().unwrap_or_default())),
                            ("gov", GivenRowEntry::from(row.get("gov").cloned().unwrap_or_default())),
                            ("ctx", GivenRowEntry::from(row.get("ctx").cloned().unwrap_or_default())),
                            ("role", GivenRowEntry::from(row.get("role").cloned().unwrap_or_default())),
                            ("jur", GivenRowEntry::from(row.get("jur").cloned().unwrap_or_default())),
                            ("src", GivenRowEntry::from(row.get("src").cloned().unwrap_or_default())),
                            (
                                "auth",
                                GivenRowEntry::from(
                                    row.get("auth").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0),
                                ),
                            ),
                            ("vf", dt_entry(new_valid_from)),
                            ("vt", GivenRowEntry::Empty),
                            ("kf", dt_entry(corrected_at)),
                            ("kt", GivenRowEntry::Empty),
                            ("obs", dt_entry(observed)),
                        ]),
                    )
                    .await;
            }
        }

        if let Ok(rows) = self
            .reads()
            .collect_named_rows_given(FETCH_GENERIC_Q, id_row())
            .await
        {
            if let Some(row) = rows.first() {
                let subject = EntityId::from_uuid(uuid::Uuid::parse_str(row.get("sid").unwrap()).unwrap());
                let object = EntityId::from_uuid(uuid::Uuid::parse_str(row.get("oid").unwrap()).unwrap());
                let predicate = row.get("pred").cloned().unwrap_or_default();
                let new_bitemporal = Bitemporal {
                    valid_from: new_valid_from,
                    valid_to: None,
                    known_from: corrected_at,
                    known_to: None,
                };
                let new_id = AssertionId::deterministic(
                    subject,
                    &predicate,
                    object,
                    &new_bitemporal,
                    0,
                    &format!("retro:{}@{}", assertion_id.0, corrected_at.timestamp()),
                );
                let observed = row
                    .get("obs")
                    .and_then(|s| Self::parse_dt(s))
                    .unwrap_or(corrected_at);
                return self
                    .run_write_given(
                        GENERIC_INSERT_Q,
                        given_row(vec![
                            ("sid", GivenRowEntry::from(subject.0.to_string())),
                            ("oid", GivenRowEntry::from(object.0.to_string())),
                            ("aid", GivenRowEntry::from(new_id.0.to_string())),
                            ("pred", GivenRowEntry::from(predicate.replace('"', ""))),
                            ("ev", GivenRowEntry::from(row.get("ev").cloned().unwrap_or_default())),
                            ("ctx", GivenRowEntry::from(row.get("ctx").cloned().unwrap_or_default())),
                            ("src", GivenRowEntry::from(row.get("src").cloned().unwrap_or_default())),
                            (
                                "auth",
                                GivenRowEntry::from(
                                    row.get("auth").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0),
                                ),
                            ),
                            ("vf", dt_entry(new_valid_from)),
                            ("vt", GivenRowEntry::Empty),
                            ("kf", dt_entry(corrected_at)),
                            ("kt", GivenRowEntry::Empty),
                            ("obs", dt_entry(observed)),
                        ]),
                    )
                    .await;
            }
        }
        Ok(())
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
                self.run_write_given(
                    REGISTER_PERSON_Q,
                    given_row(vec![
                        ("id", GivenRowEntry::from(id.0.to_string())),
                        ("name", GivenRowEntry::from(name.replace('"', ""))),
                        ("canonical", GivenRowEntry::from(canonical.replace('"', ""))),
                        ("jur", GivenRowEntry::from(jurisdiction)),
                    ]),
                )
                .await?;
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
                self.run_write_given(
                    REGISTER_COMPANY_Q,
                    given_row(vec![
                        ("id", GivenRowEntry::from(id.0.to_string())),
                        ("name", GivenRowEntry::from(name.replace('"', ""))),
                        ("jur", GivenRowEntry::from(jurisdiction)),
                    ]),
                )
                .await?;
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
                let assertion_id = AssertionId::deterministic(
                    owner.entity(),
                    &format!("owns_{share_pct}"),
                    owned.entity(),
                    &bitemporal,
                    0,
                    &format!("{}@{}", provenance.source_id, provenance.observed_at.timestamp()),
                );
                self.run_write_given(
                    Self::ownership_insert_query(owner),
                    given_row(vec![
                        ("oid", GivenRowEntry::from(owner.entity().0.to_string())),
                        ("cid", GivenRowEntry::from(owned.0.to_string())),
                        ("aid", GivenRowEntry::from(assertion_id.0.to_string())),
                        ("sp", pct_entry(share_pct)),
                        ("ev", GivenRowEntry::from(Self::evidence_str(evidence))),
                        ("gov", GivenRowEntry::from(Self::governance_str(governance))),
                        ("ctx", GivenRowEntry::from(Self::context_str(context))),
                        ("role", GivenRowEntry::from(Self::role_str(role))),
                        ("jur", GivenRowEntry::from(jurisdiction)),
                        ("src", GivenRowEntry::from(provenance.source_id.replace('"', ""))),
                        ("auth", pct_entry(provenance.source_authority)),
                        ("vf", dt_entry(bitemporal.valid_from)),
                        ("vt", opt_dt_entry(bitemporal.valid_to)),
                        ("kf", dt_entry(bitemporal.known_from)),
                        ("kt", opt_dt_entry(bitemporal.known_to)),
                        ("obs", dt_entry(provenance.observed_at)),
                    ]),
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
                self.run_write_given(
                    IDENTITY_ALIAS_Q,
                    given_row(vec![
                        ("pid", GivenRowEntry::from(person_a.0.to_string())),
                        ("alias", GivenRowEntry::from(alias.replace('"', ""))),
                        ("canonical", GivenRowEntry::from(canonical.replace('"', ""))),
                        ("merge", GivenRowEntry::from(merge)),
                        ("ctx", GivenRowEntry::from(Self::context_str(context))),
                        ("at", dt_entry(at)),
                    ]),
                )
                .await?;
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
                self.run_write_given(
                    SANCTION_LISTING_Q,
                    given_row(vec![
                        ("pid", GivenRowEntry::from(person.0.to_string())),
                        ("list", GivenRowEntry::from(list_name.replace('"', ""))),
                        ("listed", GivenRowEntry::from(listed)),
                        ("ctx", GivenRowEntry::from(Self::context_str(context))),
                        ("vf", dt_entry(bitemporal.valid_from)),
                        ("vt", opt_dt_entry(bitemporal.valid_to)),
                        ("kf", dt_entry(bitemporal.known_from)),
                        ("kt", opt_dt_entry(bitemporal.known_to)),
                    ]),
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
                for (ev, disc) in [(supporting, 0u32), (refuting, 1u32)] {
                    let assertion_id = AssertionId::deterministic(
                        subject,
                        &predicate,
                        object,
                        &bitemporal,
                        disc,
                        &format!("{}@{}", provenance.source_id, provenance.observed_at.timestamp()),
                    );
                    self.run_write_given(
                        GENERIC_INSERT_Q,
                        given_row(vec![
                            ("sid", GivenRowEntry::from(subject.0.to_string())),
                            ("oid", GivenRowEntry::from(object.0.to_string())),
                            ("aid", GivenRowEntry::from(assertion_id.0.to_string())),
                            ("pred", GivenRowEntry::from(predicate.replace('"', ""))),
                            ("ev", GivenRowEntry::from(Self::evidence_str(ev))),
                            ("ctx", GivenRowEntry::from(Self::context_str(context))),
                            ("src", GivenRowEntry::from(provenance.source_id.replace('"', ""))),
                            ("auth", pct_entry(provenance.source_authority)),
                            ("vf", dt_entry(bitemporal.valid_from)),
                            ("vt", opt_dt_entry(bitemporal.valid_to)),
                            ("kf", dt_entry(bitemporal.known_from)),
                            ("kt", opt_dt_entry(bitemporal.known_to)),
                            ("obs", dt_entry(provenance.observed_at)),
                        ]),
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
                    self.run_write_given(
                        &OWNERSHIP_INSERT_ANY_Q,
                        given_row(vec![
                            ("oid", GivenRowEntry::from(subject.0.to_string())),
                            ("cid", GivenRowEntry::from(object.0.to_string())),
                            ("aid", GivenRowEntry::from(assertion_id.0.to_string())),
                            ("sp", pct_entry(share_pct)),
                            ("ev", GivenRowEntry::from(Self::evidence_str(evidence))),
                            ("gov", GivenRowEntry::from("OBSERVED")),
                            ("ctx", GivenRowEntry::from(Self::context_str(context))),
                            ("role", GivenRowEntry::from("SHAREHOLDER")),
                            ("jur", GivenRowEntry::from("GLOBAL")),
                            ("src", GivenRowEntry::from(provenance.source_id.replace('"', ""))),
                            ("auth", pct_entry(provenance.source_authority)),
                            ("vf", dt_entry(bitemporal.valid_from)),
                            ("vt", opt_dt_entry(bitemporal.valid_to)),
                            ("kf", dt_entry(bitemporal.known_from)),
                            ("kt", opt_dt_entry(bitemporal.known_to)),
                            ("obs", dt_entry(provenance.observed_at)),
                        ]),
                    )
                    .await?;
                } else {
                    self.run_write_given(
                        GENERIC_INSERT_Q,
                        given_row(vec![
                            ("sid", GivenRowEntry::from(subject.0.to_string())),
                            ("oid", GivenRowEntry::from(object.0.to_string())),
                            ("aid", GivenRowEntry::from(assertion_id.0.to_string())),
                            ("pred", GivenRowEntry::from(predicate.replace('"', ""))),
                            ("ev", GivenRowEntry::from(Self::evidence_str(evidence))),
                            ("ctx", GivenRowEntry::from(Self::context_str(context))),
                            ("src", GivenRowEntry::from(provenance.source_id.replace('"', ""))),
                            ("auth", pct_entry(provenance.source_authority)),
                            ("vf", dt_entry(bitemporal.valid_from)),
                            ("vt", opt_dt_entry(bitemporal.valid_to)),
                            ("kf", dt_entry(bitemporal.known_from)),
                            ("kt", opt_dt_entry(bitemporal.known_to)),
                            ("obs", dt_entry(provenance.observed_at)),
                        ]),
                    )
                    .await?;
                }
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::ComplianceRule {
                rule_id,
                threshold_pct,
                ..
            } => {
                self.run_write_given(
                    COMPLIANCE_RULE_Q,
                    given_row(vec![
                        ("rid", GivenRowEntry::from(rule_id.replace('"', ""))),
                        ("threshold", pct_entry(threshold_pct)),
                    ]),
                )
                .await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::CloseAssertionKnowledge {
                assertion_id,
                known_to,
            } => {
                self.run_write_given(
                    CLOSE_KNOWLEDGE_Q,
                    given_row(vec![
                        ("id", GivenRowEntry::from(assertion_id.0.to_string())),
                        ("kt", dt_entry(known_to)),
                    ]),
                )
                .await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::RetroactiveCorrection {
                assertion_id,
                new_valid_from,
                corrected_at,
            } => {
                self.retroactive_correct(assertion_id, new_valid_from, corrected_at)
                    .await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
            Event::RegisterTrust {
                id,
                name,
                jurisdiction,
                ..
            } => {
                self.run_write_given(
                    REGISTER_TRUST_Q,
                    given_row(vec![
                        ("id", GivenRowEntry::from(id.0.to_string())),
                        ("name", GivenRowEntry::from(name.replace('"', ""))),
                        ("jur", GivenRowEntry::from(jurisdiction)),
                    ]),
                )
                .await?;
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
                self.run_write_given(
                    CONTROL_NOMINEE_Q,
                    given_row(vec![
                        ("controller", GivenRowEntry::from(controller.0.to_string())),
                        ("controlled", GivenRowEntry::from(controlled.0.to_string())),
                        ("nominee", GivenRowEntry::from(nominee.0.to_string())),
                        ("instrument", GivenRowEntry::from(instrument.0.to_string())),
                        ("ctx", GivenRowEntry::from(Self::context_str(context))),
                        ("jur", GivenRowEntry::from(jurisdiction)),
                        ("src", GivenRowEntry::from(provenance.source_id.replace('"', ""))),
                        ("auth", pct_entry(provenance.source_authority)),
                        ("obs", dt_entry(provenance.observed_at)),
                        ("vf", dt_entry(bitemporal.valid_from)),
                        ("vt", opt_dt_entry(bitemporal.valid_to)),
                        ("kf", dt_entry(bitemporal.known_from)),
                        ("kt", opt_dt_entry(bitemporal.known_to)),
                    ]),
                )
                .await?;
                delta.physical_mutations = 1;
                delta.semantic_changes = 1;
            }
        }

        self.churn.physical_mutations += delta.physical_mutations;
        self.churn.semantic_changes += delta.semantic_changes;
        Ok(delta)
    }
}
