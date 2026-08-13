use std::collections::HashMap;
use std::sync::LazyLock;
use futures::StreamExt;
use typedb_driver::{
    answer::QueryAnswer,
    concept::Concept,
    given::{GivenRowEntry, GivenRows},
    TransactionType, TypeDBDriver,
};

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
struct VisibleAssertion {
    id: AssertionId,
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

/// `given` prologue shared by every parameterized read: the probe entity and the
/// two bitemporal instants. Keeping the query text itself constant lets the server's
/// 3.12.2 parse/translation/compile caches (keyed on the exact query string) hit on
/// every call; only the given rows change.
const GIVEN_EVK: &str = "given $eid: string, $valid: datetime, $known: datetime;";

/// One row binding `$eid`, `$valid`, `$known`.
fn rows_evk(entity: EntityId, valid_at: Timestamp, known_at: Timestamp) -> GivenRows {
    let mut rows = GivenRows::new(
        vec!["eid".to_string(), "valid".to_string(), "known".to_string()],
        1,
    );
    rows.push_row(vec![
        GivenRowEntry::from(entity.0.to_string()),
        GivenRowEntry::from(valid_at.naive_utc()),
        GivenRowEntry::from(known_at.naive_utc()),
    ])
    .expect("given row width");
    rows
}

/// One row binding a single `$eid`.
fn rows_eid(entity: Uuid) -> GivenRows {
    let mut rows = GivenRows::new(vec!["eid".to_string()], 1);
    rows.push_row(vec![GivenRowEntry::from(entity.to_string())])
        .expect("given row width");
    rows
}

static OWNERSHIP_OWNED_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $x has entity-id == $eid;
            $o isa ownership, links (owner: $owner, owned: $x);
            $owner has entity-id $sid;
            $o has share-pct $sp, has evidence-state $ev, has context-type $ctx;
            not {{ $ev == "REFUTED"; }};
            {bt}
            try {{ $o has assertion-id $aid; }};
        select $sid, $sp, $ev, $ctx, $vf, $aid;"#,
        bt = TypeDbReads::bitemporal_active("$o"),
    )
});

static OWNERSHIP_OWNER_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $x has entity-id == $eid;
            $o isa ownership, links (owner: $x, owned: $owned);
            $owned has entity-id $oid;
            $o has share-pct $sp, has evidence-state $ev, has context-type $ctx;
            not {{ $ev == "REFUTED"; }};
            {bt}
            try {{ $o has assertion-id $aid; }};
        select $oid, $sp, $ev, $ctx, $vf, $aid;"#,
        bt = TypeDbReads::bitemporal_active("$o"),
    )
});

static GENERIC_SUBJECT_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $x has entity-id == $eid;
            $a isa generic-assertion, links (subject: $x, object: $o);
            $o has entity-id $oid;
            $a has predicate-name $pred, has evidence-state $ev, has context-type $ctx;
            not {{ $ev == "REFUTED"; }};
            {bt}
            try {{ $a has assertion-id $aid; }};
        select $pred, $oid, $ev, $ctx, $vf, $aid;"#,
        bt = TypeDbReads::bitemporal_active("$a"),
    )
});

static GENERIC_OBJECT_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $x has entity-id == $eid;
            $a isa generic-assertion, links (subject: $s, object: $x);
            $s has entity-id $sid;
            $a has predicate-name $pred, has evidence-state $ev, has context-type $ctx;
            not {{ $ev == "REFUTED"; }};
            {bt}
            try {{ $a has assertion-id $aid; }};
        select $pred, $sid, $ev, $ctx, $vf, $aid;"#,
        bt = TypeDbReads::bitemporal_active("$a"),
    )
});

static BENEFICIAL_OWNERS_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $target isa company, has entity-id == $eid;
            let $p in transitive-owner-persons($target, $valid, $known);
            $p has entity-id $pid;
        select $pid;"#
    )
});

static EXPO_DIRECT_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $target isa company, has entity-id == $eid;
            {{ let $d in active-company-owners($target, $valid, $known); }}
            or {{ let $d in active-person-owners($target, $valid, $known); }};
            $d has entity-id $did;
        select $did;"#
    )
});

static EXPO_SUBS_COMPANY_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $root isa company, has entity-id == $eid;
            let $ce in transitive-owned-companies($root, $valid, $known);
            not {{ $ce has entity-id == $eid; }};
            {{ let $co in active-company-owners($ce, $valid, $known); }}
            or {{ let $co in active-person-owners($ce, $valid, $known); }};
            $co has entity-id $coid;
        select $coid;"#
    )
});

static EXPO_SUBS_PERSON_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $rootp isa person, has entity-id == $eid;
            {{ let $ce in active-owned-companies-of-person($rootp, $valid, $known); }}
            or {{
                let $c0 in active-owned-companies-of-person($rootp, $valid, $known);
                let $ce in transitive-owned-companies($c0, $valid, $known);
            }};
            {{ let $co in active-company-owners($ce, $valid, $known); }}
            or {{ let $co in active-person-owners($ce, $valid, $known); }};
            $co has entity-id $coid;
        select $coid;"#
    )
});

static EXPO_SANCTIONED_COMPANY_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $target isa company, has entity-id == $eid;
            {{
                let $p in active-person-owners($target, $valid, $known);
            }} or {{
                let $ce in transitive-owned-companies($target, $valid, $known);
                not {{ $ce has entity-id == $eid; }};
                let $p in active-person-owners($ce, $valid, $known);
            }};
            $p has entity-id $pid;
            $s isa sanction-listing, links (sanctioned-person: $p);
            {bt}
            $s has listed-flag true;
        select $pid;"#,
        bt = TypeDbReads::bitemporal_active("$s"),
    )
});

static EXPO_SANCTIONED_PERSON_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $rootp isa person, has entity-id == $eid;
            {{ let $ce in active-owned-companies-of-person($rootp, $valid, $known); }}
            or {{
                let $c0 in active-owned-companies-of-person($rootp, $valid, $known);
                let $ce in transitive-owned-companies($c0, $valid, $known);
            }};
            let $p in active-person-owners($ce, $valid, $known);
            $p has entity-id $pid;
            $s isa sanction-listing, links (sanctioned-person: $p);
            {bt}
            $s has listed-flag true;
        select $pid;"#,
        bt = TypeDbReads::bitemporal_active("$s"),
    )
});

const IDENTITY_NAMES_Q: &str = r#"given $a: string, $b: string;
    match
        $p1 isa person, has entity-id == $a, has canonical-name $c1;
        $p2 isa person, has entity-id == $b, has canonical-name $c2;
    select $c1, $c2;"#;

const IDENTITY_MERGE_Q: &str = r#"given $eid: string;
    match
        $p isa person, has entity-id == $eid;
        $l isa identity-link, links (linked-person: $p), has merge-flag true;
    select $l;"#;

static IS_SANCTIONED_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $p isa person, has entity-id == $eid;
            $s isa sanction-listing, links (sanctioned-person: $p);
            {bt}
            $s has listed-flag true;
        select $vf;"#,
        bt = TypeDbReads::bitemporal_active("$s"),
    )
});

static NEIGHBORHOOD_PAIRED_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $x has entity-id == $eid;
            $r links ($role: $x);
            $r links ($orole: $y);
            not {{ $y is $x; }};
            $y has entity-id $yid;
            $r isa $rt;
            {bt}
        select $rt, $role, $yid;"#,
        bt = TypeDbReads::bitemporal_active("$r"),
    )
});

static NEIGHBORHOOD_SOLO_Q: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"{GIVEN_EVK}
        match
            $x has entity-id == $eid;
            $r links ($role: $x);
            $r isa $rt;
            {bt}
        select $rt, $role;"#,
        bt = TypeDbReads::bitemporal_active("$r"),
    )
});

impl<'a> TypeDbReads<'a> {
    /// Full bitemporal visibility, evaluated server-side against the `given` instants
    /// `$valid` and `$known`. An interval is active when it started at or before the
    /// probe instant and no upper bound at or before that instant exists — the
    /// negations make "absent attribute = open interval" a query predicate instead of
    /// a Rust post-filter.
    pub fn bitemporal_active(rel: &str) -> String {
        format!(
            r#"{rel} has valid-from $vf, has known-from $kf;
                $vf <= $valid;
                $kf <= $known;
                not {{ {rel} has valid-to $vt; $vt <= $valid; }};
                not {{ {rel} has known-to $kt; $kt <= $known; }};"#
        )
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

    async fn visible_assertions(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<VisibleAssertion>> {
        let mut out = Vec::new();

        for row in self
            .collect_named_rows_given(&OWNERSHIP_OWNED_Q, rows_evk(entity, valid_at, known_at))
            .await?
        {
            if row.get("sid").and_then(|s| Uuid::parse_str(s).ok()).is_none() {
                continue;
            }
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
                predicate: format!("owns_{share}"),
                object: entity,
                evidence: Self::parse_evidence(row.get("ev").map(String::as_str).unwrap_or("")),
                context: Self::parse_context(row.get("ctx").map(String::as_str).unwrap_or("")),
                valid_from,
            });
        }

        for row in self
            .collect_named_rows_given(&OWNERSHIP_OWNER_Q, rows_evk(entity, valid_at, known_at))
            .await?
        {
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
                predicate: format!("owns_{share}"),
                object,
                evidence: Self::parse_evidence(row.get("ev").map(String::as_str).unwrap_or("")),
                context: Self::parse_context(row.get("ctx").map(String::as_str).unwrap_or("")),
                valid_from,
            });
        }

        for row in self
            .collect_named_rows_given(&GENERIC_SUBJECT_Q, rows_evk(entity, valid_at, known_at))
            .await?
        {
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
                predicate: row.get("pred").cloned().unwrap_or_default(),
                object,
                evidence: Self::parse_evidence(row.get("ev").map(String::as_str).unwrap_or("")),
                context: Self::parse_context(row.get("ctx").map(String::as_str).unwrap_or("")),
                valid_from,
            });
        }

        for row in self
            .collect_named_rows_given(&GENERIC_OBJECT_Q, rows_evk(entity, valid_at, known_at))
            .await?
        {
            if row.get("sid").and_then(|s| Uuid::parse_str(s).ok()).is_none() {
                continue;
            }
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

    /// Beneficial owners via the schema-level left-recursive TypeQL function:
    /// recursion and bitemporal edge filtering both run inside the server.
    pub async fn beneficial_owners(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<PersonId>> {
        let mut owners: Vec<PersonId> = self
            .collect_named_rows_given(&BENEFICIAL_OWNERS_Q, rows_evk(entity, valid_at, known_at))
            .await?
            .into_iter()
            .filter_map(|row| {
                Some(PersonId(Uuid::parse_str(row.get("pid")?).ok()?))
            })
            .collect();
        owners.sort_by_key(|p| p.0);
        owners.dedup();
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

    /// Exposure via the schema-level functions: direct owners, co-owners of every
    /// company the entity transitively owns, and sanctioned persons on that path all
    /// come back from three queries whose recursion and bitemporal filtering run
    /// inside the server.
    pub async fn ownership_exposure(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Exposure> {
        let direct_owners: Vec<EntityId> = self
            .collect_named_rows_given(&EXPO_DIRECT_Q, rows_evk(entity, valid_at, known_at))
            .await?
            .into_iter()
            .filter_map(|row| Some(EntityId(Uuid::parse_str(row.get("did")?).ok()?)))
            .collect();

        // Companies the entity transitively owns, and their other direct owners.
        // Mirrors the oracle: for every company reachable downward from the entity,
        // each of its active owners joins the exposure path. Walking down from the
        // fixed root keeps tabled recursion to a single table per probe. The root
        // binding must stay linear (outside any disjunction): the planner rejects a
        // variable bound by `isa` inside a branch being passed to a function in that
        // same branch, so company and person roots are two queries.
        let mut co_owners: Vec<EntityId> = Vec::new();
        for query in [&*EXPO_SUBS_COMPANY_Q, &*EXPO_SUBS_PERSON_Q] {
            co_owners.extend(
                self.collect_named_rows_given(query, rows_evk(entity, valid_at, known_at))
                    .await?
                    .into_iter()
                    .filter_map(|row| Some(EntityId(Uuid::parse_str(row.get("coid")?).ok()?))),
            );
        }

        let direct = !direct_owners.is_empty();
        let indirect = !co_owners.is_empty();
        let mut path = direct_owners;
        path.extend(co_owners);
        path.sort_by_key(|e| e.0);
        path.dedup();

        // First (lowest-uuid) sanctioned person on the path. Same split as above:
        // one query per root kind, then min over the union.
        let mut sanctioned: Vec<Uuid> = Vec::new();
        for query in [&*EXPO_SANCTIONED_COMPANY_Q, &*EXPO_SANCTIONED_PERSON_Q] {
            sanctioned.extend(
                self.collect_named_rows_given(query, rows_evk(entity, valid_at, known_at))
                    .await?
                    .into_iter()
                    .filter_map(|row| Uuid::parse_str(row.get("pid")?).ok()),
            );
        }
        let sanctioned_controller = sanctioned.into_iter().min().map(PersonId);

        Ok(Exposure {
            entity,
            direct,
            indirect,
            path,
            sanctioned_controller,
        })
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
        let mut name_rows = GivenRows::new(vec!["a".to_string(), "b".to_string()], 1);
        name_rows
            .push_row(vec![
                GivenRowEntry::from(person_a.0.to_string()),
                GivenRowEntry::from(person_b.0.to_string()),
            ])
            .expect("given row width");
        let rows = self
            .collect_named_rows_given(IDENTITY_NAMES_Q, name_rows)
            .await?;
        let (canon_a, canon_b) = rows
            .first()
            .map(|r| (Self::col(r, "c1"), Self::col(r, "c2")))
            .unwrap_or_default();

        let has_merge = !self
            .collect_named_rows_given(IDENTITY_MERGE_Q, rows_eid(person_a.0))
            .await?
            .is_empty();
        let has_merge = has_merge
            || !self
                .collect_named_rows_given(IDENTITY_MERGE_Q, rows_eid(person_b.0))
                .await?
                .is_empty();

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
        Ok(!self
            .collect_named_rows_given(&IS_SANCTIONED_Q, rows_evk(entity, valid_at, known_at))
            .await?
            .is_empty())
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
        let mut edges = Vec::new();
        let mut paired_kinds: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();

        // Participations that have at least one counterparty.
        for row in self
            .collect_named_rows_given(&NEIGHBORHOOD_PAIRED_Q, rows_evk(entity, valid_at, known_at))
            .await?
        {
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

        // All participations, including relations where the entity is the only player.
        for row in self
            .collect_named_rows_given(&NEIGHBORHOOD_SOLO_Q, rows_evk(entity, valid_at, known_at))
            .await?
        {
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

    /// Runs a constant query text with parameters supplied as `given` rows, so the
    /// server's parse/translation/compile caches hit on every call.
    pub(crate) async fn collect_named_rows_given(
        &self,
        query: &str,
        rows: GivenRows,
    ) -> Result<Vec<HashMap<String, String>>> {
        let tx = self
            .driver
            .transaction(self.database, TransactionType::Read)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let answer = tx
            .query_with_rows(query, rows)
            .await
            .map_err(|e| benchmark_core::BenchmarkError::Database(e.to_string()))?;
        let named = Self::extract_named_rows(answer).await;
        drop(tx);
        named
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
