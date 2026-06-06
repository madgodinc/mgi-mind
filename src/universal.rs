//! The two universal coordinators ("the brothers").
//!
//! mgi-mind's memory is several specialist silos that today don't talk:
//! `memories` (vector search), `facts` (triples + duel/staleness), `procedures`
//! (error->fix), sessions. Each is good at its job; none has a view across the
//! others. Two gaps follow:
//!
//!   1. No memory-side coordinator: nothing reconciles signals across silos
//!      (a fact going stale should dim the related memories; a procedure that
//!      fired should lift the confidence of the facts it used). Search over
//!      `memories` literally cannot see `facts` staleness today.
//!   2. No retrieval-side coordinator: the "hybrid" search (dense + sparse +
//!      RRF + rerank) is the 2024 industry default, not a differentiator, and
//!      it only searches the `memories` silo.
//!
//! This module defines the two coordinators and — crucially — the link between
//! them. They are SIBLINGS that help each other:
//!
//!   MemoryCoordinator  — owns "what do we know", across all silos. Knows which
//!                        items are active/stale/superseded, how items relate,
//!                        and can answer "give me everything about X" as one
//!                        view instead of four separate queries.
//!   RetrievalCoordinator — owns "how do we find it". Fuses many retrieval axes
//!                        (dense, sparse, graph/relational, temporal/validity)
//!                        — the "hybrid of hybrids" — over whatever the memory
//!                        coordinator exposes.
//!
//! The pairing: RetrievalCoordinator asks MemoryCoordinator "what is currently
//! valid / relevant?" to bias ranking (so stale facts sink, fresh ones rise);
//! MemoryCoordinator asks RetrievalCoordinator to surface related items when
//! reconciling a write (so a new fact can find the memories it shadows). Each
//! also stays strong in its own lane. Neither replaces the silos — they sit
//! ABOVE them as connective tissue.
//!
//! This file is the CONTRACT (traits + shared types). Implementations wrap the
//! existing storage/knowledge/procedure modules incrementally; nothing here
//! changes current behavior until an impl is wired in.

use crate::config::MindConfig;
use anyhow::Result;

/// Which silo an item came from. Lets a unified result carry provenance so the
/// caller (and the retrieval coordinator) can weight by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    /// Free-text note in the `memories` vector collection.
    Memory,
    /// (subject, predicate, object) triple in the `facts` collection.
    Fact,
    /// An (error -> fix) procedural lesson.
    Procedure,
}

/// Validity of an item as judged by the memory coordinator. Mirrors the fact
/// status model but applies uniformly across silos so retrieval can rank by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validity {
    /// Current, safe to use as a default.
    Active,
    /// Weakened but not retired (CUPMem WEAK_CHALLENGE shape).
    Weakened,
    /// Unsafe as a current default, no settled replacement yet.
    UnknownCurrent,
    /// Retired: stale / superseded / propagation-shadowed.
    Retired,
}

/// One item in the unified cross-silo view. Carries enough for the retrieval
/// coordinator to fuse and rank without re-querying each silo.
#[derive(Debug, Clone)]
pub struct UnifiedItem {
    pub id: String,
    pub kind: MemoryKind,
    pub text: String,
    pub validity: Validity,
    /// When the item became current (RFC3339), for the temporal retrieval axis.
    pub created_at: Option<String>,
    /// Relevance score from whatever axis produced it; fused later.
    pub score: f32,
    /// ids of related items across silos (the connective edges). Empty until
    /// the memory coordinator computes links.
    pub related: Vec<String>,
}

/// A single retrieval axis (dense, sparse, graph, temporal, ...). The retrieval
/// coordinator fuses several of these. Each axis is independently testable.
#[async_trait::async_trait]
pub trait RetrievalAxis: Send + Sync {
    fn name(&self) -> &'static str;
    async fn candidates(
        &self,
        config: &MindConfig,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UnifiedItem>>;
}

/// Brother #1 — memory-side coordinator. The connective tissue over all silos.
#[async_trait::async_trait]
pub trait MemoryCoordinator: Send + Sync {
    /// One unified view: everything known about `subject`/`query` across silos,
    /// each item tagged with its kind and validity. Replaces "query four silos
    /// separately" with one call.
    async fn whole_view(&self, config: &MindConfig, query: &str) -> Result<Vec<UnifiedItem>>;

    /// Current validity of an item — the signal retrieval uses to sink stale
    /// items and lift fresh ones. This is what makes vector search finally
    /// "see" fact staleness.
    async fn validity_of(&self, config: &MindConfig, id: &str) -> Result<Validity>;

    /// Cross-silo reconciliation hook for the write path: when a new item lands,
    /// propagate its consequences to related items in OTHER silos (a stale fact
    /// dims related memories, etc.). Returns ids whose validity changed.
    async fn reconcile(&self, config: &MindConfig, new_item_id: &str) -> Result<Vec<String>>;
}

/// Brother #2 — retrieval-side coordinator. The "hybrid of hybrids": fuses many
/// axes AND biases by the memory coordinator's validity signal.
#[async_trait::async_trait]
pub trait RetrievalCoordinator: Send + Sync {
    /// Fuse all configured axes, then re-weight by validity from the paired
    /// MemoryCoordinator (stale sinks, active/fresh rises). The pairing is the
    /// whole point — retrieval that knows what is still true.
    async fn search(
        &self,
        config: &MindConfig,
        query: &str,
        limit: usize,
        memory: &dyn MemoryCoordinator,
    ) -> Result<Vec<UnifiedItem>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validity_ordering_is_explicit() {
        // Active is the only "use as default" state; everything else is degraded.
        for v in [Validity::Weakened, Validity::UnknownCurrent, Validity::Retired] {
            assert_ne!(v, Validity::Active);
        }
    }

    #[test]
    fn unified_item_carries_kind_and_validity() {
        let it = UnifiedItem {
            id: "x".into(),
            kind: MemoryKind::Fact,
            text: "user located_in austin".into(),
            validity: Validity::Active,
            created_at: None,
            score: 1.0,
            related: vec![],
        };
        assert_eq!(it.kind, MemoryKind::Fact);
        assert_eq!(it.validity, Validity::Active);
    }
}
