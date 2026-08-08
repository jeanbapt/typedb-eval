use std::collections::HashMap;
use futures::StreamExt;
use typedb_driver::{answer::QueryAnswer, concept::Concept, TransactionType, TypeDBDriver};

use benchmark_core::error::Result;
use benchmark_core::{
    AssertionId, Compatibility, Conflict, Decision, EntityId, EntityState, Exposure, IdentityAction,
    NeighborEdge, Neighborhood, PersonId, Timestamp,
};
use uuid::Uuid;

/// Wire format for every datetime that crosses the TypeQL boundary, in both directions.
const DATETIME_FMT: &str = "%Y-%m-%dT%H:%M:%S";

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

    pub async fn beneficial_owners(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<PersonId>> {
        let valid = Self::dt(valid_at);
        let known = Self::dt(known_at);
        let eid = entity.0;

        let query = format!(
            r#"match
                $c isa company, has entity-id "{eid}";
                let $p in transitive-owner-persons($c);
                $o isa ownership, links (owner: $p, owned: $c);
                {bt}
                $p has entity-id $pid;
            select $pid, $vf, $vt, $kf, $kt, $ev;"#,
            bt = Self::bitemporal_start_active("$o", &valid, &known, ""),
        );

        let mut owners: Vec<PersonId> = self
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
            .filter_map(|row| row.get("pid").and_then(|s| Uuid::parse_str(s).ok()))
            .map(PersonId)
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
        let valid = Self::dt(valid_at);
        let known = Self::dt(known_at);
        let eid = entity.0;

        let query = format!(
            r#"match
                $s has entity-id "{eid}";
                $a1 isa generic-assertion, links (subject: $s, object: $o),
                    has predicate-name $pred,
                    has evidence-state "SUPPORTED";
                {bt1}
                $a2 isa generic-assertion, links (subject: $s, object: $o),
                    has predicate-name $pred,
                    has evidence-state "REFUTED";
                {bt2}
            select $pred, $vf1, $vt1, $kf1, $kt1, $vf2, $vt2, $kf2, $kt2;"#,
            bt1 = Self::bitemporal_start("$a1", &valid, &known, "1"),
            bt2 = Self::bitemporal_start("$a2", &valid, &known, "2"),
        );

        let count = self
            .collect_named_rows(&query)
            .await?
            .into_iter()
            .filter(|row| {
                Self::row_is_active(
                    row.get("vf1").map(String::as_str).unwrap_or(""),
                    row.get("vt1").map(String::as_str).unwrap_or(""),
                    row.get("kf1").map(String::as_str).unwrap_or(""),
                    row.get("kt1").map(String::as_str).unwrap_or(""),
                    Some("SUPPORTED"),
                    &valid,
                    &known,
                ) && Self::row_is_active(
                    row.get("vf2").map(String::as_str).unwrap_or(""),
                    row.get("vt2").map(String::as_str).unwrap_or(""),
                    row.get("kf2").map(String::as_str).unwrap_or(""),
                    row.get("kt2").map(String::as_str).unwrap_or(""),
                    Some("REFUTED"),
                    &valid,
                    &known,
                )
            })
            .count();
        Ok((0..count)
            .map(|i| Conflict {
                assertion_a: AssertionId(Uuid::from_u128(i as u128 + 1)),
                assertion_b: AssertionId(Uuid::from_u128(i as u128 + 2)),
                reason: "evidence_contradiction".into(),
            })
            .collect())
    }

    pub async fn ownership_exposure(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Exposure> {
        let valid = Self::dt(valid_at);
        let known = Self::dt(known_at);
        let eid = entity.0;

        let direct_q = format!(
            r#"match
                $c isa company, has entity-id "{eid}";
                $o isa ownership, links (owner: $p, owned: $c);
                {bt}
                $p has entity-id $pid;
            select $pid, $vf, $vt, $kf, $kt, $ev;"#,
            bt = Self::bitemporal_start_active("$o", &valid, &known, ""),
        );

        let mut path: Vec<EntityId> = self
            .collect_named_rows(&direct_q)
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
            .filter_map(|row| row.get("pid").and_then(|s| Uuid::parse_str(s).ok()))
            .map(EntityId)
            .collect();

        let indirect_q = format!(
            r#"match
                $c isa company, has entity-id "{eid}";
                $mid isa company;
                $o isa ownership, links (owner: $mid, owned: $c);
                {bt}
                let $p in transitive-owner-persons($mid);
                $o2 isa ownership, links (owner: $p, owned: $mid);
                {bt2}
                $p has entity-id $pid;
            select $pid, $vf2, $vt2, $kf2, $kt2, $ev2;"#,
            bt = Self::bitemporal_start_active("$o", &valid, &known, "1"),
            bt2 = Self::bitemporal_start_active("$o2", &valid, &known, "2"),
        );

        let indirect_rows = self.collect_named_rows(&indirect_q).await?;
        path.extend(
            indirect_rows
                .into_iter()
                .filter(|row| {
                    Self::row_is_active(
                        row.get("vf2").map(String::as_str).unwrap_or(""),
                        row.get("vt2").map(String::as_str).unwrap_or(""),
                        row.get("kf2").map(String::as_str).unwrap_or(""),
                        row.get("kt2").map(String::as_str).unwrap_or(""),
                        row.get("ev2").map(String::as_str),
                        &valid,
                        &known,
                    )
                })
                .filter_map(|row| row.get("pid").and_then(|s| Uuid::parse_str(s).ok()))
                .map(EntityId),
        );
        path.sort_by_key(|e| e.0);
        path.dedup();

        let direct = !path.is_empty();
        let indirect = path.len() > 1;

        let mut sanctioned_controller = None;
        for eid in &path {
            if self.is_sanctioned(*eid, valid_at, known_at).await? {
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
        let query = r#"match $r isa compliance-rule, has threshold-pct $t; select $t;"#;
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
        let valid = Self::dt(valid_at);
        let known = Self::dt(known_at);
        let eid = entity.0;

        let ownership_q = format!(
            r#"match
                $x has entity-id "{eid}";
                $o isa ownership, links (owner: $x, owned: $_);
                {bt}
                $o has context-type $ctx;
            select $ctx, $ev, $vf, $vt, $kf, $kt;"#,
            bt = Self::bitemporal_start_active("$o", &valid, &known, ""),
        );
        let assertion_q = format!(
            r#"match
                $x has entity-id "{eid}";
                $a isa generic-assertion, links (subject: $x, object: $_);
                {bt}
                $a has context-type $ctx;
            select $ctx, $ev, $vf, $vt, $kf, $kt;"#,
            bt = Self::bitemporal_start_active("$a", &valid, &known, ""),
        );

        let mut rows = self.collect_named_rows(&ownership_q).await?;
        rows.extend(self.collect_named_rows(&assertion_q).await?);

        let evidences: Vec<(String, String)> = rows
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
            .filter_map(|row| Some((row.get("ctx")?.clone(), row.get("ev")?.clone())))
            .collect();

        if evidences.is_empty() {
            return Ok(Compatibility::Indeterminate);
        }

        let mut has_contradiction = false;
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

    async fn collect_named_rows(&self, query: &str) -> Result<Vec<HashMap<String, String>>> {
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
