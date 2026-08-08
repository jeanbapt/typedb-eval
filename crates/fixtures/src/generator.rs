use chrono::{Duration, TimeZone, Utc};
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use benchmark_core::{
    oracle::Oracle, Bitemporal, Context, Event, FixtureBundle, GovernanceLevel, OntologyGeneration,
    PersonId, Provenance, Role, Scale, TrustId,
};
use benchmark_core::{CompanyId, EvidenceState};

const JURISDICTIONS: &[&str] = &["US", "UK", "EU", "SG"];
const SOURCES: &[&str] = &["registrar", "kyc_vendor", "sanctions_db", "news_feed", "manual_review"];

const FIRST_NAMES: &[&str] = &[
    "John", "Jonathan", "Jane", "James", "Maria", "Mohammed", "Wei", "Anna", "Carlos", "Yuki",
];
const LAST_NAMES: &[&str] = &[
    "Smith", "Johnson", "Williams", "Brown", "Jones", "Garcia", "Miller", "Davis", "Wilson",
    "Taylor",
];
const COMPANY_PREFIXES: &[&str] = &["Acme", "Global", "Pacific", "Atlantic", "Northern", "Southern"];
const COMPANY_SUFFIXES: &[&str] = &["Ltd", "Limited", "LLC", "Inc", "Corp", "GmbH"];

pub fn generate_fixtures(seed: u64, scale: Scale) -> FixtureBundle {
    generate_fixtures_with(seed, scale, OntologyGeneration::Base)
}

/// Generate fixtures for a given ontology generation.
///
/// The base events are byte-identical across generations: extension events are appended
/// after truncation and shuffling, and draw from a separate RNG stream. Any measured
/// difference between generations is therefore attributable to the ontology change alone.
pub fn generate_fixtures_with(
    seed: u64,
    scale: Scale,
    generation: OntologyGeneration,
) -> FixtureBundle {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    let num_companies = 200;
    let num_persons = 500;
    let num_ownerships = 1000;

    let mut events: Vec<Event> = Vec::new();
    let mut persons: Vec<PersonId> = Vec::new();
    let mut companies: Vec<CompanyId> = Vec::new();

    // Compliance rules (~20)
    for i in 0..20 {
        events.push(Event::ComplianceRule {
            rule_id: format!("rule_{i:02}"),
            description: format!("Beneficial ownership threshold rule {i}"),
            threshold_pct: 20.0 + (i as f32),
        });
    }

    // Register persons
    for i in 0..num_persons {
        let first = FIRST_NAMES[i % FIRST_NAMES.len()];
        let last = LAST_NAMES[(i * 7 + 3) % LAST_NAMES.len()];
        let name = format!("{first} {last}");
        let canonical = name.to_lowercase();
        let id = PersonId::from_uuid(uuid_from_seed(seed, i as u64, 1));
        persons.push(id);
        let jurisdiction = JURISDICTIONS[i % JURISDICTIONS.len()];
        let context = match i % 3 {
            0 => Context::CorporateRegistry,
            1 => Context::Kyc,
            _ => Context::Sanctions,
        };
        events.push(Event::RegisterPerson {
            id,
            name: name.clone(),
            canonical_name: canonical.clone(),
            jurisdiction: jurisdiction.to_string(),
            context,
            at: base_time + Duration::days(i as i64),
        });

        // Homonym / alias patterns
        if i % 17 == 0 && i + 1 < num_persons {
            let alias_name = if first == "John" {
                "Jonathan".to_string() + " " + last
            } else {
                format!("{first} {last} Jr")
            };
            events.push(Event::IdentityAlias {
                person_a: id,
                alias: alias_name,
                canonical: canonical.clone(),
                merge: i % 34 == 0,
                context: Context::Kyc,
                at: base_time + Duration::days(i as i64 + 1),
            });
        }

        // Paraphrase already canonicalized (semantic churn test)
        if i % 23 == 0 {
            events.push(Event::IdentityAlias {
                person_a: id,
                alias: name.to_uppercase(),
                canonical: canonical.clone(),
                merge: true,
                context: Context::CorporateRegistry,
                at: base_time + Duration::days(i as i64 + 2),
            });
        }
    }

    // Register companies
    for i in 0..num_companies {
        let prefix = COMPANY_PREFIXES[i % COMPANY_PREFIXES.len()];
        let suffix = COMPANY_SUFFIXES[(i * 3) % COMPANY_SUFFIXES.len()];
        let name = format!("{prefix} {suffix}");
        let id = CompanyId::from_uuid(uuid_from_seed(seed, i as u64, 2));
        companies.push(id);
        events.push(Event::RegisterCompany {
            id,
            name,
            jurisdiction: JURISDICTIONS[i % JURISDICTIONS.len()].to_string(),
            at: base_time + Duration::days(i as i64),
        });
    }

    // Ownership relations
    for i in 0..num_ownerships {
        let owner = persons[i % persons.len()];
        let owned = companies[i % companies.len()];
        let share_pct = 10.0 + (i % 90) as f32;
        let source = SOURCES[i % SOURCES.len()];
        let valid_from = base_time + Duration::days((i % 300) as i64);
        let known_from = valid_from + Duration::days(rng.gen_range(0..30));

        events.push(Event::AssertOwnership {
            owner,
            owned,
            share_pct,
            evidence: if i % 50 == 0 {
                EvidenceState::Refuted
            } else {
                EvidenceState::Supported
            },
            governance: match i % 4 {
                0 => GovernanceLevel::Observed,
                1 => GovernanceLevel::Corroborated,
                2 => GovernanceLevel::Reviewed,
                _ => GovernanceLevel::Final,
            },
            context: match i % 3 {
                0 => Context::CorporateRegistry,
                1 => Context::Kyc,
                _ => Context::Sanctions,
            },
            role: match i % 4 {
                0 => Role::BeneficialOwner,
                1 => Role::Director,
                2 => Role::Shareholder,
                _ => Role::Controller,
            },
            jurisdiction: JURISDICTIONS[i % JURISDICTIONS.len()].to_string(),
            provenance: Provenance {
                source_id: source.to_string(),
                source_authority: 0.5 + (i % 50) as f32 / 100.0,
                observed_at: known_from,
            },
            bitemporal: Bitemporal {
                valid_from,
                valid_to: if i % 100 == 0 {
                    Some(valid_from + Duration::days(60))
                } else {
                    None
                },
                known_from,
                known_to: None,
            },
        });

        // Modified ownership
        if i % 75 == 0 {
            events.push(Event::AssertOwnership {
                owner: persons[(i + 1) % persons.len()],
                owned,
                share_pct: share_pct + 5.0,
                evidence: EvidenceState::Supported,
                governance: GovernanceLevel::Final,
                context: Context::CorporateRegistry,
                role: Role::BeneficialOwner,
                jurisdiction: JURISDICTIONS[i % JURISDICTIONS.len()].to_string(),
                provenance: Provenance {
                    source_id: "registrar".into(),
                    source_authority: 0.95,
                    observed_at: known_from + Duration::days(90),
                },
                bitemporal: Bitemporal {
                    valid_from: valid_from + Duration::days(90),
                    valid_to: None,
                    known_from: known_from + Duration::days(90),
                    known_to: None,
                },
            });
        }
    }

    // Sanctions listings
    for i in (0..num_persons).step_by(25) {
        let person = persons[i];
        let listed_at = base_time + Duration::days(100 + i as i64);
        events.push(Event::SanctionListing {
            person,
            list_name: "OFAC_SDN".into(),
            listed: true,
            context: Context::Sanctions,
            bitemporal: Bitemporal {
                valid_from: listed_at,
                valid_to: None,
                known_from: listed_at,
                known_to: None,
            },
        });
        // Delisting for some
        if i % 50 == 0 {
            let delisted_at = listed_at + Duration::days(180);
            events.push(Event::SanctionListing {
                person,
                list_name: "OFAC_SDN".into(),
                listed: false,
                context: Context::Sanctions,
                bitemporal: Bitemporal {
                    valid_from: delisted_at,
                    valid_to: None,
                    known_from: delisted_at,
                    known_to: None,
                },
            });
        }
    }

    // Contradictory sources
    for i in (0..num_ownerships).step_by(40) {
        let owner = persons[i % persons.len()];
        let owned = companies[i % companies.len()];
        let at = base_time + Duration::days(200 + i as i64);
        events.push(Event::ContradictorySource {
            subject: owner.entity(),
            predicate: "owns".into(),
            object: owned.entity(),
            supporting: EvidenceState::Supported,
            refuting: EvidenceState::Refuted,
            context: Context::Kyc,
            provenance: Provenance {
                source_id: "kyc_vendor".into(),
                source_authority: 0.7,
                observed_at: at,
            },
            bitemporal: Bitemporal {
                valid_from: at,
                valid_to: None,
                known_from: at,
                known_to: None,
            },
        });
    }

    // Retroactive corrections
    for i in (0..50).step_by(5) {
        if i < events.len() {
            events.push(Event::RetroactiveCorrection {
                assertion_id: benchmark_core::AssertionId::new(),
                new_valid_from: base_time + Duration::days(10),
                corrected_at: base_time + Duration::days(400 + i as i64),
            });
        }
    }

    // Late arrivals
    for i in (0..num_persons).step_by(30) {
        let person = persons[i];
        let company = companies[i % companies.len()];
        let late_at = base_time + Duration::days(500 + i as i64);
        events.push(Event::LateArrival {
            subject: person.entity(),
            predicate: "owns".into(),
            object: company.entity(),
            evidence: EvidenceState::Supported,
            context: Context::CorporateRegistry,
            provenance: Provenance {
                source_id: "manual_review".into(),
                source_authority: 0.8,
                observed_at: late_at,
            },
            bitemporal: Bitemporal {
                valid_from: base_time + Duration::days(50),
                valid_to: None,
                known_from: late_at,
                known_to: None,
            },
        });
    }

    // Expand to target scale by replaying with variations
    let target = scale.event_count();
    let mut expansion_idx = 0u64;
    while events.len() < target {
        let idx = rng.gen_range(0..events.len().min(1000));
        if let Some(cloned) = clone_event_with_variation(&events[idx], &mut rng, base_time) {
            events.push(cloned);
        } else if !persons.is_empty() && !companies.is_empty() {
            // Synthetic padding events for scale M/L
            let owner = persons[expansion_idx as usize % persons.len()];
            let owned = companies[expansion_idx as usize % companies.len()];
            let day_offset = 600 + (expansion_idx % 500) as i64;
            let at = base_time + Duration::days(day_offset);
            events.push(Event::LateArrival {
                subject: owner.entity(),
                predicate: "owns".into(),
                object: owned.entity(),
                evidence: EvidenceState::Supported,
                context: Context::Kyc,
                provenance: Provenance {
                    source_id: format!("padding_{expansion_idx}"),
                    source_authority: 0.6,
                    observed_at: at,
                },
                bitemporal: Bitemporal {
                    valid_from: base_time + Duration::days(day_offset - 30),
                    valid_to: None,
                    known_from: at,
                    known_to: None,
                },
            });
            expansion_idx += 1;
        } else {
            break;
        }
    }
    events.truncate(target);

    // Shuffle only non-registration events to preserve FK order during ingest
    let mut structural: Vec<Event> = Vec::new();
    let mut data_events: Vec<Event> = Vec::new();
    for event in events {
        match &event {
            Event::RegisterPerson { .. }
            | Event::RegisterCompany { .. }
            | Event::ComplianceRule { .. } => structural.push(event),
            _ => data_events.push(event),
        }
    }
    data_events.shuffle(&mut rng);
    events = structural;
    events.extend(data_events);

    if generation.is_extended() {
        events.extend(extension_events(seed, base_time, &persons, &companies));
    }

    // Build oracle and probes
    let oracle = Oracle::from_events(&events);
    let probe_count = 50.min(companies.len());
    let probes = oracle.generate_probes(probe_count);
    let expected = oracle.compute_expected(&probes).expect("oracle compute");

    FixtureBundle {
        seed,
        scale,
        generation,
        events,
        probes,
        expected,
    }
}

/// Events introduced by the ontology extension: a new party kind (`trust`) and a 4-ary
/// `control-via-nominee` relation whose `controller` role is polymorphic over
/// person / company / trust.
///
/// Every company gets exactly one control relation so that Q9 coverage does not depend on
/// which entities the probe generator happens to select.
fn extension_events(
    seed: u64,
    base_time: chrono::DateTime<Utc>,
    persons: &[PersonId],
    companies: &[CompanyId],
) -> Vec<Event> {
    if persons.is_empty() || companies.is_empty() {
        return Vec::new();
    }

    let mut rng = ChaCha8Rng::seed_from_u64(seed ^ 0x5EED_1CE5_u64);
    let num_trusts = 50usize;
    let mut events = Vec::with_capacity(num_trusts + companies.len());

    let trusts: Vec<TrustId> = (0..num_trusts)
        .map(|i| TrustId::from_uuid(uuid_from_seed(seed, i as u64, 4)))
        .collect();

    for (i, trust) in trusts.iter().enumerate() {
        events.push(Event::RegisterTrust {
            id: *trust,
            name: format!("Trust {i:03}"),
            jurisdiction: JURISDICTIONS[i % JURISDICTIONS.len()].into(),
            at: base_time + Duration::days(5),
        });
    }

    for (i, company) in companies.iter().enumerate() {
        // Rotate the controller across all three party kinds so the polymorphic role is
        // genuinely exercised rather than being a person-only role in disguise.
        let controller = match i % 3 {
            0 => persons[i % persons.len()].entity(),
            1 => companies[(i + 1) % companies.len()].entity(),
            _ => trusts[i % trusts.len()].entity(),
        };
        let at = base_time + Duration::days(120 + (i % 300) as i64);
        events.push(Event::ControlViaNominee {
            controller,
            controlled: *company,
            nominee: persons[(i * 7) % persons.len()],
            instrument: trusts[i % trusts.len()],
            context: Context::CorporateRegistry,
            jurisdiction: JURISDICTIONS[i % JURISDICTIONS.len()].into(),
            provenance: Provenance {
                source_id: SOURCES[rng.gen_range(0..SOURCES.len())].into(),
                source_authority: 0.85,
                observed_at: at,
            },
            bitemporal: Bitemporal {
                valid_from: base_time + Duration::days(10),
                valid_to: None,
                known_from: at,
                known_to: None,
            },
        });
    }

    events
}

fn uuid_from_seed(seed: u64, index: u64, namespace: u64) -> uuid::Uuid {
    let bytes = [
        ((seed >> 56) & 0xFF) as u8,
        ((seed >> 48) & 0xFF) as u8,
        ((seed >> 40) & 0xFF) as u8,
        ((seed >> 32) & 0xFF) as u8,
        ((seed >> 24) & 0xFF) as u8,
        ((seed >> 16) & 0xFF) as u8,
        ((index >> 8) & 0xFF) as u8,
        (index & 0xFF) as u8,
        ((namespace >> 8) & 0xFF) as u8,
        (namespace & 0xFF) as u8,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    uuid::Uuid::from_bytes(bytes)
}

fn clone_event_with_variation(
    event: &Event,
    rng: &mut ChaCha8Rng,
    base_time: chrono::DateTime<Utc>,
) -> Option<Event> {
    match event {
        Event::RegisterPerson { .. } => None,
        Event::RegisterCompany { .. } => None,
        Event::ComplianceRule { .. } => None,
        // Extension events are appended verbatim after padding, never cloned, so that the
        // base event stream stays identical across ontology generations.
        Event::RegisterTrust { .. } => None,
        Event::ControlViaNominee { .. } => None,
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
        } => Some(Event::AssertOwnership {
            owner: *owner,
            owned: *owned,
            share_pct: *share_pct,
            evidence: *evidence,
            governance: *governance,
            context: *context,
            role: *role,
            jurisdiction: jurisdiction.clone(),
            provenance: Provenance {
                source_id: provenance.source_id.clone(),
                source_authority: provenance.source_authority,
                observed_at: provenance.observed_at + Duration::hours(rng.gen_range(1..48)),
            },
            bitemporal: Bitemporal {
                valid_from: bitemporal.valid_from + Duration::hours(rng.gen_range(1..24)),
                valid_to: bitemporal.valid_to,
                known_from: bitemporal.known_from + Duration::hours(rng.gen_range(1..24)),
                known_to: bitemporal.known_to,
            },
        }),
        Event::IdentityAlias {
            person_a,
            alias,
            canonical,
            merge,
            context,
            at,
        } => Some(Event::IdentityAlias {
            person_a: *person_a,
            alias: format!("{alias}_dup"),
            canonical: canonical.clone(),
            merge: *merge,
            context: *context,
            at: *at + Duration::hours(rng.gen_range(1..12)),
        }),
        Event::SanctionListing {
            person,
            list_name,
            listed,
            context,
            bitemporal,
        } => Some(Event::SanctionListing {
            person: *person,
            list_name: list_name.clone(),
            listed: *listed,
            context: *context,
            bitemporal: Bitemporal {
                valid_from: bitemporal.valid_from + Duration::days(rng.gen_range(1..10)),
                valid_to: bitemporal.valid_to,
                known_from: bitemporal.known_from + Duration::days(rng.gen_range(1..10)),
                known_to: bitemporal.known_to,
            },
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_generation() {
        let a = generate_fixtures(42, Scale::S);
        let b = generate_fixtures(42, Scale::S);
        assert_eq!(a.events.len(), b.events.len());
        assert_eq!(a.seed, b.seed);
    }

    #[test]
    fn scale_event_counts() {
        let s = generate_fixtures(1, Scale::S);
        assert_eq!(s.events.len(), 1000);
        let m = generate_fixtures(1, Scale::M);
        assert_eq!(m.events.len(), 20000);
    }
}
