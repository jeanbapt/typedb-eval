use async_trait::async_trait;

use crate::error::Result;
use crate::types::*;

/// Common store interface for both backends (PRD §17).
#[async_trait]
pub trait ComplianceStore: Send + Sync {
    async fn ingest(&mut self, event: Event) -> Result<StateDelta>;

    async fn state_at(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<EntityState>;

    async fn contradictions(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<Conflict>>;

    async fn ownership_exposure(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Exposure>;

    async fn compliance_decision(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Decision>;

    async fn beneficial_owners(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Vec<PersonId>> {
        Ok(self
            .state_at(entity, valid_at, known_at)
            .await?
            .beneficial_owners)
    }

    async fn identity_action(
        &self,
        person_a: PersonId,
        person_b: PersonId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<IdentityAction>;

    async fn context_compatibility(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Compatibility>;

    /// Q9 — every relation the entity participates in, whatever the relation type or role.
    ///
    /// Implementations MUST be written against the base ontology and MUST NOT be edited
    /// when the ontology is extended; the schema-evolution experiment measures exactly
    /// how much of the extended ontology each backend still sees through this query.
    async fn neighborhood(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Neighborhood>;

    /// Repaired variant of [`ComplianceStore::neighborhood`] that has been explicitly
    /// updated for the extended ontology. Backends that need no repair return the same
    /// result as `neighborhood`; the LOC difference between the two is the repair cost.
    async fn neighborhood_repaired(
        &self,
        entity: EntityId,
        valid_at: Timestamp,
        known_at: Timestamp,
    ) -> Result<Neighborhood> {
        self.neighborhood(entity, valid_at, known_at).await
    }

    /// Non-blank lines of query code that had to be added so that Q9 sees the extended
    /// ontology. Zero when the original query needed no repair.
    fn query_repair_loc(&self) -> u64 {
        0
    }

    async fn reset(&mut self) -> Result<()>;
}
