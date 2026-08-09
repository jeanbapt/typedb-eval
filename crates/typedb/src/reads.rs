use std::collections::{HashMap, HashSet};
use futures::StreamExt;
use typedb_driver::{answer::QueryAnswer, concept::Concept, TransactionType, TypeDBDriver};

use benchmark_core::error::Result;
use benchmark_core::{
    AssertionId, Compatibility, Conflict, Context, Decision, EntityId, EntityState, EvidenceState,
    Exposure, IdentityAction, NeighborEdge, Neighborhood, PersonId, Timestamp,
};
use benchmark_core::lattice::join_evidence;
use uuid::Uuid;

/// Wire format for every datetime that crosses the TypeQL boundary, in both directions.
const DATETIME_FMT: &str = "%Y-%m-%dT%H:%M:%S";

#[derive(Clone, Debug)]
struct ActiveOwnership {
    owner: EntityId,
    owned: EntityId,
    evidence: EvidenceState,
}

#[derive(Clone, Debug)]
struct VisibleAssertion {
    id: AssertionId,
    subject: EntityId,
    predicate: String,
    object: EntityId,
    evidence: EvidenceState,
    context: Context,
    valid_from: Timestamp,
}

pub struct TypeDbReads<'a> {
    pub driver: &'a TypeDBDriver,
    pub database: &'a str,
}

impl<'a> TypeDbReads<'a> {
    pub fn dt(t: Timestamp) -> String {
        t.format(DATETIME_FMT).to_string()
    }

    pub fn bitemporal_start(rel: &str, valid: &str, known: &str, suffix: &str) -> String {
        format!(
            r#"{rel} has valid-from $vf{suffix}, has known-from $kf{suffix};
                $vf{suffix} <= {valid};
                $kf{suffix} <= {known};
                try {{ {rel} has valid-to $vt{suffix}; }};
                try {{ {rel} has known-to $kt{suffix}; }};"#
        )
    }

    pub fn bitemporal_start_active(rel: &str, valid: &str, known: &str, suffix: &str) -> String {
        format!(
            r#"{rel} has valid-from $vf{suffix}, has known-from $kf{suffix}, has evidence-state $ev{suffix};
                $vf{suffix} <= {valid};
                $kf{suffix} <= {known};
                not {{ $ev{suffix} == "REFUTED"; }};
                try {{ {rel} has valid-to $vt{suffix}; }};
                try {{ {rel} has known-to $kt{suffix}; }};"#
        )
    }

    fn parse_dt(s: &str) -> Option<chrono::NaiveDateTime> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok()
    }

    fn interval_open(end: Option<&str>, at: &str) -> bool {
        match end {
            None | Some("") => true,
            Some(end) => Self::parse_dt(end)
                .zip(Self::parse_dt(at))
                .map(|(e, a)| e > a)
                .unwrap_or_else(|| {
                    // Falling back to "open" here is what previously made every closed
                    // interval invisible to the filter; keep the lenient behaviour but
                    // make it impossible for it to happen silently again.
                    tracing::warn!(%end, %at, "unparseable bitemporal bound, treating as open");
                    true
                }),
        }
    }

    fn row_is_active(
        _vf: &str,
        vt: &str,
        _kf: &str,
        kt: &str,
        ev: Option<&str>,
        valid: &str,
        known: &str,
    ) -> bool {
        if ev == Some("REFUTED") {
            return false;
        }
        Self::interval_open(if vt.is_empty() { None } else { Some(vt) }, valid)
            && Self::interval_open(if kt.is_empty() { None } else { Some(kt) }, known)
    }

    fn parse_evidence(s: &str) -> EvidenceState {
        match s {
            "SUPPORTED" => EvidenceState::Supported,
            "REFUTED" => EvidenceState::Refuted,
            "CONTRADICTORY" => EvidenceState::Contradictory,
            _ => EvidenceState::Unknown,
        }
    }

    fn parse_row_timestamp(s: &str) -> Option<Timestamp> {
        if s.is_empty() {
            return None;
        }
        chrono::NaiveDateTime::parse_from_str(s, DATETIME_FMT)
            .ok()
            .map(|t| t.and_utc())
            .or_else(|| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|t| t.with_timezone(&chrono::Utc))
            })
    }

    async fn party_sets(&self) -> Result<(HashSet<EntityId>, HashSet<EntityId>)> {
        let mut persons = HashSet::new();
        for row in self
            .collect_named_rows(r#"match $p isa person, has entity-id $id; select $id;"#)
            .await?
        {
            if let Some(id) = row.get("id").and_then(|s| Uuid::parse_str(s).ok()) {
                persons.insert(EntityId(id));
            }
        }
        let mut companies = HashSet::new();
        for row in self
            .collect_named_rows(r#"match $c isa company, has entity-id $id; select $id;"#)
            .await?
        {
            if let Some(id) = row.get("id").and_then(|s| Uuid::parse_str(s).ok()) {
                companies.insert(EntityId(id));
            }
        }
        Ok((persons, companies))
    }

    async fn active_ownerships(
        &self,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<ActiveOwnership>> {
        let valid = Self::dt(valid_at);
        let known = Self::dt(known_at);
        let query = format!(
            r#"match
                $o isa ownership, links (owner: $owner, owned: $owned);
                $owner has entity-id $oid;
                $owned has entity-id $ownedid;
                $o has valid-from $vf, has known-from $kf, has evidence-state $ev;
                $vf <= {valid};
                $kf <= {known};
                try {{ $o has valid-to $vt; }};
                try {{ $o has known-to $kt; }};
            select $oid, $ownedid, $ev, $vf, $vt, $kf, $kt;"#,
            valid = valid,
            known = known,
        );
        Ok(self
            .collect_named_rows(&query)
            .await?
            .into_iter()
            .filter(|row| {
                Self::row_is_active(
                    row.get("vf").map(String::as_str).unwrap_or(""),
                    row.get("vt").map(String::as_str).unwrap_or(""),
                    row.get("kf").map(String::as_str).unwrap_or(""),
                    row.get("kt").map(String::as_str).unwrap_or(""),
                    row.get("ev").map(String::as_str),
                    &valid,
                    &known,
                )
            })
            .filter_map(|row| {
                Some(ActiveOwnership {
                    owner: EntityId::from_uuid(
                        Uuid::parse_str(row.get("oid")?).ok()?,
                    ),
                    owned: EntityId::from_uuid(
                        Uuid::parse_str(row.get("ownedid")?).ok()?,
                    ),
                    evidence: Self::parse_evidence(row.get("ev")?),
                })
            })
            .collect())
    }

    fn owners_of(edges: &[ActiveOwnership], entity: EntityId) -> Vec<EntityId> {
        edges
            .iter()
            .filter(|e| e.owned == entity && e.evidence != EvidenceState::Refuted)
            .map(|e| e.owner)
            .collect()
    }

    fn ownership_path(edges: &[ActiveOwnership], from: EntityId, to: EntityId) -> bool {
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
            for owner in Self::owners_of(edges, current) {
                stack.push(owner);
            }
        }
        false
    }

    fn collect_person_owners(
        edges: &[ActiveOwnership],
        entity: EntityId,
        persons: &HashSet<EntityId>,
        companies: &HashSet<EntityId>,
        visited: &mut HashSet<EntityId>,
        out: &mut HashSet<PersonId>,
    ) {
        if !visited.insert(entity) {
            return;
        }
        for owner in Self::owners_of(edges, entity) {
            if persons.contains(&owner) {
                out.insert(PersonId(owner.0));
            } else if companies.contains(&owner) {
                Self::collect_person_owners(edges, owner, persons, companies, visited, out);
            }
        }
    }

    fn oracle_exposure(
        edges: &[ActiveOwnership],
        entity: EntityId,
        persons: &HashSet<EntityId>,
        companies: &HashSet<EntityId>,
    ) -> Exposure {
        let direct_owners = Self::owners_of(edges, entity);
        let direct = !direct_owners.is_empty();
        let mut path = direct_owners.clone();
        let mut indirect = false;

        for company in companies {
            let ce = *company;
            if ce == entity {
                continue;
            }
            if Self::ownership_path(edges, ce, entity) {
                for owner in Self::owners_of(edges, ce) {
                    indirect = true;
                    path.push(owner);
                }
            }
        }

        path.sort_by_key(|e| e.0);
        path.dedup();

        Exposure {
            entity,
            direct,
            indirect,
            path,
            sanctioned_controller: None,
        }
    }

    async fn visible_assertions(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<VisibleAssertion>> {
        let valid = Self::dt(valid_at);
        let known = Self::dt(known_at);
        let eid = entity.0;
        let mut out = Vec::new();

        let ownership_owned_q = format!(
            r#"match
                $x has entity-id "{eid}";
                $o isa ownership, links (owner: $owner, owned: $x);
                $owner has entity-id $sid;
                $o has share-pct $sp, has evidence-state $ev, has context-type $ctx;
                $o has valid-from $vf, has known-from $kf;
                $vf <= {valid};
                $kf <= {known};
                try {{ $o has valid-to $vt; }};
                try {{ $o has known-to $kt; }};
                try {{ $o has assertion-id $aid; }};
            select $sid, $sp, $ev, $ctx, $vf, $vt, $kf, $kt, $aid;"#,
            valid = valid,
            known = known,
        );
        for row in self.collect_named_rows(&ownership_owned_q).await? {
            if !Self::row_is_active(
                row.get("vf").map(String::as_str).unwrap_or(""),
                row.get("vt").map(String::as_str).unwrap_or(""),
                row.get("kf").map(String::as_str).unwrap_or(""),
                row.get("kt").map(String::as_str).unwrap_or(""),
                row.get("ev").map(String::as_str),
                &valid,
                &known,
            ) {
                continue;
            }
            let Some(subject) = row
                .get("sid")
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(EntityId)
            else {
                continue;
            };
            let share: f32 = row
                .get("sp")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let valid_from = row
                .get("vf")
                .and_then(|s| Self::parse_row_timestamp(s))
                .unwrap_or(valid_at);
            let id = row
                .get("aid")
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(AssertionId)
                .unwrap_or_else(|| AssertionId::new());
            out.push(VisibleAssertion {
                id,
                subject,
                predicate: format!("owns_{share}"),
                object: entity,
                evidence: Self::parse_evidence(row.get("ev").map(String::as_str).unwrap_or("")),
                context: Self::parse_context(row.get("ctx").map(String::as_str).unwrap_or("")),
                valid_from,
            });
        }

        let ownership_owner_q = format!(
            r#"match
                $x has entity-id "{eid}";
                $o isa ownership, links (owner: $x, owned: $owned);
                $owned has entity-id $oid;
                $o has share-pct $sp, has evidence-state $ev, has context-type $ctx;
                $o has valid-from $vf, has known-from $kf;
                $vf <= {valid};
                $kf <= {known};
                try {{ $o has valid-to $vt; }};
                try {{ $o has known-to $kt; }};
                try {{ $o has assertion-id $aid; }};
            select $oid, $sp, $ev, $ctx, $vf, $vt, $kf, $kt, $aid;"#,
            valid = valid,
            known = known,
        );
        for row in self.collect_named_rows(&ownership_owner_q).await? {
            if !Self::row_is_active(
                row.get("vf").map(String::as_str).unwrap_or(""),
                row.get("vt").map(String::as_str).unwrap_or(""),
                row.get("kf").map(String::as_str).unwrap_or(""),
                row.get("kt").map(String::as_str).unwrap_or(""),
                row.get("ev").map(String::as_str),
                &valid,
                &known,
            ) {
                continue;
            }
            let Some(object) = row
                .get("oid")
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(EntityId)
            else {
                continue;
            };
            let share: f32 = row
                .get("sp")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let valid_from = row
                .get("vf")
                .and_then(|s| Self::parse_row_timestamp(s))
                .unwrap_or(valid_at);
            let id = row
                .get("aid")
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(AssertionId)
                .unwrap_or_else(|| AssertionId::new());
            out.push(VisibleAssertion {
                id,
                subject: entity,
                predicate: format!("owns_{share}"),
                object,
                evidence: Self::parse_evidence(row.get("ev").map(String::as_str).unwrap_or("")),
                context: Self::parse_context(row.get("ctx").map(String::as_str).unwrap_or("")),
                valid_from,
            });
        }

        let generic_subject_q = format!(
            r#"match
                $x has entity-id "{eid}";
                $a isa generic-assertion, links (subject: $x, object: $o);
                $o has entity-id $oid;
                $a has predicate-name $pred, has evidence-state $ev, has context-type $ctx;
                $a has valid-from $vf, has known-from $kf;
                $vf <= {valid};
                $kf <= {known};
                try {{ $a has valid-to $vt; }};
                try {{ $a has known-to $kt; }};
                try {{ $a has assertion-id $aid; }};
            select $pred, $oid, $ev, $ctx, $vf, $vt, $kf, $kt, $aid;"#,
            valid = valid,
            known = known,
        );
        for row in self.collect_named_rows(&generic_subject_q).await? {
            if !Self::row_is_active(
                row.get("vf").map(String::as_str).unwrap_or(""),
                row.get("vt").map(String::as_str).unwrap_or(""),
                row.get("kf").map(String::as_str).unwrap_or(""),
                row.get("kt").map(String::as_str).unwrap_or(""),
                row.get("ev").map(String::as_str),
                &valid,
                &known,
            ) {
                continue;
            }
            let Some(object) = row
                .get("oid")
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(EntityId)
            else {
                continue;
            };
            let valid_from = row
                .get("vf")
                .and_then(|s| Self::parse_row_timestamp(s))
                .unwrap_or(valid_at);
            let id = row
                .get("aid")
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(AssertionId)
                .unwrap_or_else(|| AssertionId::new());
            out.push(VisibleAssertion {
                id,
                subject: entity,
                predicate: row.get("pred").cloned().unwrap_or_default(),
                object,
                evidence: Self::parse_evidence(row.get("ev").map(String::as_str).unwrap_or("")),
                context: Self::parse_context(row.get("ctx").map(String::as_str).unwrap_or("")),
                valid_from,
            });
        }

        let generic_object_q = format!(
            r#"match
                $x has entity-id "{eid}";
                $a isa generic-assertion, links (subject: $s, object: $x);
                $s has entity-id $sid;
                $a has predicate-name $pred, has evidence-state $ev, has context-type $ctx;
                $a has valid-from $vf, has known-from $kf;
                $vf <= {valid};
                $kf <= {known};
                try {{ $a has valid-to $vt; }};
                try {{ $a has known-to $kt; }};
                try {{ $a has assertion-id $aid; }};
            select $pred, $sid, $ev, $ctx, $vf, $vt, $kf, $kt, $aid;"#,
            valid = valid,
            known = known,
        );
        for row in self.collect_named_rows(&generic_object_q).await? {
            if !Self::row_is_active(
                row.get("vf").map(String::as_str).unwrap_or(""),
                row.get("vt").map(String::as_str).unwrap_or(""),
                row.get("kf").map(String::as_str).unwrap_or(""),
                row.get("kt").map(String::as_str).unwrap_or(""),
                row.get("ev").map(String::as_str),
                &valid,
                &known,
            ) {
                continue;
            }
            let Some(subject) = row
                .get("sid")
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(EntityId)
            else {
                continue;
            };
            let valid_from = row
                .get("vf")
                .and_then(|s| Self::parse_row_timestamp(s))
                .unwrap_or(valid_at);
            let id = row
                .get("aid")
                .and_then(|s| Uuid::parse_str(s).ok())
                .map(AssertionId)
                .unwrap_or_else(|| AssertionId::new());
            out.push(VisibleAssertion {
                id,
                subject,
                predicate: row.get("pred").cloned().unwrap_or_default(),
                object: entity,
                evidence: Self::parse_evidence(row.get("ev").map(String::as_str).unwrap_or("")),
                context: Self::parse_context(row.get("ctx").map(String::as_str).unwrap_or("")),
                valid_from,
            });
        }

        Ok(out)
    }

    fn parse_context(s: &str) -> Context {
        match s {
            "KYC" => Context::Kyc,
            "SANCTIONS" => Context::Sanctions,
            "REGULATORY" => Context::Regulatory,
            _ => Context::CorporateRegistry,
        }
    }

    fn oracle_context_compatibility(assertions: &[VisibleAssertion]) -> Compatibility {
        let contexts = Context::base_contexts();
        let mut states: HashMap<Context, Vec<&VisibleAssertion>> = HashMap::new();
        for ctx in contexts {
            states.insert(
                *ctx,
                assertions.iter().filter(|a| a.context == *ctx).collect(),
            );
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

        let cr = states
            .get(&Context::CorporateRegistry)
            .cloned()
            .unwrap_or_default();
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
            Compatibility::Indeterminate
        } else if has_contradiction {
            Compatibility::Contradictory
        } else {
            Compatibility::Consistent
        }
    }

    fn oracle_contradictions(assertions: &[VisibleAssertion]) -> Vec<Conflict> {
        let mut conflicts = Vec::new();
        for i in 0..assertions.len() {
            for j in (i + 1)..assertions.len() {
                let a = &assertions[i];
                let b = &assertions[j];
                if a.predicate == b.predicate
                    && a.object == b.object
                    && a.valid_from <= b.valid_from
                    && join_evidence(a.evidence, b.evidence) == EvidenceState::Contradictory
                {
                    conflicts.push(Conflict {
                        assertion_a: a.id,
                        assertion_b: b.id,
                        reason: "evidence_contradiction".into(),
                    });
                }
            }
        }
        conflicts
    }

    pub async fn beneficial_owners(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<PersonId>> {
        let (persons, companies) = self.party_sets().await?;
        let edges = self.active_ownerships(valid_at, known_at).await?;
        let mut out = HashSet::new();
        let mut visited = HashSet::new();
        Self::collect_person_owners(&edges, entity, &persons, &companies, &mut visited, &mut out);
        let mut owners: Vec<PersonId> = out.into_iter().collect();
        owners.sort_by_key(|p| p.0);
        Ok(owners)
    }

    pub async fn state_at(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<EntityState> {
        let owners = self.beneficial_owners(entity, valid_at, known_at).await?;
        let sanctioned = self.is_sanctioned(entity, valid_at, known_at).await?;
        Ok(EntityState {
            entity,
            assertions: vec![],
            beneficial_owners: owners,
            sanctioned,
        })
    }

    pub async fn contradictions(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<Conflict>> {
        let assertions = self.visible_assertions(entity, valid_at, known_at).await?;
        Ok(Self::oracle_contradictions(&assertions))
    }

    pub async fn ownership_exposure(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Exposure> {
        let (persons, companies) = self.party_sets().await?;
        let edges = self.active_ownerships(valid_at, known_at).await?;
        let mut exposure = Self::oracle_exposure(&edges, entity, &persons, &companies);

        for owner in &exposure.path.clone() {
            if persons.contains(owner)
                && self.is_sanctioned(*owner, valid_at, known_at).await?
            {
                exposure.sanctioned_controller = Some(PersonId(owner.0));
                break;
            }
        }

        Ok(exposure)
    }

    pub async fn compliance_decision(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Decision> {
        let exposure = self
            .ownership_exposure(entity, valid_at, known_at)
            .await?;
        let conflicts = self.contradictions(entity, valid_at, known_at).await?;

        if exposure.sanctioned_controller.is_some() {
            return Ok(Decision::Block);
        }
        if !conflicts.is_empty() {
            return Ok(Decision::Review);
        }
        let threshold = self.compliance_threshold().await?;
        let owners = self.beneficial_owners(entity, valid_at, known_at).await?;
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

    async fn compliance_threshold(&self) -> Result<f64> {
        let query =
            r#"match $r isa compliance-rule, has rule-id "rule_00", has threshold-pct $t; select $t;"#;
        Ok(self
            .collect_named_rows(query)
            .await?
            .into_iter()
            .find_map(|row| row.get("t").and_then(|s| s.parse().ok()))
            .unwrap_or(25.0))
    }

    pub async fn identity_action(
        &self,
        person_a: PersonId,
        person_b: PersonId,
        _valid_at: Timestamp,
        _known_at: Timestamp,
    ) -> Result<IdentityAction> {
        let a = person_a.0;
        let b = person_b.0;

        let query = format!(
            r#"match
                $p1 isa person, has entity-id "{a}", has canonical-name $c1;
                $p2 isa person, has entity-id "{b}", has canonical-name $c2;
            select $c1, $c2;"#
        );

        let rows = self.collect_rows(&query).await?;
        let (canon_a, canon_b) = rows.first().map(|r| (r.0.clone(), r.1.clone())).unwrap_or_default();

        let merge_q = format!(
            r#"match
                $p isa person, has entity-id "{a}";
                $l isa identity-link, links (linked-person: $p), has merge-flag true;
            select $l;"#
        );
        let has_merge = !self.collect_rows(&merge_q).await?.is_empty();

        let merge_q2 = format!(
            r#"match
                $p isa person, has entity-id "{b}";
                $l isa identity-link, links (linked-person: $p), has merge-flag true;
            select $l;"#
        );
        let has_merge = has_merge || !self.collect_rows(&merge_q2).await?.is_empty();

        if has_merge || (!canon_a.is_empty() && canon_a == canon_b) {
            Ok(IdentityAction::Merge)
        } else {
            Ok(IdentityAction::KeepSeparate)
        }
    }

    pub async fn context_compatibility(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Compatibility> {
        let assertions = self.visible_assertions(entity, valid_at, known_at).await?;
        Ok(Self::oracle_context_compatibility(&assertions))
    }

    async fn is_sanctioned(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<bool> {
        let valid = Self::dt(valid_at);
        let known = Self::dt(known_at);
        let eid = entity.0;

        let query = format!(
            r#"match
                $p isa person, has entity-id "{eid}";
                $s isa sanction-listing, links (sanctioned-person: $p);
                {bt}
                $s has listed-flag true;
            select $vf, $vt, $kf, $kt;"#,
            bt = Self::bitemporal_start("$s", &valid, &known, ""),
        );

        Ok(self
            .collect_named_rows(&query)
            .await?
            .into_iter()
            .any(|row| {
                Self::row_is_active(
                    row.get("vf").map(String::as_str).unwrap_or(""),
                    row.get("vt").map(String::as_str).unwrap_or(""),
                    row.get("kf").map(String::as_str).unwrap_or(""),
                    row.get("kt").map(String::as_str).unwrap_or(""),
                    None,
                    &valid,
                    &known,
                )
            }))
    }

    /// Q9 — role-agnostic traversal.
    ///
    /// No relation type is named anywhere in these two patterns: `$r links ($role: $x)`
    /// matches whatever relations exist. That is the property under test, so this code
    /// must stay frozen when the ontology is extended.
    pub async fn neighborhood(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Neighborhood> {
        let valid = Self::dt(valid_at);
        let known = Self::dt(known_at);
        let bt = Self::bitemporal_start("$r", &valid, &known, "");

        // Participations that have at least one counterparty.
        let paired = format!(
            r#"match
                $x has entity-id "{eid}";
                $r links ($role: $x);
                $r links ($orole: $y);
                not {{ $y is $x; }};
                $y has entity-id $yid;
                $r isa $rt;
                {bt}
            select $rt, $role, $yid, $vt, $kt;"#,
            eid = entity.0
        );

        // All participations, including relations where the entity is the only player.
        let solo = format!(
            r#"match
                $x has entity-id "{eid}";
                $r links ($role: $x);
                $r isa $rt;
                {bt}
            select $rt, $role, $vt, $kt;"#,
            eid = entity.0
        );

        let mut edges = Vec::new();
        let mut paired_kinds: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();

        for row in self.collect_named_rows(&paired).await? {
            if !Self::row_visible(&row, &valid, &known) {
                continue;
            }
            let (rt, role) = (Self::col(&row, "rt"), Self::col(&row, "role"));
            let Some(counterparty) = Self::parse_uuid(&Self::col(&row, "yid")) else {
                continue;
            };
            paired_kinds.insert((rt.clone(), role.clone()));
            edges.push(NeighborEdge {
                relation_type: Self::normalise_type(&rt),
                role: Self::normalise_type(&role),
                counterparty: Some(EntityId::from_uuid(counterparty)),
            });
        }

        for row in self.collect_named_rows(&solo).await? {
            if !Self::row_visible(&row, &valid, &known) {
                continue;
            }
            let (rt, role) = (Self::col(&row, "rt"), Self::col(&row, "role"));
            if paired_kinds.contains(&(rt.clone(), role.clone())) {
                continue;
            }
            edges.push(NeighborEdge {
                relation_type: Self::normalise_type(&rt),
                role: Self::normalise_type(&role),
                counterparty: None,
            });
        }

        Ok(Neighborhood::new(entity, edges))
    }

    fn col(row: &HashMap<String, String>, key: &str) -> String {
        row.get(key).cloned().unwrap_or_default()
    }

    fn row_visible(row: &HashMap<String, String>, valid: &str, known: &str) -> bool {
        Self::interval_open(Some(Self::col(row, "vt").as_str()).filter(|s| !s.is_empty()), valid)
            && Self::interval_open(Some(Self::col(row, "kt").as_str()).filter(|s| !s.is_empty()), known)
    }

    /// Role labels come back qualified (`ownership:owner`); the benchmark compares bare
    /// names so that both backends speak the same vocabulary.
    fn normalise_type(label: &str) -> String {
        label.rsplit(':').next().unwrap_or(label).trim().to_string()
    }

    fn parse_uuid(s: &str) -> Option<Uuid> {
        Uuid::parse_str(s.trim().trim_matches('"')).ok()
    }

    pub(crate) async fn collect_named_rows(&self, query: &str) -> Result<Vec<HashMap<String, String>>> {
        let tx = self
            .driver
            .transaction(self.database, TransactionType::Read)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let answer = tx
            .query(query)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let rows = Self::extract_named_rows(answer).await;
        drop(tx);
        rows
    }

    async fn extract_named_rows(answer: QueryAnswer) -> Result<Vec<HashMap<String, String>>> {
        if !answer.is_row_stream() {
            return Ok(vec![]);
        }
        let mut stream = answer.into_rows();
        let mut out = Vec::new();
        while let Some(row) = stream.next().await {
            let row = row.map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
            let mut map = HashMap::new();
            for col in row.get_column_names() {
                if let Ok(Some(concept)) = row.get(col) {
                    map.insert(col.to_string(), concept_string(concept));
                } else {
                    map.insert(col.to_string(), String::new());
                }
            }
            out.push(map);
        }
        Ok(out)
    }

    async fn collect_string_column(&self, query: &str, column: &str) -> Result<Vec<String>> {
        let rows = self.collect_rows(query).await?;
        if rows.is_empty() {
            return Ok(vec![]);
        }
        // Single-column select
        if rows[0].1.is_empty() {
            return Ok(rows.into_iter().map(|r| r.0).collect());
        }
        // Named column - re-run parsing from full row map
        let tx = self
            .driver
            .transaction(self.database, TransactionType::Read)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let answer = tx
            .query(query)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let out = Self::extract_column(answer, column).await;
        drop(tx);
        out
    }

    async fn collect_rows(&self, query: &str) -> Result<Vec<(String, String)>> {
        let tx = self
            .driver
            .transaction(self.database, TransactionType::Read)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let answer = tx
            .query(query)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let rows = Self::extract_rows(answer).await;
        drop(tx);
        rows
    }

    async fn extract_rows(answer: QueryAnswer) -> Result<Vec<(String, String)>> {
        if !answer.is_row_stream() {
            return Ok(vec![]);
        }
        let mut stream = answer.into_rows();
        let mut out = Vec::new();
        while let Some(row) = stream.next().await {
            let row = row.map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
            let cols = row.get_column_names();
            let mut values = Vec::new();
            for col in cols {
                if let Ok(Some(concept)) = row.get(col) {
                    values.push(concept_string(concept));
                }
            }
            match values.len() {
                0 => {}
                1 => out.push((values[0].clone(), String::new())),
                _ => out.push((values[0].clone(), values[1].clone())),
            }
        }
        Ok(out)
    }

    async fn extract_column(answer: QueryAnswer, column: &str) -> Result<Vec<String>> {
        if !answer.is_row_stream() {
            return Ok(vec![]);
        }
        let mut stream = answer.into_rows();
        let mut out = Vec::new();
        while let Some(row) = stream.next().await {
            let row = row.map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
            if let Ok(Some(concept)) = row.get(column) {
                out.push(concept_string(concept));
            } else if let Ok(Some(concept)) = row.get_index(0) {
                out.push(concept_string(concept));
            }
        }
        Ok(out)
    }
}

fn concept_string(concept: &Concept) -> String {
    if let Some(s) = concept.try_get_string() {
        return s.to_string();
    }
    if let Some(i) = concept.try_get_integer() {
        return i.to_string();
    }
    if let Some(b) = concept.try_get_boolean() {
        return b.to_string();
    }
    if let Some(d) = concept.try_get_double() {
        return d.to_string();
    }
    // Datetimes must be rendered in the same format `TypeDbReads::dt` emits, otherwise the
    // Rust-side bitemporal comparisons silently fail to parse and treat every closed
    // interval as still open.
    if let Some(dt) = concept.try_get_datetime() {
        return dt.format(DATETIME_FMT).to_string();
    }
    if let Some(dt) = concept.try_get_datetime_tz() {
        return dt.naive_utc().format(DATETIME_FMT).to_string();
    }
    if let Some(d) = concept.try_get_date() {
        return d.format("%Y-%m-%d").to_string();
    }
    concept.get_label().to_string()
}
