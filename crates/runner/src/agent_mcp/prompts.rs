use benchmark_core::{QueryFamily, QueryProbe, Timestamp};

/// Natural-language prompt an agent would receive (retrieval task).
pub fn nl_prompt(probe: &QueryProbe) -> String {
    match probe.family {
        QueryFamily::Q1BeneficialOwner => {
            "Who are the current beneficial owners of this company?".into()
        }
        QueryFamily::Q2BitemporalLookup => format!(
            "What did we know on {} about the situation valid on {}?",
            probe.known_at.format("%Y-%m-%d"),
            probe.valid_at.format("%Y-%m-%d")
        ),
        QueryFamily::Q3Contradictions => {
            "Which active assertions contradict each other for this entity?".into()
        }
        QueryFamily::Q4IdentityDiscrimination => {
            "Should John Smith and Jonathan Smith be merged or kept separate?".into()
        }
        QueryFamily::Q5OwnershipExposure => {
            "Does a sanctioned person control this company directly or indirectly?".into()
        }
        QueryFamily::Q6ContextCompatibility => {
            "Are Corporate Registry, KYC, and Sanctions contexts compatible for this entity?"
                .into()
        }
        QueryFamily::Q7HistoricalReplay => format!(
            "Given only information known on {}, would compliance be ALLOW, REVIEW, or BLOCK?",
            probe.known_at.format("%Y-%m-%d")
        ),
        QueryFamily::Q8RetrospectiveView => format!(
            "With today's knowledge, how should we qualify the situation valid on {}?",
            probe.valid_at.format("%Y-%m-%d")
        ),
        QueryFamily::Q9RoleAgnosticTraversal => {
            "What is this entity connected to, through any kind of relationship?".into()
        }
    }
}

/// SQL an agent would generate after schema introspection (Postgres MCP path).
pub fn postgres_sql(probe: &QueryProbe) -> String {
    let eid = probe.entity.0;
    let valid = ts(probe.valid_at);
    let known = ts(probe.known_at);

    match probe.family {
        QueryFamily::Q1BeneficialOwner => format!(
            r#"SELECT DISTINCT o.owner_id FROM ownership_assertion o
               WHERE o.owned_id = '{eid}'
                 AND o.valid_range @> '{valid}'::timestamptz
                 AND o.known_range @> '{known}'::timestamptz
                 AND o.evidence != 'REFUTED'"#
        ),
        QueryFamily::Q2BitemporalLookup => format!(
            r#"SELECT COUNT(*) FROM ownership_assertion o
               WHERE (o.owner_id = '{eid}' OR o.owned_id = '{eid}')
                 AND o.valid_range @> '{valid}'::timestamptz
                 AND o.known_range @> '{known}'::timestamptz"#
        ),
        QueryFamily::Q3Contradictions => format!(
            r#"SELECT COUNT(*) FROM assertion a1
               JOIN assertion a2 ON a1.subject_id = a2.subject_id
                 AND a1.predicate = a2.predicate AND a1.id < a2.id
               WHERE a1.subject_id = '{eid}'
                 AND ((a1.evidence = 'SUPPORTED' AND a2.evidence = 'REFUTED')
                   OR (a1.evidence = 'REFUTED' AND a2.evidence = 'SUPPORTED'))"#
        ),
        QueryFamily::Q4IdentityDiscrimination => {
            let a = probe.person_a.map(|p| p.0).unwrap_or(eid);
            let b = probe.person_b.map(|p| p.0).unwrap_or(eid);
            format!(
                r#"SELECT p1.canonical_name = p2.canonical_name AS should_merge
                   FROM person p1, person p2
                   WHERE p1.id = '{a}' AND p2.id = '{b}'"#
            )
        }
        QueryFamily::Q5OwnershipExposure => format!(
            r#"SELECT COUNT(*) FROM ownership_assertion o
               WHERE o.owned_id = '{eid}'
                 AND o.valid_range @> '{valid}'::timestamptz
                 AND o.known_range @> '{known}'::timestamptz
                 AND o.evidence != 'REFUTED'"#
        ),
        QueryFamily::Q6ContextCompatibility => format!(
            r#"SELECT COUNT(DISTINCT context) FROM (
                 SELECT context FROM assertion WHERE subject_id = '{eid}'
                 UNION ALL
                 SELECT context FROM ownership_assertion
                 WHERE owner_id = '{eid}' OR owned_id = '{eid}'
               ) ctx"#
        ),
        QueryFamily::Q7HistoricalReplay | QueryFamily::Q8RetrospectiveView => format!(
            r#"SELECT CASE
                 WHEN EXISTS (
                   SELECT 1 FROM sanction_listing s
                   WHERE s.person_id = '{eid}'
                     AND s.listed = true
                     AND s.valid_range @> '{valid}'::timestamptz
                     AND s.known_range @> '{known}'::timestamptz
                 ) THEN 'BLOCK'
                 ELSE 'ALLOW'
               END AS decision"#
        ),
        QueryFamily::Q9RoleAgnosticTraversal => format!(
            r#"SELECT 'ownership' AS rel, owned_id AS other FROM ownership_assertion
                WHERE owner_id = '{eid}'
               UNION ALL
               SELECT 'ownership', owner_id FROM ownership_assertion WHERE owned_id = '{eid}'
               UNION ALL
               SELECT 'generic-assertion', object_id FROM assertion WHERE subject_id = '{eid}'
               UNION ALL
               SELECT 'generic-assertion', subject_id FROM assertion WHERE object_id = '{eid}'
               UNION ALL
               SELECT 'sanction-listing', NULL::uuid FROM sanction_listing
                WHERE person_id = '{eid}'"#
        ),
    }
}

/// TypeQL an agent would generate after schema introspection (TypeDB MCP path).
pub fn typedb_typeql(probe: &QueryProbe) -> String {
    let eid = probe.entity.0;
    match probe.family {
        QueryFamily::Q1BeneficialOwner => format!(
            r#"match
                $c isa company, has entity-id "{eid}";
                (owner: $p, owned: $c) isa ownership;
                $p has entity-id $pid;
              select $pid;"#
        ),
        QueryFamily::Q5OwnershipExposure => format!(
            r#"match
                $c isa company, has entity-id "{eid}";
                (owner: $p, owned: $c) isa ownership;
              select $p;"#
        ),
        QueryFamily::Q9RoleAgnosticTraversal => format!(
            r#"match
                $x has entity-id "{eid}";
                $r links ($role: $x);
                $r isa $rt;
              select $rt, $role;"#
        ),
        _ => format!(
            r#"match $x has entity-id "{eid}"; select $x;"#
        ),
    }
}

fn ts(t: Timestamp) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string()
}
