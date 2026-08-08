use benchmark_core::{ComplianceStore, CorrectnessMetrics, ExpectedAnswer, QueryProbe};
use benchmark_core::{oracle::Oracle, Decision, IdentityAction};

pub async fn run_correctness_checks<S: ComplianceStore>(
    store: &S,
    oracle: &Oracle,
    probes: &[QueryProbe],
    expected: &[(QueryProbe, ExpectedAnswer)],
) -> CorrectnessMetrics {
    let mut metrics = CorrectnessMetrics {
        total_probes: probes.len() as u64,
        ..Default::default()
    };

    for (probe, expected_answer) in expected {
        let actual = oracle.answer_probe(probe);
        let store_result = execute_and_compare(store, probe).await;

        match (&actual, &store_result, expected_answer) {
            (Ok(actual_ans), Ok(store_ans), exp) => {
                if compare_answers(actual_ans, exp) && compare_answers(store_ans, exp) {
                    metrics.passed += 1;
                } else {
                    classify_mismatch(&mut metrics, exp, store_ans);
                }
            }
            _ => {
                classify_mismatch(
                    &mut metrics,
                    expected_answer,
                    &ExpectedAnswer::Decision {
                        decision: Decision::Review,
                    },
                );
            }
        }
    }

    metrics
}

async fn execute_and_compare<S: ComplianceStore>(
    store: &S,
    probe: &QueryProbe,
) -> Result<ExpectedAnswer, String> {
    use benchmark_core::QueryFamily;
    match probe.family {
        QueryFamily::Q1BeneficialOwner => {
            let owners = store
                .beneficial_owners(probe.entity, probe.valid_at, probe.known_at)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ExpectedAnswer::BeneficialOwners { owners })
        }
        QueryFamily::Q2BitemporalLookup => {
            let state = store
                .state_at(probe.entity, probe.valid_at, probe.known_at)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ExpectedAnswer::EntityState { state })
        }
        QueryFamily::Q3Contradictions => {
            let conflicts = store
                .contradictions(probe.entity, probe.valid_at, probe.known_at)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ExpectedAnswer::Conflicts { conflicts })
        }
        QueryFamily::Q4IdentityDiscrimination => {
            let a = probe.person_a.ok_or("missing person_a")?;
            let b = probe.person_b.ok_or("missing person_b")?;
            let action = store
                .identity_action(a, b, probe.valid_at, probe.known_at)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ExpectedAnswer::IdentityAction { action })
        }
        QueryFamily::Q5OwnershipExposure => {
            let exposure = store
                .ownership_exposure(probe.entity, probe.valid_at, probe.known_at)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ExpectedAnswer::Exposure { exposure })
        }
        QueryFamily::Q6ContextCompatibility => {
            let result = store
                .context_compatibility(probe.entity, probe.valid_at, probe.known_at)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ExpectedAnswer::Compatibility { result })
        }
        QueryFamily::Q7HistoricalReplay | QueryFamily::Q8RetrospectiveView => {
            let decision = store
                .compliance_decision(probe.entity, probe.valid_at, probe.known_at)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ExpectedAnswer::Decision { decision })
        }
        QueryFamily::Q9RoleAgnosticTraversal => {
            let neighborhood = store
                .neighborhood(probe.entity, probe.valid_at, probe.known_at)
                .await
                .map_err(|e| e.to_string())?;
            Ok(ExpectedAnswer::Neighborhood { neighborhood })
        }
    }
}

pub fn compare_answers(actual: &ExpectedAnswer, expected: &ExpectedAnswer) -> bool {
    match (actual, expected) {
        (
            ExpectedAnswer::BeneficialOwners { owners: a },
            ExpectedAnswer::BeneficialOwners { owners: e },
        ) => {
            let mut sa = a.clone();
            let mut se = e.clone();
            sa.sort_by_key(|p| p.0);
            se.sort_by_key(|p| p.0);
            sa == se
        }
        (ExpectedAnswer::Decision { decision: a }, ExpectedAnswer::Decision { decision: e }) => {
            a == e
        }
        (
            ExpectedAnswer::IdentityAction { action: a },
            ExpectedAnswer::IdentityAction { action: e },
        ) => a == e,
        (
            ExpectedAnswer::Compatibility { result: a },
            ExpectedAnswer::Compatibility { result: e },
        ) => a == e,
        (ExpectedAnswer::Conflicts { conflicts: a }, ExpectedAnswer::Conflicts { conflicts: e }) => {
            a.len() == e.len()
        }
        (
            ExpectedAnswer::Exposure {
                exposure: a,
                ..
            },
            ExpectedAnswer::Exposure {
                exposure: e,
                ..
            },
        ) => a.direct == e.direct && a.indirect == e.indirect,
        (
            ExpectedAnswer::EntityState { state: a },
            ExpectedAnswer::EntityState { state: e },
        ) => {
            let mut ao = a.beneficial_owners.clone();
            let mut eo = e.beneficial_owners.clone();
            ao.sort_by_key(|p| p.0);
            eo.sort_by_key(|p| p.0);
            a.entity == e.entity && ao == eo && a.sanctioned == e.sanctioned
        }
        (
            ExpectedAnswer::Neighborhood { neighborhood: a },
            ExpectedAnswer::Neighborhood { neighborhood: e },
        ) => a.entity == e.entity && a.edges == e.edges,
        _ => false,
    }
}

fn classify_mismatch(
    metrics: &mut CorrectnessMetrics,
    expected: &ExpectedAnswer,
    actual: &ExpectedAnswer,
) {
    match (expected, actual) {
        (
            ExpectedAnswer::Decision {
                decision: Decision::Allow,
            },
            ExpectedAnswer::Decision {
                decision: Decision::Block,
            },
        ) => metrics.false_block += 1,
        (
            ExpectedAnswer::Decision {
                decision: Decision::Block,
            },
            ExpectedAnswer::Decision {
                decision: Decision::Allow,
            },
        ) => metrics.false_allow += 1,
        (
            ExpectedAnswer::Decision {
                decision: Decision::Review,
            },
            ExpectedAnswer::Decision { decision },
        ) if *decision != Decision::Review => metrics.false_review += 1,
        (
            ExpectedAnswer::IdentityAction {
                action: IdentityAction::Merge,
            },
            ExpectedAnswer::IdentityAction {
                action: IdentityAction::KeepSeparate,
            },
        ) => metrics.false_split += 1,
        (
            ExpectedAnswer::IdentityAction {
                action: IdentityAction::KeepSeparate,
            },
            ExpectedAnswer::IdentityAction {
                action: IdentityAction::Merge,
            },
        ) => metrics.false_merge += 1,
        (
            ExpectedAnswer::Neighborhood { neighborhood: e },
            ExpectedAnswer::Neighborhood { neighborhood: a },
        ) => {
            let seen: std::collections::HashSet<_> = a.edges.iter().collect();
            metrics.missed_relations +=
                e.edges.iter().filter(|edge| !seen.contains(edge)).count() as u64;
        }
        _ => metrics.incorrect_historical_replay += 1,
    }
}
