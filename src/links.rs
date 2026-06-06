//! Cross-silo link index (link layer, step 1).
//!
//! The silos (memories / facts / procedures) are connected by explicit edges in
//! a vectorless `_links` collection. This is the materialized form of the
//! connective tissue from `universal.rs`: instead of each subsystem guessing how
//! it relates to the others, the edges are stored and queryable.
//!
//! Edges are DERIVED from data already present, so the index is always
//! rebuildable and never the source of truth:
//!   - `DerivedFrom`: fact -> the memory it was extracted from (the
//!     `source_memory_id` recorded at write time).
//!   - `Supersedes`: the active fact on an axis -> the stale/superseded/
//!     shadowed facts it replaced (materialized from the duel/propagation
//!     status the engine already set).
//!
//! Building runs as a batch (CLI / maintenance / background), never on the read
//! path (audit #5: no writes during a query). Spreading activation —
//! "a fact went stale, dim the memories it produced" — is then a cheap edge
//! walk over this index.

use crate::config::MindConfig;
use crate::storage;
use anyhow::Result;
use qdrant_client::qdrant::value::Kind;
use std::collections::HashMap;

/// Kind of edge. Stored as a string in the edge payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// fact -> source memory it was extracted from.
    DerivedFrom,
    /// active fact -> a retired fact it superseded on the same axis.
    Supersedes,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::DerivedFrom => "derived_from",
            LinkKind::Supersedes => "supersedes",
        }
    }
}

/// One directed edge.
#[derive(Debug, Clone)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub kind: LinkKind,
}

fn edge_id(from: &str, to: &str, kind: LinkKind) -> String {
    // Deterministic so rebuilds are idempotent (no duplicate edges).
    storage::deterministic_id("_links", &format!("{from}\u{0}{to}\u{0}{}", kind.as_str()))
}

/// Rebuild the link index from current facts. Idempotent. Returns edge count.
/// Batch operation — call from maintenance, not from a query.
pub async fn rebuild(config: &MindConfig) -> Result<usize> {
    use qdrant_client::qdrant::{PointStruct, UpsertPointsBuilder};

    let client = storage::get_client(config).await?;
    storage::create_vectorless_collection(&client, storage::LINKS_COLLECTION).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(0);
    }

    // Pass 1: read every fact with its axis + status + source.
    struct FactRow {
        id: String,
        axis: String, // subject\0predicate
        object: String,
        status: String,
        source: Option<String>,
    }
    let mut rows: Vec<FactRow> = Vec::new();
    for p in storage::scroll_all(&client, storage::FACTS_COLLECTION).await? {
        let get = |k: &str| -> String {
            p.payload
                .get(k)
                .and_then(|v| match &v.kind {
                    Some(Kind::StringValue(s)) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let id = p.id.as_ref().and_then(|pid| match &pid.point_id_options {
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => Some(u.clone()),
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => Some(n.to_string()),
            None => None,
        });
        let Some(id) = id else { continue };
        let subject = get("subject");
        let predicate = get("predicate");
        if subject.is_empty() || predicate.is_empty() {
            continue;
        }
        let src = get("source_memory_id");
        rows.push(FactRow {
            id,
            axis: format!("{subject}\u{0}{predicate}"),
            object: get("object"),
            status: get("status"),
            source: if src.is_empty() { None } else { Some(src) },
        });
    }

    let mut edges: Vec<Link> = Vec::new();

    // DerivedFrom: fact -> source memory.
    for r in &rows {
        if let Some(src) = &r.source {
            edges.push(Link {
                from: r.id.clone(),
                to: src.clone(),
                kind: LinkKind::DerivedFrom,
            });
        }
    }

    // Supersedes: per axis, the active fact -> each retired fact on it.
    let mut by_axis: HashMap<&str, Vec<&FactRow>> = HashMap::new();
    for r in &rows {
        by_axis.entry(r.axis.as_str()).or_default().push(r);
    }
    for group in by_axis.values() {
        let active: Vec<&&FactRow> = group
            .iter()
            .filter(|r| r.status.is_empty() || r.status == "active")
            .collect();
        let retired: Vec<&&FactRow> = group
            .iter()
            .filter(|r| !(r.status.is_empty() || r.status == "active"))
            .collect();
        for a in &active {
            for d in &retired {
                if a.object != d.object {
                    edges.push(Link {
                        from: a.id.clone(),
                        to: d.id.clone(),
                        kind: LinkKind::Supersedes,
                    });
                }
            }
        }
    }

    if edges.is_empty() {
        return Ok(0);
    }
    let points: Vec<PointStruct> = edges
        .iter()
        .map(|e| {
            let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
            payload.insert("from".into(), e.from.clone().into());
            payload.insert("to".into(), e.to.clone().into());
            payload.insert("kind".into(), e.kind.as_str().into());
            PointStruct::new(
                edge_id(&e.from, &e.to, e.kind),
                qdrant_client::qdrant::NamedVectors::default(),
                payload,
            )
        })
        .collect();
    let n = points.len();
    client
        .upsert_points(UpsertPointsBuilder::new(storage::LINKS_COLLECTION, points).wait(true))
        .await?;
    Ok(n)
}

/// Spreading activation: memory ids reachable from a retired fact via
/// DerivedFrom edges (i.e. memories that produced now-stale facts). Used to dim
/// those memories in retrieval. Read-only edge walk over the index.
pub async fn memories_of_facts(
    config: &MindConfig,
    fact_ids: &[String],
) -> Result<std::collections::HashSet<String>> {
    use qdrant_client::qdrant::{Condition, Filter, ScrollPointsBuilder};
    let mut out = std::collections::HashSet::new();
    if fact_ids.is_empty() {
        return Ok(out);
    }
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::LINKS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(out);
    }
    let want: std::collections::HashSet<&str> = fact_ids.iter().map(|s| s.as_str()).collect();
    let filter = Filter {
        must: vec![Condition::matches("kind", "derived_from".to_string())],
        ..Default::default()
    };
    let mut offset = None;
    loop {
        let mut b = ScrollPointsBuilder::new(storage::LINKS_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            b = b.offset(o);
        }
        let resp = client.scroll(b).await?;
        for p in &resp.result {
            let g = |k: &str| {
                p.payload.get(k).and_then(|v| match &v.kind {
                    Some(Kind::StringValue(s)) => Some(s.as_str()),
                    _ => None,
                })
            };
            if let (Some(from), Some(to)) = (g("from"), g("to")) {
                if want.contains(from) {
                    out.insert(to.to_string());
                }
            }
        }
        match resp.next_page_offset {
            Some(o) => offset = Some(o),
            None => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_id_is_deterministic_and_kind_scoped() {
        let a = edge_id("f1", "m1", LinkKind::DerivedFrom);
        let b = edge_id("f1", "m1", LinkKind::DerivedFrom);
        let c = edge_id("f1", "m1", LinkKind::Supersedes);
        assert_eq!(a, b); // same inputs -> same id (idempotent rebuild)
        assert_ne!(a, c); // kind is part of identity
    }

    #[test]
    fn link_kind_wire_format_stable() {
        assert_eq!(LinkKind::DerivedFrom.as_str(), "derived_from");
        assert_eq!(LinkKind::Supersedes.as_str(), "supersedes");
    }
}
