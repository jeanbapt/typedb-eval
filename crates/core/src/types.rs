use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntityId(pub Uuid);

impl EntityId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }
}

impl Default for EntityId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PersonId(pub Uuid);

impl PersonId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }

    pub fn entity(self) -> EntityId {
        EntityId(self.0)
    }
}

impl Default for PersonId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompanyId(pub Uuid);

impl CompanyId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }

    pub fn entity(self) -> EntityId {
        EntityId(self.0)
    }
}

impl Default for CompanyId {
    fn default() -> Self {
        Self::new()
    }
}

/// Introduced by the ontology extension (schema-evolution experiment). A trust is a
/// third party kind that can fill the same roles as a person or a company.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrustId(pub Uuid);

impl TrustId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }

    pub fn entity(self) -> EntityId {
        EntityId(self.0)
    }
}

impl Default for TrustId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssertionId(pub Uuid);

impl AssertionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AssertionId {
    fn default() -> Self {
        Self::new()
    }
}

pub type Timestamp = DateTime<Utc>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Context {
    CorporateRegistry,
    Kyc,
    Sanctions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Role {
    BeneficialOwner,
    Director,
    Shareholder,
    Controller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Decision {
    Allow,
    Review,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Compatibility {
    Consistent,
    Contradictory,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IdentityAction {
    Merge,
    KeepSeparate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Scale {
    S,
    M,
    L,
}

impl Scale {
    pub fn event_count(self) -> usize {
        match self {
            Scale::S => 1_000,
            Scale::M => 20_000,
            Scale::L => 200_000,
        }
    }
}

/// Which ontology the fixtures and stores are running against.
///
/// The schema-evolution experiment runs the *same* query implementations against both
/// generations. `Extended` adds a `trust` party kind and a 4-ary `control-via-nominee`
/// relation; nothing else changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OntologyGeneration {
    Base,
    Extended,
}

impl OntologyGeneration {
    pub fn is_extended(self) -> bool {
        matches!(self, OntologyGeneration::Extended)
    }

    pub fn name(self) -> &'static str {
        match self {
            OntologyGeneration::Base => "base",
            OntologyGeneration::Extended => "extended",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum QueryFamily {
    Q1BeneficialOwner,
    Q2BitemporalLookup,
    Q3Contradictions,
    Q4IdentityDiscrimination,
    Q5OwnershipExposure,
    Q6ContextCompatibility,
    Q7HistoricalReplay,
    Q8RetrospectiveView,
    Q9RoleAgnosticTraversal,
}

impl QueryFamily {
    pub fn all() -> &'static [QueryFamily] {
        &[
            QueryFamily::Q1BeneficialOwner,
            QueryFamily::Q2BitemporalLookup,
            QueryFamily::Q3Contradictions,
            QueryFamily::Q4IdentityDiscrimination,
            QueryFamily::Q5OwnershipExposure,
            QueryFamily::Q6ContextCompatibility,
            QueryFamily::Q7HistoricalReplay,
            QueryFamily::Q8RetrospectiveView,
            QueryFamily::Q9RoleAgnosticTraversal,
        ]
    }
}

/// Bitemporal bounds for an assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bitemporal {
    pub valid_from: Timestamp,
    pub valid_to: Option<Timestamp>,
    pub known_from: Timestamp,
    pub known_to: Option<Timestamp>,
}

impl Bitemporal {
    pub fn valid_at(&self, t: Timestamp) -> bool {
        t >= self.valid_from && self.valid_to.map_or(true, |to| t < to)
    }

    pub fn known_at(&self, t: Timestamp) -> bool {
        t >= self.known_from && self.known_to.map_or(true, |to| t < to)
    }

    pub fn visible_at(&self, valid_at: Timestamp, known_at: Timestamp) -> bool {
        self.valid_at(valid_at) && self.known_at(known_at)
    }
}

/// Provenance metadata — not an algebraic dimension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source_id: String,
    pub source_authority: f32,
    pub observed_at: Timestamp,
}

/// A semantic assertion with all dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    pub id: AssertionId,
    pub subject: EntityId,
    pub predicate: String,
    pub object: EntityId,
    pub evidence: crate::EvidenceState,
    pub governance: crate::GovernanceLevel,
    pub context: Context,
    pub role: Option<Role>,
    pub jurisdiction: String,
    pub provenance: Provenance,
    pub bitemporal: Bitemporal,
}

/// Entity state snapshot at a point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityState {
    pub entity: EntityId,
    pub assertions: Vec<Assertion>,
    pub beneficial_owners: Vec<PersonId>,
    pub sanctioned: bool,
}

/// A conflict between two assertions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub assertion_a: AssertionId,
    pub assertion_b: AssertionId,
    pub reason: String,
}

/// Exposure result for ownership traversal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exposure {
    pub entity: EntityId,
    pub direct: bool,
    pub indirect: bool,
    pub path: Vec<EntityId>,
    pub sanctioned_controller: Option<PersonId>,
}

/// One (relation type, role played, counterparty) triple.
///
/// Flattened rather than grouped per relation instance: TypeDB's role-agnostic traversal
/// cannot cheaply expose a stable relation identity, so grouping by instance would not be
/// comparable across backends. `counterparty` is `None` for unary relations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NeighborEdge {
    /// Relation type name, normalised across backends (e.g. `ownership`).
    pub relation_type: String,
    /// Role filled by the probed entity in that relation.
    pub role: String,
    /// One other role player, or `None` when the relation is unary.
    pub counterparty: Option<EntityId>,
}

/// Result of Q9: every relation the probed entity participates in, regardless of
/// relation type or role. This is the query that a role-agnostic engine answers without
/// enumerating relation types, and that a relational engine must enumerate by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Neighborhood {
    pub entity: EntityId,
    pub edges: Vec<NeighborEdge>,
}

impl Neighborhood {
    pub fn new(entity: EntityId, mut edges: Vec<NeighborEdge>) -> Self {
        edges.sort();
        edges.dedup();
        Self { entity, edges }
    }

    /// Distinct relation types discovered. The headline schema-evolution metric: after the
    /// ontology grows, does an unmodified query still see every relation type?
    pub fn relation_types(&self) -> Vec<String> {
        let mut types: Vec<String> = self.edges.iter().map(|e| e.relation_type.clone()).collect();
        types.sort();
        types.dedup();
        types
    }
}

impl PartialOrd for EntityId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EntityId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

/// Delta returned after ingesting an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDelta {
    pub physical_mutations: u64,
    pub semantic_changes: u64,
}

/// Benchmark event types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RegisterPerson {
        id: PersonId,
        name: String,
        canonical_name: String,
        jurisdiction: String,
        context: Context,
        at: Timestamp,
    },
    RegisterCompany {
        id: CompanyId,
        name: String,
        jurisdiction: String,
        at: Timestamp,
    },
    AssertOwnership {
        owner: PersonId,
        owned: CompanyId,
        share_pct: f32,
        evidence: crate::EvidenceState,
        governance: crate::GovernanceLevel,
        context: Context,
        role: Role,
        jurisdiction: String,
        provenance: Provenance,
        bitemporal: Bitemporal,
    },
    IdentityAlias {
        person_a: PersonId,
        alias: String,
        canonical: String,
        merge: bool,
        context: Context,
        at: Timestamp,
    },
    SanctionListing {
        person: PersonId,
        list_name: String,
        listed: bool,
        context: Context,
        bitemporal: Bitemporal,
    },
    ContradictorySource {
        subject: EntityId,
        predicate: String,
        object: EntityId,
        supporting: crate::EvidenceState,
        refuting: crate::EvidenceState,
        context: Context,
        provenance: Provenance,
        bitemporal: Bitemporal,
    },
    RetroactiveCorrection {
        assertion_id: AssertionId,
        new_valid_from: Timestamp,
        corrected_at: Timestamp,
    },
    LateArrival {
        subject: EntityId,
        predicate: String,
        object: EntityId,
        evidence: crate::EvidenceState,
        context: Context,
        provenance: Provenance,
        bitemporal: Bitemporal,
    },
    ComplianceRule {
        rule_id: String,
        description: String,
        threshold_pct: f32,
    },

    // --- Ontology extension (only emitted under OntologyGeneration::Extended) ---
    RegisterTrust {
        id: TrustId,
        name: String,
        jurisdiction: String,
        at: Timestamp,
    },
    /// 4-ary relation with a polymorphic `controller` role: the controller may be a
    /// person, a company or a trust. This is the shape that a binary-relation schema
    /// cannot express without either a junction table or nullable foreign-key columns.
    ControlViaNominee {
        controller: EntityId,
        controlled: CompanyId,
        nominee: PersonId,
        instrument: TrustId,
        context: Context,
        jurisdiction: String,
        provenance: Provenance,
        bitemporal: Bitemporal,
    },
}

/// Query probe used by the benchmark runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryProbe {
    pub family: QueryFamily,
    pub entity: EntityId,
    pub valid_at: Timestamp,
    pub known_at: Timestamp,
    pub person_a: Option<PersonId>,
    pub person_b: Option<PersonId>,
}

/// Expected answer for a query probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "answer_type", rename_all = "snake_case")]
pub enum ExpectedAnswer {
    BeneficialOwners { owners: Vec<PersonId> },
    EntityState { state: EntityState },
    Conflicts { conflicts: Vec<Conflict> },
    IdentityAction { action: IdentityAction },
    Exposure { exposure: Exposure },
    Compatibility { result: Compatibility },
    Decision { decision: Decision },
    Neighborhood { neighborhood: Neighborhood },
}

/// Full fixture bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureBundle {
    pub seed: u64,
    pub scale: Scale,
    #[serde(default = "default_generation")]
    pub generation: OntologyGeneration,
    pub events: Vec<Event>,
    pub probes: Vec<QueryProbe>,
    pub expected: Vec<(QueryProbe, ExpectedAnswer)>,
}

fn default_generation() -> OntologyGeneration {
    OntologyGeneration::Base
}
