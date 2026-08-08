use serde::{Deserialize, Serialize};

/// Dimensions that can be ablated in the benchmark (PRD §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AblationDimension {
    None,
    Identity,
    Evidence,
    ValidTime,
    KnowledgeTime,
    Jurisdiction,
    Role,
    Source,
    SourceAuthority,
    Provenance,
    Governance,
}

impl AblationDimension {
    pub fn all_ablatable() -> &'static [AblationDimension] {
        &[
            AblationDimension::Identity,
            AblationDimension::Evidence,
            AblationDimension::ValidTime,
            AblationDimension::KnowledgeTime,
            AblationDimension::Jurisdiction,
            AblationDimension::Role,
            AblationDimension::Source,
            AblationDimension::SourceAuthority,
            AblationDimension::Provenance,
            AblationDimension::Governance,
        ]
    }

    pub fn name(self) -> &'static str {
        match self {
            AblationDimension::None => "none",
            AblationDimension::Identity => "identity",
            AblationDimension::Evidence => "evidence",
            AblationDimension::ValidTime => "valid_time",
            AblationDimension::KnowledgeTime => "knowledge_time",
            AblationDimension::Jurisdiction => "jurisdiction",
            AblationDimension::Role => "role",
            AblationDimension::Source => "source",
            AblationDimension::SourceAuthority => "source_authority",
            AblationDimension::Provenance => "provenance",
            AblationDimension::Governance => "governance",
        }
    }
}
