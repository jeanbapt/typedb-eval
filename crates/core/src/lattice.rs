use serde::{Deserialize, Serialize};

/// Evidence lattice: UNKNOWN < SUPPORTED, REFUTED; SUPPORTED ⊔ REFUTED = CONTRADICTORY
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvidenceState {
    #[default]
    Unknown,
    Supported,
    Refuted,
    Contradictory,
}

impl EvidenceState {
    pub fn join(self, other: Self) -> Self {
        join_evidence(self, other)
    }
}

pub fn join_evidence(a: EvidenceState, b: EvidenceState) -> EvidenceState {
    use EvidenceState::*;
    match (a, b) {
        (Unknown, x) | (x, Unknown) => x,
        (x, y) if x == y => x,
        (Supported, Refuted) | (Refuted, Supported) => Contradictory,
        (Contradictory, _) | (_, Contradictory) => Contradictory,
        _ => Contradictory,
    }
}

/// Governance ordering: OBSERVED < CORROBORATED < REVIEWED < FINAL
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GovernanceLevel {
    #[default]
    Observed,
    Corroborated,
    Reviewed,
    Final,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_join_contradiction() {
        assert_eq!(
            join_evidence(EvidenceState::Supported, EvidenceState::Refuted),
            EvidenceState::Contradictory
        );
    }

    #[test]
    fn evidence_join_identity() {
        assert_eq!(
            join_evidence(EvidenceState::Supported, EvidenceState::Supported),
            EvidenceState::Supported
        );
    }

    #[test]
    fn governance_ordering() {
        assert!(GovernanceLevel::Final > GovernanceLevel::Observed);
    }
}
