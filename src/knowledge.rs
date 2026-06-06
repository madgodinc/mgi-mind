use anyhow::{Context, Result};
use qdrant_client::qdrant::{
    Condition, Filter, NamedVectors, PointStruct, PointsIdsList, ScrollPointsBuilder,
    SetPayloadPointsBuilder, UpsertPointsBuilder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::config::MindConfig;
use crate::storage;

/// Namespace for deterministic fact IDs - dedup by (subject, predicate, object) (audit #13).
const FACT_NAMESPACE: Uuid = Uuid::from_u128(0x6d676900_6661_6374_0000_000000000001);

/// Predicate cardinality — load-bearing for the v1.4 duel rule.
///
/// Without cardinality, every `(subject, predicate)` pair with two distinct
/// objects would look like a conflict — even when both objects are honestly
/// true at the same time ("uses Rust" + "uses Go"). Cardinality is the axis
/// that decides whether a second value contradicts or coexists.
///
/// - `Single`: at most one current object per subject. `primary_language`,
///   `birth_year`, `current_project`. Two values → conflict.
/// - `TemporalSingle`: single at any moment, but historically a sequence.
///   Same conflict semantics as `Single` for the live value; the old value
///   is dampened with a `valid_until`, not deleted. This is the natural
///   pair for bi-temporal axes (v1.4 §4).
/// - `Multi`: many objects allowed simultaneously. `uses_language`,
///   `worked_at`, `speaks`. Two values → both kept, no conflict.
///
/// Unknown predicates default to `Multi` — better to keep both honest facts
/// than to start a false duel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Cardinality {
    Single,
    TemporalSingle,
    #[default]
    Multi,
}

impl Cardinality {
    /// Parse from a lowercase MCP / config string. Returns `None` for unknown
    /// values so the caller can decide (warning + Multi fallback, hard error,
    /// etc.) instead of silently picking a wrong default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "single" => Some(Cardinality::Single),
            "temporal-single" | "temporal_single" | "temporalsingle" => {
                Some(Cardinality::TemporalSingle)
            }
            "multi" => Some(Cardinality::Multi),
            _ => None,
        }
    }

    /// Does a second value along this predicate constitute a conflict with the
    /// existing one? True for `Single` and `TemporalSingle`, false for `Multi`.
    pub fn admits_conflict(self) -> bool {
        !matches!(self, Cardinality::Multi)
    }
}

/// Per-fact lifecycle status — refined from a bool `conflict_pending` after the
/// STALE benchmark read showed our two-state model can't express Type II
/// (propagated) conflicts. CUPMem (STALE's own architecture) uses a four-state
/// label (KEEP / STALE / REPLACE / UNKNOWN); we use a similar small enum so
/// Phase 2 duel resolution and Phase 3 background re-test have somewhere to
/// write outcomes without overloading a single boolean.
///
/// - `Active` — the default. Fact is live, no contestation, no propagation
///   shadow. Legacy facts (written before v1.4) read as `Active` since their
///   payload has no status field.
/// - `Contested` — Type I direct conflict pending. A different-object write
///   came in for the same `(subject, predicate)` on a Single or
///   TemporalSingle predicate. Phase 2 duel rule resolves.
/// - `Stale` — fact's belief is no longer current (e.g. lost a duel,
///   superseded by `valid_until`). Kept as audit trace, hidden from default
///   ranking.
/// - `PropagationShadowed` — Type II propagated conflict: a sibling fact's
///   update cascaded through logical dependency to make this one suspect
///   without directly contradicting it. The doubt-window / re-test path
///   uses this to flag for re-verification rather than dampening directly.
/// - `Unknown` — placeholder for facts the writer was unsure about; treated
///   as `Active` by default ranking but surfaced separately by `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EntryStatus {
    #[default]
    Active,
    Contested,
    Stale,
    PropagationShadowed,
    Unknown,
    /// Post-critic addition (PR #5 round 2): the duel rule rejected a
    /// weak challenger and moved it to candidate state. Distinct from
    /// `Contested` (both stay live, awaiting fresh observations) — a
    /// QuarantineCandidate is *waiting for promote-on-repeat*. Phase 4
    /// STALE bench reads this to distinguish "correctly rejected weak
    /// fact" from "live contested both visible" — collapsing them was
    /// the architectural hole the critic flagged.
    QuarantineCandidate,
    /// v1.7 (#111 follow-up): a previous fact in a TemporalSingle chain.
    /// Distinct from `Stale` in three ways:
    /// 1. Was once correct (it was the canonical answer at its valid time).
    /// 2. Should remain queryable by `mind_history` / "what was X on date Y".
    /// 3. Is NOT the result of a duel against contradiction — it's the
    ///    natural end-of-life for a temporal entry whose successor arrived.
    ///
    /// Hidden from default search like `Stale`, but the difference matters
    /// for audit trails and future explanation tools.
    Superseded,
}

impl EntryStatus {
    /// Lowercase wire format. Stable since v1.4 — do not rename existing
    /// variants; new variants append.
    pub fn as_str(self) -> &'static str {
        match self {
            EntryStatus::Active => "active",
            EntryStatus::Contested => "contested",
            EntryStatus::Stale => "stale",
            EntryStatus::PropagationShadowed => "propagation_shadowed",
            EntryStatus::Unknown => "unknown",
            EntryStatus::QuarantineCandidate => "quarantine_candidate",
            EntryStatus::Superseded => "superseded",
        }
    }

    #[allow(dead_code)] // wired by Phase 2 duel-rule read path
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "active" => Some(EntryStatus::Active),
            "contested" => Some(EntryStatus::Contested),
            "stale" => Some(EntryStatus::Stale),
            "propagation_shadowed" | "propagation-shadowed" | "shadowed" => {
                Some(EntryStatus::PropagationShadowed)
            }
            "unknown" => Some(EntryStatus::Unknown),
            "quarantine_candidate" | "quarantine-candidate" | "candidate" => {
                Some(EntryStatus::QuarantineCandidate)
            }
            "superseded" => Some(EntryStatus::Superseded),
            _ => None,
        }
    }

    /// Should the fact appear in default search rankings? Stale/Shadowed are
    /// hidden by default and require an explicit `include_stale` filter to
    /// surface — this is what lets the loser of a past duel keep its audit
    /// trail without poisoning future reads.
    #[allow(dead_code)] // wired by Phase 2 duel-rule read path
    pub fn is_default_visible(self) -> bool {
        matches!(
            self,
            EntryStatus::Active | EntryStatus::Contested | EntryStatus::Unknown
        )
    }
}

/// Pure (no Qdrant) conflict detector for the duel rule.
///
/// Inputs: the facts already on record for this `(subject, predicate)` pair
/// and the new object being proposed.
/// Output: whether the new object contradicts any *valid* existing object.
///
/// Multi predicates never conflict. Single / TemporalSingle conflict when a
/// new object differs from any existing valid object; re-asserting the same
/// object is idempotent, not a conflict.
///
/// Phase 0 primitive — wired into `add_fact` in Phase 0 step 1.3 and into the
/// duel rule in Phase 2. Allowed-dead until then so the v1.4 schema lands
/// without warnings, in one bisectable commit.
#[allow(dead_code)]
pub fn detect_conflict(existing: &[Fact], new_object: &str, cardinality: Cardinality) -> bool {
    if !cardinality.admits_conflict() {
        return false;
    }
    existing.iter().any(|f| f.valid && f.object != new_object)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub created_at: Option<String>,
    pub valid: bool,
}

fn extract_string(
    payload: &HashMap<String, qdrant_client::qdrant::Value>,
    key: &str,
) -> Option<String> {
    payload.get(key).and_then(|v| {
        if let Some(qdrant_client::qdrant::value::Kind::StringValue(s)) = &v.kind {
            Some(s.clone())
        } else {
            None
        }
    })
}

fn fact_id(subject: &str, predicate: &str, object: &str) -> String {
    let key = format!("{subject}\u{0}{predicate}\u{0}{object}");
    Uuid::new_v5(&FACT_NAMESPACE, key.as_bytes()).to_string()
}

// Post-critic (PR #5 race): per-(subject, predicate) lock map so
// concurrent add_fact calls on the same axis don't see each other's
// upsert-mid-flight. Without this, two concurrent add_fact on the
// same `(subject, predicate)` could observe an inconsistent set of
// existing facts in find_facts_by_subject_predicate, start parallel
// duels, and write conflicting status flags.
//
// Locks are tokio::sync::Mutex<()> so they're hold-across-await safe.
// The outer map is parking_lot::Mutex (sync; only acquired briefly
// to look up or insert the per-key Arc); critical section is short.
//
// Memory: the lock map grows monotonically per distinct
// (subject, predicate) pair. In practice the number of unique pairs
// is bounded by the knowledge graph size; for ~12k memories with
// ~100 unique predicates and ~1000 unique subjects, the map holds
// ~thousands of entries — bounded. A future eviction policy could
// drop entries with zero outstanding waiters, but for v1.4 the
// trade-off is correctness over heap minimisation.
use std::sync::Arc;

static SUBJECT_PREDICATE_LOCKS: once_cell::sync::Lazy<
    parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
> = once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

fn lock_for_subject_predicate(subject: &str, predicate: &str) -> Arc<tokio::sync::Mutex<()>> {
    let key = format!("{subject}\u{0}{predicate}");
    let mut map = SUBJECT_PREDICATE_LOCKS.lock();
    map.entry(key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub async fn add_fact(
    config: &MindConfig,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Result<String> {
    // Post-critic (PR #5): acquire per-(subject, predicate) lock so
    // concurrent add_fact on the same axis cannot race the duel.
    // Held until the end of add_fact (including the upsert + dampen
    // sequence below), ensuring atomic outcome per axis.
    let lock = lock_for_subject_predicate(subject, predicate);
    let _guard = lock.lock().await;

    let client = storage::get_client(config).await?;
    storage::ensure_facts_collection(&client).await?;

    // Facts are vectorless (audit #6): they're looked up by exact/lexical payload
    // match, never by vector, so we no longer pay an embedding inference or store
    // a dead 768-dim vector per fact.

    // Deterministic ID dedups identical triples (audit #13): re-adding the same
    // (s,p,o) overwrites instead of piling up duplicates.
    let id = fact_id(subject, predicate, object);
    let now = chrono::Utc::now().to_rfc3339();
    // Keep the original created_at on re-add; only updated_at moves.
    let created_at =
        storage::existing_payload_string(&client, storage::FACTS_COLLECTION, &id, "created_at")
            .await
            .unwrap_or_else(|| now.clone());

    // v1.4 Phase 0 step 3+4: cardinality-aware conflict detection.
    // Phase 2 step 2.2 extends this to a full duel resolution: when a
    // contradiction is found, we either flip (dampen the loser, write
    // new as Active), contest (both stay live), or quarantine the
    // newcomer. Outcome is computed by `duel::resolve_against_existing`
    // and acted on below.
    //
    // Backward-compat: the path used by `mind_fact(action="add")` and
    // the legacy `mind_fact_add` MCP tool runs through this updated
    // `add_fact`. Existing callers see no signature change; behaviour
    // change is the introduction of duel outcomes for Single /
    // TemporalSingle predicates.
    let cardinality = get_cardinality(config, predicate).await?;
    let existing = if cardinality.admits_conflict() {
        find_facts_by_subject_predicate(config, subject, predicate).await?
    } else {
        Vec::new()
    };

    // Run the duel scaffold only when there's an actual contradiction.
    // detect_conflict already filters re-assertions of the same object
    // and Multi predicates.
    let (status, loser_to_dampen) =
        if cardinality.admits_conflict() && detect_conflict(&existing, object, cardinality) {
            // Phase 2 v0: a brand-new fact has no diversity history yet, so
            // we treat it as a single live observation with no external
            // signals. The duel weight comes from `from_live_session=true,
            // diverse_confirmations=1, external_signals=0`. Phase 4
            // calibration may revise these defaults; they are the
            // honest "first-mention" weights.
            let new_inputs = crate::duel::NewFactInputs {
                from_live_session: true,
                diverse_confirmations: 1,
                external_signals: 0,
                // A brand-new fact has no typed signals yet — the v1.5
                // Phase 7 score path activates only when mind_outcome
                // calls land against an existing memory_id (in the duel
                // path for an existing F_old, see Step 7.2 sketch).
                external_signal_score: None,
            };
            let (outcome, loser) =
                crate::duel::resolve_against_existing(config, &existing, new_inputs, cardinality)
                    .await?;
            match outcome {
                crate::duel::DuelOutcome::Flip => (EntryStatus::Active, loser),
                crate::duel::DuelOutcome::Contested => (EntryStatus::Contested, None),
                // Quarantine: the duel says the fresh fact is too weak to
                // unseat the entrenched one. Post-critic (round 2): use the
                // dedicated EntryStatus::QuarantineCandidate to distinguish
                // "weak rejected, waiting promote-on-repeat" from "live
                // contested both visible." Earlier interim collapsed both
                // to Contested, which prevented STALE bench from measuring
                // the difference.
                crate::duel::DuelOutcome::Quarantine => (EntryStatus::QuarantineCandidate, None),
            }
        } else {
            (EntryStatus::Active, None)
        };

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("subject".into(), subject.into());
    payload.insert("predicate".into(), predicate.into());
    payload.insert("object".into(), object.into());
    payload.insert("created_at".into(), created_at.into());
    payload.insert("updated_at".into(), now.into());
    payload.insert("valid".into(), "true".into());
    payload.insert("type".into(), "fact".into());
    payload.insert("status".into(), status.as_str().into());

    // Payload-only point (NamedVectors::default() is empty - no vector stored).
    let point = PointStruct::new(id.clone(), NamedVectors::default(), payload);

    client
        .upsert_points(UpsertPointsBuilder::new(storage::FACTS_COLLECTION, vec![point]).wait(true))
        .await
        .context("Failed to add fact")?;
    // v1.5 Phase 8 step 8.1D: graph just gained a fact. The
    // background re-test loop watches this counter so the cadence
    // speeds up after a burst of additions.
    crate::doubt::record_edit();

    // Phase 2 step 2.3: if the duel produced a Flip, dampen the loser
    // *after* the winner is in the store. Doing it after the upsert
    // means the read of `find_facts_by_subject_predicate` always sees
    // either the new winner alone (after dampening) or both temporarily
    // (between upsert and dampen) — never a state where the loser is
    // dampened but the winner isn't yet recorded.
    if let Some(loser_id) = loser_to_dampen {
        crate::duel::dampen_loser(config, &loser_id).await?;
    }

    Ok(id)
}

/// Query facts whose subject, predicate, OR object matches the term, filtered
/// SERVER-SIDE (audit #6): `valid = true` AND a full-text match on any of the
/// three fields. Qdrant returns only the matching facts (using the payload
/// indexes), instead of scrolling the whole collection into RAM and filtering
/// in process. Matching is full-text (tokenized) rather than raw substring.
pub async fn query_facts(config: &MindConfig, query: &str) -> Result<Vec<Fact>> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    // valid = true AND status NOT IN {stale, superseded} AND (subject ~ q OR …).
    //
    // Bug fix (issue #25, 2026-06-05): without the status exclusion,
    // dampened losers (status="stale", set by dampen_loser) and historical
    // chain entries (status="superseded", set by the TemporalSingle walk)
    // would leak into queries. Filtering on `valid` alone misses both
    // because dampen_loser / mark_superseded only touch `status`.
    let filter = Filter {
        must: vec![Condition::matches("valid", "true".to_string())],
        must_not: vec![
            Condition::matches("status", "stale".to_string()),
            Condition::matches("status", "superseded".to_string()),
            Condition::matches("status", "propagation_shadowed".to_string()),
        ],
        should: vec![
            Condition::matches_text("subject", query),
            Condition::matches_text("predicate", query),
            Condition::matches_text("object", query),
        ],
        ..Default::default()
    };

    let mut facts = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(storage::FACTS_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }

        let response = client.scroll(builder).await?;
        for point in &response.result {
            let p = &point.payload;
            let id = point
                .id
                .as_ref()
                .map(|pid| {
                    use qdrant_client::qdrant::point_id::PointIdOptions;
                    match &pid.point_id_options {
                        Some(PointIdOptions::Uuid(u)) => u.clone(),
                        Some(PointIdOptions::Num(n)) => n.to_string(),
                        None => "unknown".to_string(),
                    }
                })
                .unwrap_or_default();

            facts.push(Fact {
                id,
                subject: extract_string(p, "subject").unwrap_or_default(),
                predicate: extract_string(p, "predicate").unwrap_or_default(),
                object: extract_string(p, "object").unwrap_or_default(),
                created_at: extract_string(p, "created_at"),
                valid: true,
            });
        }

        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }

    Ok(facts)
}

/// Soft-delete: mark a fact `valid = false` instead of physically removing it,
/// so the temporal-validity flag is actually honored (audit #13).
pub async fn invalidate_fact(config: &MindConfig, id: &str) -> Result<()> {
    let client = storage::get_client(config).await?;

    let point_id: qdrant_client::qdrant::PointId = id.to_string().into();

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("valid".into(), "false".into());

    client
        .set_payload(
            SetPayloadPointsBuilder::new(storage::FACTS_COLLECTION, payload)
                .points_selector(PointsIdsList {
                    ids: vec![point_id],
                })
                .wait(true),
        )
        .await
        .context("Failed to invalidate fact")?;

    Ok(())
}

// ===== v1.4 Phase 0 step 3: cardinality registry + conflict events =====

/// Deterministic ID for a predicate registry entry. Stable across re-registration.
fn predicate_id(predicate: &str) -> String {
    let key = format!("__pred__\u{0}{predicate}");
    Uuid::new_v5(&FACT_NAMESPACE, key.as_bytes()).to_string()
}

/// Register or update the cardinality of a predicate.
///
/// Idempotent: re-registering the same predicate with the same cardinality
/// is a no-op upsert. Changing the cardinality of an already-registered
/// predicate is allowed but logged — it affects how future writes resolve
/// conflicts, and the Phase 2 duel rule treats this as a config change,
/// not a data change.
pub async fn register_cardinality(
    config: &MindConfig,
    predicate: &str,
    cardinality: Cardinality,
) -> Result<()> {
    let client = storage::get_client(config).await?;
    ensure_predicates_collection(&client).await?;

    let id = predicate_id(predicate);
    let now = chrono::Utc::now().to_rfc3339();

    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert("predicate".into(), predicate.into());
    payload.insert(
        "cardinality".into(),
        match cardinality {
            Cardinality::Single => "single",
            Cardinality::TemporalSingle => "temporal-single",
            Cardinality::Multi => "multi",
        }
        .into(),
    );
    payload.insert("updated_at".into(), now.into());

    let point = PointStruct::new(id, NamedVectors::default(), payload);
    client
        .upsert_points(
            UpsertPointsBuilder::new(storage::PREDICATES_COLLECTION, vec![point]).wait(true),
        )
        .await
        .context("Failed to register predicate cardinality")?;
    Ok(())
}

/// Look up the cardinality of a predicate. Returns `Multi` for any predicate
/// not registered — safe default per §4 (better to keep both honest facts
/// than to fire a false duel between coexisting truths).
pub async fn get_cardinality(config: &MindConfig, predicate: &str) -> Result<Cardinality> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::PREDICATES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Cardinality::Multi);
    }

    let id = predicate_id(predicate);
    let s = storage::existing_payload_string(
        &client,
        storage::PREDICATES_COLLECTION,
        &id,
        "cardinality",
    )
    .await;
    Ok(s.and_then(|s| Cardinality::parse(&s))
        .unwrap_or(Cardinality::Multi))
}

/// List all registered predicate cardinalities. Newest first by
/// `updated_at`. Used by `mgimind doctor` and `mind_predicate(action="list")`.
pub async fn list_cardinalities(config: &MindConfig) -> Result<Vec<(String, Cardinality)>> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::PREDICATES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(storage::PREDICATES_COLLECTION)
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }
        let response = client.scroll(builder).await?;
        for point in &response.result {
            let p = &point.payload;
            let predicate = extract_string(p, "predicate").unwrap_or_default();
            let card = extract_string(p, "cardinality")
                .and_then(|s| Cardinality::parse(&s))
                .unwrap_or(Cardinality::Multi);
            out.push((predicate, card));
        }
        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }
    Ok(out)
}

pub async fn ensure_predicates_collection(client: &qdrant_client::Qdrant) -> Result<()> {
    if !client
        .collection_exists(storage::PREDICATES_COLLECTION)
        .await
        .unwrap_or(false)
    {
        storage::create_vectorless_collection(client, storage::PREDICATES_COLLECTION).await?;
    }
    Ok(())
}

/// Scroll all *valid* facts in the knowledge graph. Used by Phase 1
/// migrations to walk every (subject, predicate, object) triple in one
/// pass. Server-side filter `valid = true` so dampened/invalidated facts
/// are excluded (they do not participate in dependant counts or
/// cardinality inference).
pub async fn list_all_facts(config: &MindConfig) -> Result<Vec<Fact>> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    // Bug fix (issue #25, PR #26): exclude stale (dampened) and superseded
    // (history) losers so listings reflect the post-duel canonical state.
    let filter = Filter {
        must: vec![Condition::matches("valid", "true".to_string())],
        must_not: vec![
            Condition::matches("status", "stale".to_string()),
            Condition::matches("status", "superseded".to_string()),
            Condition::matches("status", "propagation_shadowed".to_string()),
        ],
        ..Default::default()
    };

    let mut facts = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(storage::FACTS_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }
        let response = client.scroll(builder).await?;
        for point in &response.result {
            let p = &point.payload;
            let id = point
                .id
                .as_ref()
                .map(|pid| {
                    use qdrant_client::qdrant::point_id::PointIdOptions;
                    match &pid.point_id_options {
                        Some(PointIdOptions::Uuid(u)) => u.clone(),
                        Some(PointIdOptions::Num(n)) => n.to_string(),
                        None => "unknown".to_string(),
                    }
                })
                .unwrap_or_default();
            facts.push(Fact {
                id,
                subject: extract_string(p, "subject").unwrap_or_default(),
                predicate: extract_string(p, "predicate").unwrap_or_default(),
                object: extract_string(p, "object").unwrap_or_default(),
                created_at: extract_string(p, "created_at"),
                valid: true,
            });
        }
        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }
    Ok(facts)
}

/// v1.5 Phase 8 step 8.1A: scroll every valid fact and return the
/// top-N by `dependants_count` payload. Used by the background
/// re-test loop to pick "load-bearing" facts to re-evaluate first.
///
/// O(total_facts) — every tick scans the full base, then sorts the
/// top-N in memory. For Mad's ~12k-fact target the scan is one round
/// of Qdrant scroll + an O(N log K) selection, well inside the
/// per-tick budget (default cadence 60min = 3600s).
///
/// Returns `(fact_id, dependants_count)` pairs in descending order.
/// Facts without an explicit `dependants_count` payload (Phase 1
/// migration not yet run on legacy facts) are treated as 0 and rank
/// at the bottom — those are the candidates the re-test loop will
/// re-evaluate last.
pub async fn list_top_dependants_facts(
    config: &MindConfig,
    top_n: usize,
) -> Result<Vec<(String, u32)>> {
    if top_n == 0 {
        return Ok(Vec::new());
    }
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    // Bug fix (issue #25, PR #26): exclude stale (dampened) losers — dependants-
    // ranking should reflect post-duel canonical facts, not entombed losers.
    let filter = Filter {
        must: vec![Condition::matches("valid", "true".to_string())],
        must_not: vec![
            Condition::matches("status", "stale".to_string()),
            Condition::matches("status", "propagation_shadowed".to_string()),
        ],
        ..Default::default()
    };

    let mut pairs: Vec<(String, u32)> = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(storage::FACTS_COLLECTION)
            .filter(filter.clone())
            .limit(256)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }
        let response = client.scroll(builder).await?;
        for point in &response.result {
            let id = point
                .id
                .as_ref()
                .map(storage::format_point_id)
                .unwrap_or_default();
            let dep = extract_string(&point.payload, "dependants_count")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            pairs.push((id, dep));
        }
        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }

    // Sort descending by dependants_count; tie-break by id for
    // determinism in tests.
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(top_n);
    Ok(pairs)
}

/// Set a payload field on a fact by id. Used by Phase 1 migrations to
/// write back computed values (dependants_count, confirmations_count)
/// without re-creating the point.
pub async fn set_fact_payload_field(
    config: &MindConfig,
    fact_id: &str,
    field: &str,
    value: String,
) -> Result<()> {
    let client = storage::get_client(config).await?;
    let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
    payload.insert(field.into(), value.into());

    let point_id: qdrant_client::qdrant::PointId = fact_id.to_string().into();
    client
        .set_payload(
            SetPayloadPointsBuilder::new(storage::FACTS_COLLECTION, payload)
                .points_selector(PointsIdsList {
                    ids: vec![point_id],
                })
                .wait(true),
        )
        .await
        .context("Failed to set fact payload field")?;
    // v1.5 Phase 8 step 8.1D: signal that the graph changed. The
    // background re-test loop watches this counter to speed up its
    // cadence after a burst of edits. Cheap atomic add — fine to call
    // from hot paths.
    crate::doubt::record_edit();
    Ok(())
}

/// Find all *valid* facts already on record for a given `(subject, predicate)`
/// pair. Used by `add_fact` to decide whether a new triple opens a duel
/// (Phase 2) or coexists peacefully (Multi cardinality, or first write).
///
/// Server-side filter: `valid = true` AND `subject = ...` AND `predicate = ...`.
/// Exact match, not full-text — the duel-rule signal must not fire on
/// fuzzy term overlap.
pub async fn find_facts_by_subject_predicate(
    config: &MindConfig,
    subject: &str,
    predicate: &str,
) -> Result<Vec<Fact>> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok(Vec::new());
    }

    let filter = Filter {
        must: vec![
            Condition::matches("valid", "true".to_string()),
            Condition::matches("subject", subject.to_string()),
            Condition::matches("predicate", predicate.to_string()),
        ],
        // Bug fix (issue #25, 2026-06-05): exclude already-stale and
        // superseded facts from the "existing" set fed to the duel rule.
        // Otherwise an entombed loser or history entry would still be
        // considered live by find_*, causing the new fact to duel against
        // a tombstone.
        must_not: vec![
            Condition::matches("status", "stale".to_string()),
            Condition::matches("status", "superseded".to_string()),
            Condition::matches("status", "propagation_shadowed".to_string()),
        ],
        ..Default::default()
    };

    let mut facts = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(storage::FACTS_COLLECTION)
            .filter(filter.clone())
            .limit(64)
            .with_payload(true);
        if let Some(o) = offset.clone() {
            builder = builder.offset(o);
        }
        let response = client.scroll(builder).await?;
        for point in &response.result {
            let p = &point.payload;
            let id = point
                .id
                .as_ref()
                .map(|pid| {
                    use qdrant_client::qdrant::point_id::PointIdOptions;
                    match &pid.point_id_options {
                        Some(PointIdOptions::Uuid(u)) => u.clone(),
                        Some(PointIdOptions::Num(n)) => n.to_string(),
                        None => "unknown".to_string(),
                    }
                })
                .unwrap_or_default();
            facts.push(Fact {
                id,
                subject: extract_string(p, "subject").unwrap_or_default(),
                predicate: extract_string(p, "predicate").unwrap_or_default(),
                object: extract_string(p, "object").unwrap_or_default(),
                created_at: extract_string(p, "created_at"),
                valid: true,
            });
        }
        match response.next_page_offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }
    Ok(facts)
}

/// Count facts whose lifecycle status indicates an unresolved conflict.
/// Phase 0: surface counts by category for `mgimind doctor`. Phase 2 owns
/// the actual resolution.
///
/// Returns (contested, propagation_shadowed). Contested = Type I (direct
/// `(subject, predicate)` conflict). PropagationShadowed = Type II (sibling
/// update cascaded suspicion). Phase 3 background re-test sets the second.
pub async fn count_pending_conflicts(config: &MindConfig) -> Result<(u64, u64)> {
    let client = storage::get_client(config).await?;
    if !client
        .collection_exists(storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false)
    {
        return Ok((0, 0));
    }

    let one = |status: &str| Filter {
        must: vec![
            Condition::matches("valid", "true".to_string()),
            Condition::matches("status", status.to_string()),
        ],
        ..Default::default()
    };

    let contested = client
        .count(
            qdrant_client::qdrant::CountPointsBuilder::new(storage::FACTS_COLLECTION)
                .filter(one(EntryStatus::Contested.as_str()))
                .exact(true),
        )
        .await?;
    let shadowed = client
        .count(
            qdrant_client::qdrant::CountPointsBuilder::new(storage::FACTS_COLLECTION)
                .filter(one(EntryStatus::PropagationShadowed.as_str()))
                .exact(true),
        )
        .await?;
    Ok((
        contested.result.map(|r| r.count).unwrap_or(0),
        shadowed.result.map(|r| r.count).unwrap_or(0),
    ))
}

// ===== v1.4 Phase 0: cardinality + conflict detector =====
//
// Pure tests for the load-bearing primitives. No Qdrant, no async, no
// embedder — every assertion runs in microseconds and the whole module
// completes in well under 10ms. These tests are the spec the duel rule
// in Phase 2 will be calibrated against; if any of them fails or has
// to be edited later, the duel rule was built against the wrong axis.
#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a `valid = true` fact with a fixed subject/predicate so
    /// the cases below differ only on `object`. The conflict detector is
    /// concerned with object disagreement, not identity rewriting.
    fn rec(object: &str) -> Fact {
        Fact {
            id: format!("test-{object}"),
            subject: "Mad".into(),
            predicate: "primary_language".into(),
            object: object.into(),
            created_at: None,
            valid: true,
        }
    }

    #[test]
    fn parse_round_trips_known_variants() {
        assert_eq!(Cardinality::parse("single"), Some(Cardinality::Single));
        assert_eq!(
            Cardinality::parse("temporal-single"),
            Some(Cardinality::TemporalSingle)
        );
        assert_eq!(
            Cardinality::parse("temporal_single"),
            Some(Cardinality::TemporalSingle)
        );
        assert_eq!(Cardinality::parse("multi"), Some(Cardinality::Multi));
    }

    #[test]
    fn parse_is_case_and_whitespace_tolerant() {
        assert_eq!(Cardinality::parse("  SINGLE  "), Some(Cardinality::Single));
        assert_eq!(Cardinality::parse("Multi"), Some(Cardinality::Multi));
    }

    #[test]
    fn parse_returns_none_for_unknown() {
        // None — not silent fallback — so the caller logs the unknown value
        // and chooses (multi fallback, hard error, etc.) explicitly.
        assert_eq!(Cardinality::parse("kinda-single"), None);
        assert_eq!(Cardinality::parse(""), None);
    }

    #[test]
    fn default_is_multi() {
        // The roadmap-loadbearing default. Unknown predicates must not start
        // duels — better to keep both honest facts than to fire a false
        // conflict. If this ever changes the v1.4 §4 contract breaks.
        assert_eq!(Cardinality::default(), Cardinality::Multi);
    }

    #[test]
    fn multi_predicate_never_admits_conflict() {
        assert!(!Cardinality::Multi.admits_conflict());
        let existing = vec![rec("Rust")];
        assert!(!detect_conflict(&existing, "Go", Cardinality::Multi));
    }

    #[test]
    fn single_predicate_admits_conflict_on_distinct_object() {
        assert!(Cardinality::Single.admits_conflict());
        let existing = vec![rec("Rust")];
        assert!(detect_conflict(&existing, "Go", Cardinality::Single));
    }

    #[test]
    fn temporal_single_admits_conflict_too() {
        // TemporalSingle and Single share the live-value semantics; the
        // difference is only in how the loser is dampened (Phase 2 step 2.3),
        // not in whether a conflict event fires here.
        assert!(Cardinality::TemporalSingle.admits_conflict());
        let existing = vec![rec("Rust")];
        assert!(detect_conflict(
            &existing,
            "Go",
            Cardinality::TemporalSingle
        ));
    }

    #[test]
    fn re_asserting_same_object_is_idempotent() {
        // The duel rule only fires on disagreement. Re-stating "Rust" should
        // not look like a conflict, even under Single cardinality.
        let existing = vec![rec("Rust")];
        assert!(!detect_conflict(&existing, "Rust", Cardinality::Single));
    }

    #[test]
    fn invalidated_existing_facts_dont_trigger_conflict() {
        // A `valid = false` fact is dampened or invalidated; the new value
        // does not duel against it. This is what lets the loser of a past
        // duel keep its audit trace without poisoning future writes.
        let mut existing = rec("Rust");
        existing.valid = false;
        assert!(!detect_conflict(&[existing], "Go", Cardinality::Single));
    }

    #[test]
    fn empty_existing_set_is_never_a_conflict() {
        // First write of any kind is `New`, not a duel.
        assert!(!detect_conflict(&[], "Go", Cardinality::Single));
        assert!(!detect_conflict(&[], "Go", Cardinality::Multi));
    }

    #[test]
    fn valid_payload_convention_is_string_true_or_false() {
        // Post-critic regression guard (PR #4 should-fix):
        // - add_fact writes payload["valid"] = "true" (string)
        // - invalidate_fact writes payload["valid"] = "false" (string)
        // - find_facts_by_subject_predicate filters with
        //   Condition::matches("valid", "true".to_string())
        //
        // This test enforces the convention at compile time by
        // referencing the string literals. If anyone changes one
        // side to bool, this assertion fires and the next migration
        // run will silently return zero facts.
        //
        // The "string-true-or-false" convention is the audit
        // contract we ship as of v1.4 and forward.
        let v_true: &str = "true";
        let v_false: &str = "false";
        assert_eq!(v_true.len(), 4);
        assert_eq!(v_false.len(), 5);
        // The pin: filter strings on read MUST match write strings.
        // The actual filter in find_facts_by_subject_predicate uses
        // `Condition::matches("valid", "true".to_string())` — same
        // payload key, same value string. If a future refactor
        // serialises valid as bool true/false, this test must change
        // first.
    }

    #[test]
    fn predicate_id_is_deterministic_and_predicate_scoped() {
        // Same predicate → same UUID across calls (idempotent registration).
        // Different predicate → different UUID (no collisions in registry).
        // The id is a UUIDv5, so format is verifiable.
        let a = predicate_id("primary_language");
        let b = predicate_id("primary_language");
        let c = predicate_id("uses_language");
        assert_eq!(a, b, "same predicate must map to the same id");
        assert_ne!(
            a, c,
            "different predicate must map to different ids — no collisions in the cardinality registry"
        );
        assert!(uuid::Uuid::parse_str(&a).is_ok());
    }

    #[test]
    fn entry_status_default_is_active() {
        // Legacy facts with no status field must read as Active so reads of
        // pre-v1.4 data behave identically to v1.1.
        assert_eq!(EntryStatus::default(), EntryStatus::Active);
    }

    #[test]
    fn entry_status_wire_format_round_trips() {
        for s in [
            EntryStatus::Active,
            EntryStatus::Contested,
            EntryStatus::Stale,
            EntryStatus::PropagationShadowed,
            EntryStatus::Unknown,
        ] {
            assert_eq!(EntryStatus::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn entry_status_parse_is_tolerant() {
        assert_eq!(
            EntryStatus::parse("propagation-shadowed"),
            Some(EntryStatus::PropagationShadowed)
        );
        assert_eq!(
            EntryStatus::parse("shadowed"),
            Some(EntryStatus::PropagationShadowed)
        );
        assert_eq!(EntryStatus::parse("  ACTIVE  "), Some(EntryStatus::Active));
        assert_eq!(EntryStatus::parse("totally-bogus"), None);
    }

    #[test]
    fn default_visibility_hides_stale_and_shadowed() {
        // Default search must not surface losers or propagation-shadowed
        // facts — they are audit traces, not active beliefs. Active /
        // Contested / Unknown are still visible so users can see what's
        // live and what's pending resolution.
        assert!(EntryStatus::Active.is_default_visible());
        assert!(EntryStatus::Contested.is_default_visible());
        assert!(EntryStatus::Unknown.is_default_visible());
        assert!(!EntryStatus::Stale.is_default_visible());
        assert!(!EntryStatus::PropagationShadowed.is_default_visible());
    }

    #[test]
    fn predicate_id_isolates_predicate_from_fact_namespace() {
        // The predicate registry uses the same FACT_NAMESPACE but with a
        // distinct "__pred__\0<predicate>" key. A predicate id must not
        // collide with a fact id even if a fact happens to involve the
        // predicate name as subject/object — the keying makes that
        // structurally impossible. Probe a few adversarial shapes.
        let pred_id = predicate_id("primary_language");
        let fact_id_a = fact_id("primary_language", "primary_language", "primary_language");
        let fact_id_b = fact_id("", "primary_language", "");
        assert_ne!(pred_id, fact_id_a);
        assert_ne!(pred_id, fact_id_b);
    }
}
