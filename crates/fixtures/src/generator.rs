use chrono::{Duration, TimeZone, Utc};
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use benchmark_core::{
    oracle::Oracle, Bitemporal, CompanyId, Context, Event, EvidenceState, FixtureBundle,
    GovernanceLevel, OntologyGeneration, PartyId, PersonId, Provenance, Role, Scale, TrustId,
};

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

struct ScaleParams {
    num_persons: usize,
    num_companies: usize,
    num_ownerships: usize,
    chain_targets: usize,
}

fn scale_params(scale: Scale) -> ScaleParams {
    match scale {
        Scale::S => ScaleParams {
            num_persons: 100,
            num_companies: 40,
            num_ownerships: 250,
            chain_targets: 12,
        },
        Scale::M => ScaleParams {
            num_persons: 300,
            num_companies: 120,
            num_ownerships: 800,
            chain_targets: 30,
        },
        Scale::L => ScaleParams {
            num_persons: 500,
            num_companies: 200,
            num_ownerships: 1000,
            chain_targets: 30,
        },
    }
}

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
    let params = scale_params(scale);
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

    let mut events: Vec<Event> = Vec::new();
    let mut persons: Vec<PersonId> = Vec::new();
    let mut companies: Vec<CompanyId> = Vec::new();

    // Compliance rules (~20)
    for i in 0..20 {
        let event = Event::ComplianceRule {
            rule_id: format!("rule_{i:02}"),
            description: format!("Beneficial ownership threshold rule {i}"),
            threshold_pct: 20.0 + (i as f32),
        };
        events.push(event);
    }

    // Register persons
    for i in 0..params.num_persons {
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
        let event = Event::RegisterPerson {
            id,
            name: name.clone(),
            canonical_name: canonical.clone(),
            jurisdiction: jurisdiction.to_string(),
            context,
            at: base_time + Duration::days(i as i64),
        };
        events.push(event);

        // Homonym / alias patterns — Q4 probe pairs registered on final oracle below.
        if i % 17 == 0 && i + 1 < params.num_persons {
            let alias_name = if first == "John" {
                "Jonathan".to_string() + " " + last
            } else {
                format!("{first} {last} Jr")
            };
            let alias_event = Event::IdentityAlias {
                person_a: id,
                alias: alias_name,
                canonical: canonical.clone(),
                merge: i % 34 == 0,
                context: Context::Kyc,
                at: base_time + Duration::days(i as i64 + 1),
            };
            events.push(alias_event);
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
    for i in 0..params.num_companies {
        let prefix = COMPANY_PREFIXES[i % COMPANY_PREFIXES.len()];
        let suffix = COMPANY_SUFFIXES[(i * 3) % COMPANY_SUFFIXES.len()];
        let name = format!("{prefix} {suffix}");
        let id = CompanyId::from_uuid(uuid_from_seed(seed, i as u64, 2));
        companies.push(id);
        let event = Event::RegisterCompany {
            id,
            name,
            jurisdiction: JURISDICTIONS[i % JURISDICTIONS.len()].to_string(),
            at: base_time + Duration::days(i as i64),
        };
        events.push(event);
    }

    // Guaranteed company-to-company ownership chains (depth 2–4) for first N targets.
    events.extend(emit_company_chains(
        seed,
        base_time,
        &params,
        &persons,
        &companies,
    ));

    // Ownership relations (~70% person→company, remainder company→company / mixed).
    let mut ownership_records: Vec<OwnershipRecord> = Vec::new();
    for i in 0..params.num_ownerships {
        let owned = companies[i % companies.len()];
        let owner = if i % 10 < 7 {
            PartyId::Person(persons[i % persons.len()])
        } else {
            PartyId::Company(companies[(i * 3 + 1) % companies.len()])
        };
        let share_pct = 10.0 + (i % 90) as f32;
        let event = build_ownership_event(
            i,
            owner,
            owned,
            share_pct,
            base_time,
            &mut rng,
            i % 5 == 0,
        );
        events.push(event.clone());
        ownership_records.push(OwnershipRecord {
            event,
            share_pct,
        });

        // Modified ownership
        if i % 75 == 0 {
            events.push(build_ownership_event(
                i + 10_000,
                PartyId::Person(persons[(i + 1) % persons.len()]),
                owned,
                share_pct + 5.0,
                base_time,
                &mut rng,
                false,
            ));
        }
    }

    // Sanctions listings
    for i in (0..params.num_persons).step_by(25) {
        let person = persons[i];
        let listed_at = base_time + Duration::days(100 + i as i64);
        let listing = Event::SanctionListing {
            person,
            list_name: "OFAC_SDN".into(),
            listed: true,
            context: Context::Sanctions,
            bitemporal: Bitemporal {
                valid_from: listed_at,
                valid_to: None,
                known_from: listed_at,
                known_to: if i % 5 == 0 {
                    Some(listed_at + Duration::days(120))
                } else {
                    None
                },
            },
        };
        events.push(listing);
        if i % 50 == 0 {
            let delisted_at = listed_at + Duration::days(180);
            let delisting = Event::SanctionListing {
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
            };
            events.push(delisting);
        }
    }

    // Contradictory sources — same predicate as ownership (`owns_{pct}`).
    for i in (0..params.num_ownerships).step_by(40) {
        let record = &ownership_records[i];
        let Event::AssertOwnership {
            owner,
            owned,
            share_pct,
            ..
        } = &record.event
        else {
            continue;
        };
        let share = *share_pct;
        let at = base_time + Duration::days(200 + i as i64);
        let contradiction = Event::ContradictorySource {
            subject: owner.entity(),
            predicate: owns_predicate(share),
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
        };
        events.push(contradiction);
    }

    // Late arrivals with future known_from (valid in the past).
    for i in (0..params.num_persons).step_by(30) {
        let person = persons[i];
        let company = companies[i % companies.len()];
        let share_pct = 15.0 + (i % 50) as f32;
        let late_at = base_time + Duration::days(500 + i as i64);
        let late = Event::LateArrival {
            subject: person.entity(),
            predicate: owns_predicate(share_pct),
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
        };
        events.push(late);
    }

    // Expand to target scale by replaying with variations (closure events reserved later).
    let target = scale.event_count();
    let mut expansion_idx = 0u64;
    while events.len() < target {
        let idx = rng.gen_range(0..events.len().min(1000));
        if let Some(cloned) = clone_event_with_variation(&events[idx], &mut rng, base_time) {
            events.push(cloned);
        } else if !persons.is_empty() && !companies.is_empty() {
            let owner = persons[expansion_idx as usize % persons.len()];
            let owned = companies[expansion_idx as usize % companies.len()];
            let share_pct = 20.0 + (expansion_idx % 60) as f32;
            let day_offset = 600 + (expansion_idx % 500) as i64;
            let at = base_time + Duration::days(day_offset);
            let late = Event::LateArrival {
                subject: owner.entity(),
                predicate: owns_predicate(share_pct),
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
            };
            events.push(late);
            expansion_idx += 1;
        } else {
            break;
        }
    }

    // Shuffle only non-registration events to preserve FK order during ingest.
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

    // Reserve space for knowledge-closure events that reference real assertion IDs.
    let structural_len = structural.len();
    let mut lo = 0usize;
    let mut hi = data_events.len();
    let mut best_data = 0usize;
    let mut best_closure = Vec::new();
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let mut trial = structural.clone();
        trial.extend(data_events.iter().take(mid).cloned());
        let closure = emit_knowledge_closure_events(base_time, &Oracle::from_events(&trial));
        if structural_len + mid + closure.len() <= target {
            best_data = mid;
            best_closure = closure;
            lo = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            hi = mid - 1;
        }
    }

    let mut events = structural;
    events.extend(data_events.into_iter().take(best_data));
    events.extend(best_closure);

    if generation.is_extended() {
        events.extend(extension_events(seed, base_time, &persons, &companies));
    }

    let mut oracle = Oracle::from_events(&events);
    for i in (0..params.num_persons).step_by(17) {
        if i + 1 < persons.len() {
            oracle.register_identity_probe_pair(persons[i], persons[i + 1]);
        }
    }
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

struct OwnershipRecord {
    event: Event,
    share_pct: f32,
}

fn owns_predicate(share_pct: f32) -> String {
    format!("owns_{share_pct}")
}

fn build_ownership_event(
    i: usize,
    owner: PartyId,
    owned: CompanyId,
    share_pct: f32,
    base_time: chrono::DateTime<Utc>,
    rng: &mut ChaCha8Rng,
    close_known: bool,
) -> Event {
    let source = SOURCES[i % SOURCES.len()];
    let valid_from = base_time + Duration::days((i % 300) as i64);
    let known_from = valid_from + Duration::days(rng.gen_range(0..30));
    let known_to = if close_known {
        Some(known_from + Duration::days(60 + (i % 90) as i64))
    } else {
        None
    };

    Event::AssertOwnership {
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
            known_to,
        },
    }
}

fn emit_company_chains(
    seed: u64,
    base_time: chrono::DateTime<Utc>,
    params: &ScaleParams,
    persons: &[PersonId],
    companies: &[CompanyId],
) -> Vec<Event> {
    if companies.len() < 4 || persons.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let chain_targets = params.chain_targets.min(companies.len());

    for target_idx in 0..chain_targets {
        let depth = 2 + (target_idx % 3);
        let target = companies[target_idx];
        let mut chain: Vec<CompanyId> = Vec::with_capacity(depth);
        for hop in 0..depth {
            let owner_idx = (target_idx + hop + 1) % companies.len();
            if companies[owner_idx] != target {
                chain.push(companies[owner_idx]);
            }
        }
        if chain.is_empty() {
            chain.push(companies[(target_idx + 1) % companies.len()]);
        }

        let mut current = target;
        for (hop, owner_co) in chain.iter().enumerate().take(depth - 1) {
            let share_pct = 40.0 + (target_idx + hop) as f32;
            let valid_from = base_time + Duration::days(20 + target_idx as i64 + hop as i64);
            let known_from = valid_from + Duration::days(1);
            let event = Event::AssertOwnership {
                owner: PartyId::Company(*owner_co),
                owned: current,
                share_pct,
                evidence: EvidenceState::Supported,
                governance: GovernanceLevel::Reviewed,
                context: Context::CorporateRegistry,
                role: Role::Controller,
                jurisdiction: JURISDICTIONS[target_idx % JURISDICTIONS.len()].into(),
                provenance: Provenance {
                    source_id: format!("registrar:chain:{target_idx}:{hop}"),
                    source_authority: 0.9,
                    observed_at: known_from,
                },
                bitemporal: Bitemporal {
                    valid_from,
                    valid_to: None,
                    known_from,
                    known_to: if target_idx % 5 == 0 {
                        Some(known_from + Duration::days(90))
                    } else {
                        None
                    },
                },
            };
            out.push(event);
            current = *owner_co;
        }

        // Terminal beneficial owner (person) at top of chain.
        let person = persons[(target_idx * 5) % persons.len()];
        let share_pct = 55.0 + (target_idx % 20) as f32;
        let valid_from = base_time + Duration::days(15 + target_idx as i64);
        let known_from = valid_from + Duration::days(2);
        let top = Event::AssertOwnership {
            owner: PartyId::Person(person),
            owned: current,
            share_pct,
            evidence: EvidenceState::Supported,
            governance: GovernanceLevel::Final,
            context: Context::Kyc,
            role: Role::BeneficialOwner,
            jurisdiction: JURISDICTIONS[target_idx % JURISDICTIONS.len()].into(),
            provenance: Provenance {
                source_id: format!("kyc_vendor:chain_top:{target_idx}"),
                source_authority: 0.88,
                observed_at: known_from,
            },
            bitemporal: Bitemporal {
                valid_from,
                valid_to: None,
                known_from,
                known_to: None,
            },
        };
        out.push(top);

        let _ = seed;
    }

    out
}

fn emit_knowledge_closure_events(
    base_time: chrono::DateTime<Utc>,
    oracle: &Oracle,
) -> Vec<Event> {
    let assertion_ids = oracle.assertion_ids();
    if assertion_ids.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();

    // CloseAssertionKnowledge on ~20% of ingested assertions.
    for (step, assertion_id) in assertion_ids.iter().enumerate().step_by(5) {
        let Some(a) = oracle.get_assertion(*assertion_id) else {
            continue;
        };
        // known_to must be strictly after known_from or Postgres range updates fail.
        let known_to = a.bitemporal.known_from + Duration::days(30 + (step % 60) as i64);
        out.push(Event::CloseAssertionKnowledge {
            assertion_id: *assertion_id,
            known_to,
        });
    }

    // RetroactiveCorrection referencing real assertion IDs.
    for (step, assertion_id) in assertion_ids.iter().enumerate().step_by(7) {
        let Some(a) = oracle.get_assertion(*assertion_id) else {
            continue;
        };
        let corrected_at = a.bitemporal.known_from + Duration::days(60 + (step % 90) as i64);
        out.push(Event::RetroactiveCorrection {
            assertion_id: *assertion_id,
            new_valid_from: base_time + Duration::days(10),
            corrected_at,
        });
    }

    out
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
    let mut events = Vec::new();

    let trusts: Vec<TrustId> = (0..num_trusts)
        .map(|i| TrustId::from_uuid(uuid_from_seed(seed, i as u64, 4)))
        .collect();

    for (i, trust) in trusts.iter().enumerate() {
        let event = Event::RegisterTrust {
            id: *trust,
            name: format!("Trust {i:03}"),
            jurisdiction: JURISDICTIONS[i % JURISDICTIONS.len()].into(),
            at: base_time + Duration::days(5),
        };
        events.push(event);
    }

    for (i, company) in companies.iter().enumerate() {
        let controller = match i % 3 {
            0 => persons[i % persons.len()].entity(),
            1 => companies[(i + 1) % companies.len()].entity(),
            _ => trusts[i % trusts.len()].entity(),
        };
        let at = base_time + Duration::days(120 + (i % 300) as i64);
        let event = Event::ControlViaNominee {
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
        };
        events.push(event);
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
    _base_time: chrono::DateTime<Utc>,
) -> Option<Event> {
    match event {
        Event::RegisterPerson { .. } => None,
        Event::RegisterCompany { .. } => None,
        Event::ComplianceRule { .. } => None,
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
                source_id: format!("{}/v{}", provenance.source_id, rng.gen::<u32>()),
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
        Event::ContradictorySource {
            subject,
            predicate,
            object,
            supporting,
            refuting,
            context,
            provenance,
            bitemporal,
        } => Some(Event::ContradictorySource {
            subject: *subject,
            predicate: predicate.clone(),
            object: *object,
            supporting: *supporting,
            refuting: *refuting,
            context: *context,
            provenance: Provenance {
                source_id: provenance.source_id.clone(),
                source_authority: provenance.source_authority,
                observed_at: provenance.observed_at + Duration::hours(rng.gen_range(1..24)),
            },
            bitemporal: Bitemporal {
                valid_from: bitemporal.valid_from + Duration::hours(rng.gen_range(1..12)),
                valid_to: bitemporal.valid_to,
                known_from: bitemporal.known_from + Duration::hours(rng.gen_range(1..12)),
                known_to: bitemporal.known_to,
            },
        }),
        Event::LateArrival {
            subject,
            predicate,
            object,
            evidence,
            context,
            provenance,
            bitemporal,
        } => Some(Event::LateArrival {
            subject: *subject,
            predicate: predicate.clone(),
            object: *object,
            evidence: *evidence,
            context: *context,
            provenance: Provenance {
                source_id: provenance.source_id.clone(),
                source_authority: provenance.source_authority,
                observed_at: provenance.observed_at + Duration::days(rng.gen_range(1..5)),
            },
            bitemporal: Bitemporal {
                valid_from: bitemporal.valid_from + Duration::days(rng.gen_range(1..5)),
                valid_to: bitemporal.valid_to,
                known_from: bitemporal.known_from + Duration::days(rng.gen_range(1..5)),
                known_to: bitemporal.known_to,
            },
        }),
        Event::RetroactiveCorrection { .. } | Event::CloseAssertionKnowledge { .. } => None,
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

    #[test]
    fn postgres_bound_assertion_ids_are_unique() {
        use std::collections::HashSet;

        use benchmark_core::{AssertionId, Bitemporal, Event};

        let bundle = generate_fixtures(42, Scale::S);
        let mut seen = HashSet::new();

        for event in &bundle.events {
            match event {
                Event::AssertOwnership {
                    owner,
                    owned,
                    share_pct,
                    bitemporal,
                    provenance,
                    ..
                } => {
                    let predicate = format!("owns_{share_pct}");
                    let id = AssertionId::deterministic(
                        owner.entity(),
                        &predicate,
                        owned.entity(),
                        bitemporal,
                        0,
                        &format!("{}@{}", provenance.source_id, provenance.observed_at.timestamp()),
                    );
                    assert!(seen.insert(id), "duplicate ownership id {:?}", id.0);
                }
                Event::LateArrival {
                    subject,
                    predicate,
                    object,
                    bitemporal,
                    provenance,
                    ..
                } if predicate.starts_with("owns_") => {
                    let id = AssertionId::deterministic(
                        *subject,
                        predicate,
                        *object,
                        bitemporal,
                        1,
                        &format!("{}@{}", provenance.source_id, provenance.observed_at.timestamp()),
                    );
                    assert!(seen.insert(id), "duplicate late ownership id {:?}", id.0);
                }
                _ => {}
            }
        }
    }

    #[test]
    fn postgres_bound_assertion_ids_are_unique_m() {
        use std::collections::HashSet;

        use benchmark_core::{AssertionId, Event};

        let bundle = generate_fixtures(42, Scale::M);
        let mut seen = HashSet::new();

        for event in &bundle.events {
            match event {
                Event::AssertOwnership {
                    owner,
                    owned,
                    share_pct,
                    bitemporal,
                    provenance,
                    ..
                } => {
                    let predicate = format!("owns_{share_pct}");
                    let id = AssertionId::deterministic(
                        owner.entity(),
                        &predicate,
                        owned.entity(),
                        bitemporal,
                        0,
                        &format!("{}@{}", provenance.source_id, provenance.observed_at.timestamp()),
                    );
                    assert!(seen.insert(id), "duplicate ownership id {:?}", id.0);
                }
                Event::LateArrival {
                    subject,
                    predicate,
                    object,
                    bitemporal,
                    provenance,
                    ..
                } if predicate.starts_with("owns_") => {
                    let id = AssertionId::deterministic(
                        *subject,
                        predicate,
                        *object,
                        bitemporal,
                        1,
                        &format!("{}@{}", provenance.source_id, provenance.observed_at.timestamp()),
                    );
                    assert!(seen.insert(id), "duplicate late ownership id {:?}", id.0);
                }
                _ => {}
            }
        }
    }

    #[test]
    fn retroactive_new_ids_are_unique() {
        use std::collections::HashSet;

        use benchmark_core::{AssertionId, Bitemporal, Event, oracle::Oracle};

        let bundle = generate_fixtures(42, Scale::S);
        let oracle = Oracle::from_events(&bundle.events);
        let mut seen = HashSet::new();
        for event in &bundle.events {
            let Event::RetroactiveCorrection {
                assertion_id,
                new_valid_from,
                corrected_at,
            } = event
            else {
                continue;
            };
            let Some(old) = oracle.get_assertion(*assertion_id) else {
                continue;
            };
            let new_id = AssertionId::deterministic(
                old.subject,
                &old.predicate,
                old.object,
                &Bitemporal {
                    valid_from: *new_valid_from,
                    valid_to: old.bitemporal.valid_to,
                    known_from: *corrected_at,
                    known_to: None,
                },
                0,
                &format!("retro:{}@{}", assertion_id.0, corrected_at.timestamp()),
            );
            assert!(seen.insert(new_id), "duplicate retro id {:?}", new_id.0);
        }
    }

    #[test]
    fn ownership_uses_party_id_and_chains() {
        let bundle = generate_fixtures(7, Scale::S);
        let company_owners = bundle
            .events
            .iter()
            .filter_map(|e| match e {
                Event::AssertOwnership {
                    owner: PartyId::Company(_),
                    ..
                } => Some(()),
                _ => None,
            })
            .count();
        assert!(company_owners >= 10, "expected company-to-company ownership");
    }
}
