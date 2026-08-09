use std::collections::{HashMap, HashSet};

use chrono::{Duration, Utc};

use crate::error::Result;
use crate::lattice::{join_evidence, EvidenceState, GovernanceLevel};
use crate::types::*;

/// In-memory oracle that computes ground truth independently of backends.
#[derive(Debug, Default)]
pub struct Oracle {
    persons: HashMap<PersonId, PersonRecord>,
    companies: HashMap<CompanyId, CompanyRecord>,
    trusts: HashMap<TrustId, TrustRecord>,
    assertions: Vec<Assertion>,
    identity_links: Vec<IdentityLink>,
    sanction_listings: Vec<SanctionRecord>,
    controls: Vec<ControlRecord>,
    rules: Vec<ComplianceRuleRecord>,
    /// Pairs selected for Q4 probes (alias graph).
    identity_probe_pairs: Vec<(PersonId, PersonId)>,
    total_churn: StateDelta,
}

#[derive(Debug, Clone)]
struct TrustRecord {
    id: TrustId,
    name: String,
    jurisdiction: String,
}

#[derive(Debug, Clone)]
struct ControlRecord {
    controller: EntityId,
    controlled: CompanyId,
    nominee: PersonId,
    instrument: TrustId,
    bitemporal: Bitemporal,
}

#[derive(Debug, Clone)]
struct PersonRecord {
    id: PersonId,
    name: String,
    canonical_name: String,
    jurisdiction: String,
}

#[derive(Debug, Clone)]
struct CompanyRecord {
    id: CompanyId,
    name: String,
    jurisdiction: String,
}

#[derive(Debug, Clone)]
struct IdentityLink {
    person_a: PersonId,
    alias: String,
    canonical: String,
    merge: bool,
    context: Context,
    at: Timestamp,
}

#[derive(Debug, Clone)]
struct SanctionRecord {
    person: PersonId,
    list_name: String,
    listed: bool,
    context: Context,
    bitemporal: Bitemporal,
}

#[derive(Debug, Clone)]
struct ComplianceRuleRecord {
    rule_id: String,
    threshold_pct: f32,
}

impl Oracle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, event: Event) -> StateDelta {
        let mut delta = StateDelta::default();

        match event {
            Event::RegisterPerson {
                id,
                name,
                canonical_name,
                jurisdiction,
                ..
            } => {
                delta.physical_mutations += 1;
                if self
                    .persons
                    .insert(
                        id,
                        PersonRecord {
                            id,
                            name,
                            canonical_name,
                            jurisdiction,
                        },
                    )
                    .is_none()
                {
                    delta.semantic_changes += 1;
                }
            }
            Event::RegisterCompany {
                id, name, jurisdiction, ..
            } => {
                delta.physical_mutations += 1;
                if self
                    .companies
                    .insert(
                        id,
                        CompanyRecord {
                            id,
                            name,
                            jurisdiction,
                        },
                    )
                    .is_none()
                {
                    delta.semantic_changes += 1;
                }
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
                let assertion = Assertion {
                    id: AssertionId::new(),
                    subject: owner.entity(),
                    predicate: format!("owns_{share_pct}"),
                    object: owned.entity(),
                    evidence,
                    governance,
                    context,
                    role: Some(role),
                    jurisdiction,
                    provenance,
                    bitemporal,
                };
                delta.physical_mutations += 1;
                delta.semantic_changes += 1;
                self.assertions.push(assertion);
            }
            Event::IdentityAlias {
                person_a,
                alias,
                canonical,
                merge,
                context,
                at,
            } => {
                let is_duplicate = self.identity_links.iter().any(|l| {
                    l.person_a == person_a && l.alias == alias && l.canonical == canonical
                });
                delta.physical_mutations += 1;
                if !is_duplicate {
                    delta.semantic_changes += 1;
                }
                self.identity_links.push(IdentityLink {
                    person_a,
                    alias,
                    canonical,
                    merge,
                    context,
                    at,
                });
            }
            Event::SanctionListing {
                person,
                list_name,
                listed,
                context,
                bitemporal,
            } => {
                delta.physical_mutations += 1;
                delta.semantic_changes += 1;
                self.sanction_listings.push(SanctionRecord {
                    person,
                    list_name,
                    listed,
                    context,
                    bitemporal,
                });
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
                delta.physical_mutations += 2;
                delta.semantic_changes += 1;
                self.assertions.push(Assertion {
                    id: AssertionId::new(),
                    subject,
                    predicate: predicate.clone(),
                    object,
                    evidence: supporting,
                    governance: GovernanceLevel::Observed,
                    context,
                    role: None,
                    jurisdiction: "GLOBAL".into(),
                    provenance: provenance.clone(),
                    bitemporal: bitemporal.clone(),
                });
                self.assertions.push(Assertion {
                    id: AssertionId::new(),
                    subject,
                    predicate,
                    object,
                    evidence: refuting,
                    governance: GovernanceLevel::Observed,
                    context,
                    role: None,
                    jurisdiction: "GLOBAL".into(),
                    provenance,
                    bitemporal,
                });
            }
            Event::RetroactiveCorrection {
                assertion_id,
                new_valid_from,
                corrected_at,
            } => {
                delta.physical_mutations += 1;
                delta.semantic_changes += 1;
                if let Some(idx) = self.assertions.iter().position(|a| a.id == assertion_id) {
                    let old = self.assertions[idx].clone();
                    self.assertions[idx].bitemporal.known_to = Some(corrected_at);
                    self.assertions.push(Assertion {
                        id: AssertionId::new(),
                        subject: old.subject,
                        predicate: old.predicate,
                        object: old.object,
                        evidence: old.evidence,
                        governance: old.governance,
                        context: old.context,
                        role: old.role,
                        jurisdiction: old.jurisdiction,
                        provenance: old.provenance,
                        bitemporal: Bitemporal {
                            valid_from: new_valid_from,
                            valid_to: old.bitemporal.valid_to,
                            known_from: corrected_at,
                            known_to: None,
                        },
                    });
                }
            }
            Event::CloseAssertionKnowledge {
                assertion_id,
                known_to,
            } => {
                delta.physical_mutations += 1;
                if let Some(a) = self
                    .assertions
                    .iter_mut()
                    .find(|a| a.id == assertion_id)
                {
                    a.bitemporal.known_to = Some(known_to);
                }
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
                delta.physical_mutations += 1;
                delta.semantic_changes += 1;
                self.assertions.push(Assertion {
                    id: AssertionId::new(),
                    subject,
                    predicate,
                    object,
                    evidence,
                    governance: GovernanceLevel::Observed,
                    context,
                    role: None,
                    jurisdiction: "GLOBAL".into(),
                    provenance,
                    bitemporal,
                });
            }
            Event::ComplianceRule {
                rule_id,
                threshold_pct,
                ..
            } => {
                delta.physical_mutations += 1;
                delta.semantic_changes += 1;
                self.rules.push(ComplianceRuleRecord {
                    rule_id,
                    threshold_pct,
                });
            }
            Event::RegisterTrust {
                id,
                name,
                jurisdiction,
                ..
            } => {
                delta.physical_mutations += 1;
                if self
                    .trusts
                    .insert(id, TrustRecord { id, name, jurisdiction })
                    .is_none()
                {
                    delta.semantic_changes += 1;
                }
            }
            Event::ControlViaNominee {
                controller,
                controlled,
                nominee,
                instrument,
                bitemporal,
                ..
            } => {
                delta.physical_mutations += 1;
                delta.semantic_changes += 1;
                self.controls.push(ControlRecord {
                    controller,
                    controlled,
                    nominee,
                    instrument,
                    bitemporal,
                });
            }
        }

        self.total_churn.physical_mutations += delta.physical_mutations;
        self.total_churn.semantic_changes += delta.semantic_changes;
        delta
    }

    pub fn register_identity_probe_pair(&mut self, a: PersonId, b: PersonId) {
        if !self
            .identity_probe_pairs
            .iter()
            .any(|&(x, y)| x == a && y == b)
        {
            self.identity_probe_pairs.push((a, b));
        }
    }

    pub fn total_churn(&self) -> &StateDelta {
        &self.total_churn
    }

    pub fn assertion_ids(&self) -> Vec<AssertionId> {
        self.assertions.iter().map(|a| a.id).collect()
    }

    pub fn visible_assertions(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Vec<&Assertion> {
        self.assertions
            .iter()
            .filter(|a| {
                (a.subject == entity || a.object == entity)
                    && a.bitemporal.visible_at(valid_at, known_at)
            })
            .collect()
    }

    fn owners_of(&self, entity: EntityId, valid_at: Timestamp, known_at: Timestamp) -> Vec<EntityId> {
        self.assertions
            .iter()
            .filter(|a| {
                a.object == entity
                    && a.predicate.starts_with("owns_")
                    && a.evidence != EvidenceState::Refuted
                    && a.bitemporal.visible_at(valid_at, known_at)
            })
            .map(|a| a.subject)
            .collect()
    }

    fn collect_person_owners(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
        visited: &mut HashSet<EntityId>,
        out: &mut HashSet<PersonId>,
    ) {
        if !visited.insert(entity) {
            return;
        }
        for owner in self.owners_of(entity, valid_at, known_at) {
            if let Some(p) = self.persons.keys().find(|p| p.entity() == owner) {
                out.insert(*p);
            } else if self.companies.contains_key(&CompanyId(owner.0)) {
                self.collect_person_owners(owner, valid_at, known_at, visited, out);
            }
        }
    }

    pub fn neighborhood(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Neighborhood {
        let mut edges = Vec::new();

        for a in self.assertions.iter() {
            if !a.bitemporal.visible_at(valid_at, known_at) {
                continue;
            }
            let relation_type = if a.predicate.starts_with("owns_") {
                "ownership"
            } else {
                "generic-assertion"
            };
            let (subject_role, object_role) = if relation_type == "ownership" {
                ("owner", "owned")
            } else {
                ("subject", "object")
            };
            if a.subject == entity {
                edges.push(NeighborEdge {
                    relation_type: relation_type.into(),
                    role: subject_role.into(),
                    counterparty: Some(a.object),
                });
            }
            if a.object == entity {
                edges.push(NeighborEdge {
                    relation_type: relation_type.into(),
                    role: object_role.into(),
                    counterparty: Some(a.subject),
                });
            }
        }

        for s in self.sanction_listings.iter() {
            if s.person.entity() == entity && s.bitemporal.visible_at(valid_at, known_at) {
                edges.push(NeighborEdge {
                    relation_type: "sanction-listing".into(),
                    role: "sanctioned-person".into(),
                    counterparty: None,
                });
            }
        }

        for c in self.controls.iter() {
            if !c.bitemporal.visible_at(valid_at, known_at) {
                continue;
            }
            let players = [
                ("controller", c.controller),
                ("controlled", c.controlled.entity()),
                ("nominee", c.nominee.entity()),
                ("instrument", c.instrument.entity()),
            ];
            for (role, player) in players.iter() {
                if *player != entity {
                    continue;
                }
                for (other_role, other) in players.iter() {
                    if other_role != role {
                        edges.push(NeighborEdge {
                            relation_type: "control-via-nominee".into(),
                            role: (*role).into(),
                            counterparty: Some(*other),
                        });
                    }
                }
            }
        }

        Neighborhood::new(entity, edges)
    }

    pub fn beneficial_owners(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Vec<PersonId> {
        let mut visited = HashSet::new();
        let mut owners = HashSet::new();
        self.collect_person_owners(entity, valid_at, known_at, &mut visited, &mut owners);
        let mut v: Vec<_> = owners.into_iter().collect();
        v.sort_by_key(|p| p.0);
        v
    }

    fn ownership_path(
        &self,
        from: EntityId,
        to: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> bool {
        if from == to {
            return true;
        }
        let mut visited = HashSet::new();
        let mut stack = vec![from];
        while let Some(current) = stack.pop() {
            if current == to {
                return true;
            }
            if !visited.insert(current) {
                continue;
            }
            for owner in self.owners_of(current, valid_at, known_at) {
                stack.push(owner);
            }
        }
        false
    }

    pub fn state_at(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<EntityState> {
        let assertions: Vec<_> = self
            .visible_assertions(entity, valid_at, known_at)
            .into_iter()
            .cloned()
            .collect();
        let beneficial_owners = self.beneficial_owners(entity, valid_at, known_at);
        let sanctioned = self.is_sanctioned_entity(entity, valid_at, known_at);
        Ok(EntityState {
            entity,
            assertions,
            beneficial_owners,
            sanctioned,
        })
    }

    fn is_sanctioned_entity(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> bool {
        for p in self.persons.keys() {
            if p.entity() == entity {
                return self.is_person_sanctioned(*p, valid_at, known_at);
            }
        }
        false
    }

    fn is_person_sanctioned(
        &self,
        person: PersonId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> bool {
        self.sanction_listings
            .iter()
            .filter(|s| s.person == person && s.bitemporal.visible_at(valid_at, known_at))
            .map(|s| s.listed)
            .last()
            .unwrap_or(false)
    }

    pub fn contradictions(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<Conflict>> {
        let visible: Vec<_> = self
            .visible_assertions(entity, valid_at, known_at)
            .into_iter()
            .cloned()
            .collect();
        let mut conflicts = Vec::new();
        for i in 0..visible.len() {
            for j in (i + 1)..visible.len() {
                let a = &visible[i];
                let b = &visible[j];
                if a.predicate == b.predicate
                    && a.object == b.object
                    && a.bitemporal.valid_from <= b.bitemporal.valid_from
                {
                    let joined = join_evidence(a.evidence, b.evidence);
                    if joined == EvidenceState::Contradictory {
                        conflicts.push(Conflict {
                            assertion_a: a.id,
                            assertion_b: b.id,
                            reason: "evidence_contradiction".into(),
                        });
                    }
                }
            }
        }
        Ok(conflicts)
    }

    pub fn ownership_exposure(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Exposure> {
        let direct_owners = self.owners_of(entity, valid_at, known_at);
        let direct = !direct_owners.is_empty();
        let mut path: Vec<EntityId> = direct_owners.clone();
        let mut sanctioned_controller = None;

        for owner in &direct_owners {
            if let Some(p) = self.persons.keys().find(|p| p.entity() == *owner) {
                if self.is_person_sanctioned(*p, valid_at, known_at) {
                    sanctioned_controller = Some(*p);
                }
            }
        }

        let mut indirect = false;
        for company in self.companies.keys() {
            let ce = company.entity();
            if ce == entity {
                continue;
            }
            if self.ownership_path(ce, entity, valid_at, known_at) {
                for owner in self.owners_of(ce, valid_at, known_at) {
                    indirect = true;
                    path.push(owner);
                    if let Some(p) = self.persons.keys().find(|p| p.entity() == owner) {
                        if self.is_person_sanctioned(*p, valid_at, known_at) {
                            sanctioned_controller = Some(*p);
                        }
                    }
                }
            }
        }

        path.sort();
        path.dedup();

        Ok(Exposure {
            entity,
            direct,
            indirect,
            path,
            sanctioned_controller,
        })
    }

    pub fn compliance_decision(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Decision> {
        let exposure = self.ownership_exposure(entity, valid_at, known_at)?;
        let conflicts = self.contradictions(entity, valid_at, known_at)?;

        if exposure.sanctioned_controller.is_some() {
            return Ok(Decision::Block);
        }
        if !conflicts.is_empty() {
            return Ok(Decision::Review);
        }
        let threshold = self
            .rules
            .first()
            .map(|r| r.threshold_pct)
            .unwrap_or(25.0);
        let owners = self.beneficial_owners(entity, valid_at, known_at);
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

    pub fn identity_action(
        &self,
        person_a: PersonId,
        person_b: PersonId,
        _valid_at: Timestamp,
        _known_at: Timestamp,
    ) -> Result<IdentityAction> {
        let canon_a = self
            .persons
            .get(&person_a)
            .map(|p| p.canonical_name.as_str())
            .unwrap_or("");
        let canon_b = self
            .persons
            .get(&person_b)
            .map(|p| p.canonical_name.as_str())
            .unwrap_or("");

        let should_merge = self.identity_links.iter().any(|l| {
            l.merge && (l.person_a == person_a || l.person_a == person_b)
        }) || (canon_a == canon_b && !canon_a.is_empty());

        Ok(if should_merge {
            IdentityAction::Merge
        } else {
            IdentityAction::KeepSeparate
        })
    }

    pub fn context_compatibility(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Compatibility> {
        let contexts = Context::base_contexts();
        let mut states: HashMap<Context, Vec<&Assertion>> = HashMap::new();
        for ctx in contexts {
            let assertions: Vec<_> = self
                .visible_assertions(entity, valid_at, known_at)
                .into_iter()
                .filter(|a| a.context == *ctx)
                .collect();
            states.insert(*ctx, assertions);
        }

        let mut has_contradiction = false;
        let mut has_data = false;

        for ctx_assertions in states.values() {
            if !ctx_assertions.is_empty() {
                has_data = true;
            }
            for i in 0..ctx_assertions.len() {
                for j in (i + 1)..ctx_assertions.len() {
                    if join_evidence(ctx_assertions[i].evidence, ctx_assertions[j].evidence)
                        == EvidenceState::Contradictory
                    {
                        has_contradiction = true;
                    }
                }
            }
        }

        let cr = states.get(&Context::CorporateRegistry).cloned().unwrap_or_default();
        let kyc = states.get(&Context::Kyc).cloned().unwrap_or_default();
        let sanctions = states.get(&Context::Sanctions).cloned().unwrap_or_default();

        for a in &cr {
            for b in &kyc {
                if a.predicate == b.predicate
                    && join_evidence(a.evidence, b.evidence) == EvidenceState::Contradictory
                {
                    has_contradiction = true;
                }
            }
        }
        for a in &sanctions {
            if a.evidence == EvidenceState::Supported {
                has_contradiction = true;
            }
        }

        if !has_data {
            Ok(Compatibility::Indeterminate)
        } else if has_contradiction {
            Ok(Compatibility::Contradictory)
        } else {
            Ok(Compatibility::Consistent)
        }
    }

    pub fn answer_probe(&self, probe: &QueryProbe) -> Result<ExpectedAnswer> {
        match probe.family {
            QueryFamily::Q1BeneficialOwner => Ok(ExpectedAnswer::BeneficialOwners {
                owners: self.beneficial_owners(probe.entity, probe.valid_at, probe.known_at),
            }),
            QueryFamily::Q2BitemporalLookup => Ok(ExpectedAnswer::EntityState {
                state: self.state_at(probe.entity, probe.valid_at, probe.known_at)?,
            }),
            QueryFamily::Q3Contradictions => Ok(ExpectedAnswer::Conflicts {
                conflicts: self.contradictions(probe.entity, probe.valid_at, probe.known_at)?,
            }),
            QueryFamily::Q4IdentityDiscrimination => {
                let a = probe.person_a.ok_or_else(|| {
                    crate::BenchmarkError::InvalidEvent("missing person_a".into())
                })?;
                let b = probe.person_b.ok_or_else(|| {
                    crate::BenchmarkError::InvalidEvent("missing person_b".into())
                })?;
                Ok(ExpectedAnswer::IdentityAction {
                    action: self.identity_action(a, b, probe.valid_at, probe.known_at)?,
                })
            }
            QueryFamily::Q5OwnershipExposure => Ok(ExpectedAnswer::Exposure {
                exposure: self.ownership_exposure(probe.entity, probe.valid_at, probe.known_at)?,
            }),
            QueryFamily::Q6ContextCompatibility => Ok(ExpectedAnswer::Compatibility {
                result: self.context_compatibility(probe.entity, probe.valid_at, probe.known_at)?,
            }),
            QueryFamily::Q7HistoricalReplay | QueryFamily::Q8RetrospectiveView => {
                Ok(ExpectedAnswer::Decision {
                    decision: self.compliance_decision(
                        probe.entity,
                        probe.valid_at,
                        probe.known_at,
                    )?,
                })
            }
            QueryFamily::Q9RoleAgnosticTraversal => Ok(ExpectedAnswer::Neighborhood {
                neighborhood: self.neighborhood(probe.entity, probe.valid_at, probe.known_at),
            }),
        }
    }

    pub fn build_expected(_probes: &[QueryProbe]) -> Result<Vec<(QueryProbe, ExpectedAnswer)>> {
        Ok(vec![])
    }
}

impl Oracle {
    pub fn from_events(events: &[Event]) -> Self {
        let mut oracle = Oracle::new();
        for event in events {
            oracle.ingest(event.clone());
        }
        oracle
    }

    /// Anchor probe times to the fixture epoch so closed knowledge windows matter.
    pub fn fixture_epoch(&self) -> Timestamp {
        self.assertions
            .first()
            .map(|a| a.bitemporal.valid_from)
            .unwrap_or_else(|| Utc::now() - Duration::days(365))
    }

    pub fn generate_probes(&self, count: usize) -> Vec<QueryProbe> {
        let epoch = self.fixture_epoch();
        let valid_at = epoch + Duration::days(200);
        let known_historical = epoch + Duration::days(250);
        let known_current = epoch + Duration::days(400);

        let mut probes = Vec::new();

        let mut companies: Vec<EntityId> = self.companies.values().map(|c| c.id.entity()).collect();
        companies.sort();
        let mut person_entities: Vec<EntityId> =
            self.persons.values().map(|p| p.id.entity()).collect();
        person_entities.sort();

        let entities: Vec<EntityId> = companies
            .into_iter()
            .chain(person_entities)
            .take(count)
            .collect();

        let q4_pairs: Vec<(PersonId, PersonId)> = if self.identity_probe_pairs.is_empty() {
            let mut persons: Vec<PersonId> = self.persons.keys().copied().collect();
            persons.sort_by_key(|p| p.0);
            persons
                .windows(2)
                .map(|w| (w[0], w[1]))
                .take(count)
                .collect()
        } else {
            self.identity_probe_pairs.clone()
        };

        for (i, entity) in entities.iter().enumerate() {
            for family in QueryFamily::all() {
                let mut probe = QueryProbe {
                    family: *family,
                    entity: *entity,
                    valid_at,
                    known_at: known_historical,
                    person_a: None,
                    person_b: None,
                };

                if *family == QueryFamily::Q4IdentityDiscrimination {
                    if let Some((a, b)) = q4_pairs.get(i % q4_pairs.len()) {
                        probe.person_a = Some(*a);
                        probe.person_b = Some(*b);
                    }
                }
                if *family == QueryFamily::Q8RetrospectiveView {
                    probe.known_at = known_current;
                }

                probes.push(probe);
            }
        }
        probes
    }

    pub fn compute_expected(&self, probes: &[QueryProbe]) -> Result<Vec<(QueryProbe, ExpectedAnswer)>> {
        probes
            .iter()
            .map(|p| {
                let answer = self.answer_probe(p)?;
                Ok((p.clone(), answer))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lattice::EvidenceState;
    use chrono::TimeZone;

    fn ts(days: i64) -> Timestamp {
        Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap() + Duration::days(days)
    }

    #[test]
    fn oracle_beneficial_owner_deep_chain() {
        let person = PersonId::new();
        let mid = CompanyId::new();
        let target = CompanyId::new();
        let mut oracle = Oracle::new();
        oracle.ingest(Event::RegisterPerson {
            id: person,
            name: "John Smith".into(),
            canonical_name: "john smith".into(),
            jurisdiction: "US".into(),
            context: Context::Kyc,
            at: ts(0),
        });
        for (c, name) in [(mid, "MidCo"), (target, "TargetCo")] {
            oracle.ingest(Event::RegisterCompany {
                id: c,
                name: name.into(),
                jurisdiction: "UK".into(),
                at: ts(0),
            });
        }
        for (owner, owned) in [
            (PartyId::Person(person), mid),
            (PartyId::Company(mid), target),
        ] {
            oracle.ingest(Event::AssertOwnership {
                owner,
                owned,
                share_pct: 51.0,
                evidence: EvidenceState::Supported,
                governance: GovernanceLevel::Reviewed,
                context: Context::CorporateRegistry,
                role: Role::BeneficialOwner,
                jurisdiction: "UK".into(),
                provenance: Provenance {
                    source_id: "reg1".into(),
                    source_authority: 0.9,
                    observed_at: ts(10),
                },
                bitemporal: Bitemporal {
                    valid_from: ts(20),
                    valid_to: None,
                    known_from: ts(10),
                    known_to: None,
                },
            });
        }
        let owners = oracle.beneficial_owners(target.entity(), ts(30), ts(50));
        assert_eq!(owners, vec![person]);
    }

    #[test]
    fn q7_q8_can_diverge_with_closed_knowledge() {
        let owner = PersonId::new();
        let company = CompanyId::new();
        let mut oracle = Oracle::new();
        oracle.ingest(Event::RegisterPerson {
            id: owner,
            name: "Jane Doe".into(),
            canonical_name: "jane doe".into(),
            jurisdiction: "US".into(),
            context: Context::Kyc,
            at: ts(0),
        });
        oracle.ingest(Event::RegisterCompany {
            id: company,
            name: "Acme".into(),
            jurisdiction: "UK".into(),
            at: ts(0),
        });
        oracle.ingest(Event::ComplianceRule {
            rule_id: "r0".into(),
            description: "t".into(),
            threshold_pct: 25.0,
        });
        oracle.ingest(Event::AssertOwnership {
            owner: PartyId::Person(owner),
            owned: company,
            share_pct: 51.0,
            evidence: EvidenceState::Supported,
            governance: GovernanceLevel::Reviewed,
            context: Context::CorporateRegistry,
            role: Role::BeneficialOwner,
            jurisdiction: "UK".into(),
            provenance: Provenance {
                source_id: "reg1".into(),
                source_authority: 0.9,
                observed_at: ts(10),
            },
            bitemporal: Bitemporal {
                valid_from: ts(20),
                valid_to: None,
                known_from: ts(10),
                known_to: None,
            },
        });
        oracle.ingest(Event::SanctionListing {
            person: owner,
            list_name: "OFAC".into(),
            listed: true,
            context: Context::Sanctions,
            bitemporal: Bitemporal {
                valid_from: ts(50),
                valid_to: None,
                known_from: ts(300),
                known_to: None,
            },
        });
        let d7 = oracle
            .compliance_decision(company.entity(), ts(100), ts(200))
            .unwrap();
        let d8 = oracle
            .compliance_decision(company.entity(), ts(100), ts(350))
            .unwrap();
        assert_eq!(d7, Decision::Allow);
        assert_eq!(d8, Decision::Block);
    }
}
