//! `mgimind relibrary` (v2.7) — move memories from one library to another.
//!
//! Point ids in the single-collection layout are content-addressed:
//! `uuid5(library + '\0' + content)` (see `storage::deterministic_id`). That
//! makes a plain "rename this library" unsupported — the library name is part
//! of every id, so moving a point always means giving it a NEW id. There is no
//! `set_payload(library=...)` shortcut: the id would then disagree with its own
//! content, breaking dedup and any future id recompute (e.g. a re-add of the
//! same content under the old library would collide with nothing, instead of
//! upserting in place).
//!
//! So a move is: compute the id the destination library would assign to this
//! content, copy the point there with the SAME vectors (no re-embedding) and
//! the same payload (destination library substituted in), delete the point at
//! the old id, and patch up anything elsewhere in the store that named the old
//! id. Dry-run by default — `apply` is required to write anything.
//!
//! What references an id, and what this module does about each:
//!   - `external_signals_v15` (`cited_by` entries, see `outcome.rs`): a point
//!     ANYWHERE in the store can carry a signal whose `source` is another
//!     point's id. Rewritten here (`rewrite_cited_by`) across the whole
//!     collection, not just the moved library — the citing point can live
//!     anywhere.
//!   - `access_journal.json` (`access.rs`): id -> usage stats. Rekeyed via
//!     `access::rekey_ids`.
//!   - audit log (`audit.rs`): append-only and hash-chained, so a past line
//!     cannot be rewritten. A new `AuditOp::Relibrary` event is written for the
//!     new id instead, noting the old `library:id` pair. The old id's earlier
//!     history (Add/Update/...) stays filed under the old id — `audit show`
//!     on the new id starts from the move forward. Documented limitation, not
//!     a bug: rewriting the chain would defeat its tamper-evidence.
//!   - `_mod_procstats` (procedure success/fail counters): keyed by the
//!     procedure's own id, which never depends on `library` (`PROC_NAMESPACE`
//!     hashes only `error + fix`). Procedures are filtered out of every match
//!     set server-side (`storage::scroll_library_full`), so this collection is
//!     never touched by a move.
//!   - `provenance.rs` (`mind_provenance_add`): no side table. The note at the
//!     top of that file already records that the REAL point id is the
//!     storage-computed one, not `provenance::dedup_id` — there is nothing
//!     keyed by id to rewrite.
//!   - `kv_store.json` (the generic agent key-value store behind `/kv/*`):
//!     opaque JSON values by design (the brain never parses them), so a value
//!     that embeds a memory id as a sub-field cannot be safely rewritten here.
//!     Best-effort: `scan_kv_store_for_ids` greps the raw file for a moved id
//!     and reports it so an operator can follow up by hand.
//!
//! Idempotency: a crash between the new upsert and the old delete must not
//! leave a duplicate on resume, and a true independent duplicate (unrelated
//! content that happens to already live at the destination id) must not be
//! silently clobbered or have its source deleted out from under it. Both are
//! handled by stamping every moved point with `relibrary_source_id` (the old
//! id it came from): on a collision, that marker is the one signal that tells
//! the two cases apart (see `run`).

use std::collections::HashMap;

use anyhow::{Context, Result};
use regex::Regex;

use crate::config::MindConfig;
use crate::outcome::{ExternalSignal, OutcomeSignal};

/// Payload field stamped on every point this module ever writes: the id it was
/// moved FROM. Doubles as the resume marker (see module docs) and as a
/// permanent provenance breadcrumb — "this memory used to live somewhere else"
/// is worth keeping, not worth scrubbing back out once the move is verified.
const SOURCE_ID_FIELD: &str = "relibrary_source_id";
/// Payload field stamped alongside `SOURCE_ID_FIELD`: the library it came from.
const SOURCE_LIBRARY_FIELD: &str = "relibrary_from_library";

/// How many sample matches a dry run shows.
const SAMPLE_SIZE: usize = 10;

/// What selects which points move. At least one of `source_match`,
/// `content_match`, `ids` must be set (the CLI enforces this); when more than
/// one is set, a point must satisfy ALL of them (AND, not OR) — this is
/// deliberately narrowing, not widening.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub from: String,
    pub to: String,
    pub source_match: Option<Regex>,
    pub content_match: Option<Regex>,
    pub ids: Option<Vec<String>>,
    /// Actually write. Without it, `run` only reports what would move.
    pub apply: bool,
}

impl Options {
    /// True when no selector was given at all — the CLI must refuse this
    /// before calling `run` (matching nothing and matching everything look
    /// identical from in here, so the refusal has to happen at the boundary
    /// with the user, where "nothing selected" is still distinguishable from
    /// "an empty regex that happens to match everything").
    pub fn has_selector(&self) -> bool {
        self.ids.is_some() || self.source_match.is_some() || self.content_match.is_some()
    }
}

/// A matched point, shown in the dry-run sample.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SampleEntry {
    pub id: String,
    pub source: Option<String>,
    pub content_preview: String,
}

/// A collision: a point already lives at the id the destination library would
/// assign to this content, and it was NOT produced by this exact move (see
/// `run`). Left untouched on both sides; reported so a human can decide.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Collision {
    pub old_id: String,
    pub new_id: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Report {
    /// Points in `from` matching the selector, before any write.
    pub matched: usize,
    /// First `SAMPLE_SIZE` matches, shown on every run (dry or applied).
    pub sample: Vec<SampleEntry>,
    /// Points actually moved (upserted at the new id + old id deleted).
    pub moved: usize,
    /// Matches left untouched because an unrelated point already holds the
    /// destination id.
    pub skipped_collision: usize,
    pub collisions: Vec<Collision>,
    /// Points elsewhere in the store whose `external_signals_v15` cited_by
    /// entries were repointed at a new id.
    pub signals_rewritten: usize,
    /// Access-journal entries renamed to follow their point.
    pub access_rekeyed: usize,
    /// Moved ids that still show up verbatim in `kv_store.json` — opaque to
    /// the brain, so these are reported, not rewritten. Empty when the file
    /// is absent or no moved id appears in it.
    pub kv_warnings: Vec<String>,
    /// Whether `to` had to be created in `libraries.json`.
    pub created_library: bool,
}

/// Reject configurations that can never do anything sane, before touching
/// Qdrant. Kept separate from the CLI's own argument parsing so the library
/// function is safe to call directly (tests, a future MCP wrapper) without
/// re-deriving these rules.
pub fn validate(opts: &Options) -> Result<()> {
    if opts.from.trim().is_empty() || opts.to.trim().is_empty() {
        anyhow::bail!("relibrary: --from and --to must both be non-empty library names");
    }
    if opts.from == opts.to {
        anyhow::bail!(
            "relibrary: --from and --to are the same library ('{}') — nothing to move",
            opts.from
        );
    }
    if !opts.has_selector() {
        anyhow::bail!(
            "relibrary: give at least one of --source-match, --content-match, --ids-file"
        );
    }
    const RESERVED: &[&str] = &["_procedures"];
    if RESERVED.contains(&opts.to.as_str()) || RESERVED.contains(&opts.from.as_str()) {
        anyhow::bail!(
            "relibrary: '{}' is a reserved system library, not a user library",
            if RESERVED.contains(&opts.from.as_str()) {
                &opts.from
            } else {
                &opts.to
            }
        );
    }
    Ok(())
}

/// Pure selector check, split out from `run` so it is unit-testable without a
/// Qdrant. A point qualifies when it satisfies every selector that was set.
fn matches(id: &str, source: Option<&str>, content: &str, opts: &Options) -> bool {
    if let Some(ids) = &opts.ids
        && !ids.iter().any(|x| x == id)
    {
        return false;
    }
    if let Some(re) = &opts.source_match {
        match source {
            Some(s) if re.is_match(s) => {}
            _ => return false,
        }
    }
    if let Some(re) = &opts.content_match
        && !re.is_match(content)
    {
        return false;
    }
    true
}

/// Shorten a stored memory's content to a one-line dry-run preview. Pure, so
/// it is tested without a store.
fn preview(content: &str, max_chars: usize) -> String {
    let one_line: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = one_line.chars().take(max_chars).collect();
    if truncated.chars().count() < one_line.chars().count() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

/// Repoint any `cited_by` signal (see `outcome::ExternalSignal`) whose
/// `source` names an id that just moved. Pure transform over one point's
/// signal log; returns `None` when nothing in it changed, so the caller only
/// writes points that actually need it.
fn rewrite_signals(
    signals: &[ExternalSignal],
    id_mapping: &HashMap<String, String>,
) -> Option<Vec<ExternalSignal>> {
    let mut out = signals.to_vec();
    let mut changed = false;
    for sig in &mut out {
        if sig.signal_type == OutcomeSignal::CitedBy
            && let Some(new_id) = id_mapping.get(&sig.source)
        {
            sig.source = new_id.clone();
            changed = true;
        }
    }
    changed.then_some(out)
}

/// Scan the WHOLE memories collection (every library, not just `to`/`from` —
/// a citing point can live anywhere) for `external_signals_v15` entries that
/// cite a moved id, and repoint them. Returns how many points were rewritten.
async fn rewrite_cited_by(
    config: &MindConfig,
    client: &qdrant_client::Qdrant,
    id_mapping: &HashMap<String, String>,
) -> Result<usize> {
    let all = crate::storage::scroll_all(client, crate::storage::MEMORIES_COLLECTION).await?;
    let mut rewritten = 0usize;
    for p in all {
        let Some(id) = p.id.as_ref().map(crate::storage::format_point_id) else {
            continue;
        };
        let Some(raw) = crate::storage::extract_string_pub(&p.payload, "external_signals_v15")
        else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        let Ok(signals) = serde_json::from_str::<Vec<ExternalSignal>>(&raw) else {
            // Unparseable legacy slot — not this pass's job to fix.
            continue;
        };
        if let Some(updated) = rewrite_signals(&signals, id_mapping) {
            crate::storage::write_external_signals(config, &id, &updated).await?;
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

/// Best-effort scan of the opaque agent KV store for a moved id. See the
/// module docs for why this is a warning, not a rewrite.
fn scan_kv_store_for_ids<'a>(ids: impl Iterator<Item = &'a String>) -> Vec<String> {
    let path = crate::config::mind_home().join("kv_store.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    ids.filter(|id| raw.contains(id.as_str()))
        .cloned()
        .collect()
}

/// Run a relibrary pass. Dry-run (`opts.apply == false`) only matches and
/// samples — nothing is written, `to` is not even created. `apply` does the
/// move; see the module docs for the full reference-rewrite story.
pub async fn run(config: &MindConfig, opts: Options) -> Result<Report> {
    validate(&opts)?;

    let client = crate::storage::get_client(config).await?;
    crate::storage::ensure_memories_collection(&client, config.vector_size).await?;

    let candidates = crate::storage::scroll_library_full(&client, &opts.from).await?;
    let mut matched = Vec::with_capacity(candidates.len());
    for p in candidates {
        let Some(id) = p.id.as_ref().map(crate::storage::format_point_id) else {
            continue;
        };
        let Some(content) = crate::storage::extract_string_pub(&p.payload, "content") else {
            continue;
        };
        let source = crate::storage::extract_string_pub(&p.payload, "source");
        if matches(&id, source.as_deref(), &content, &opts) {
            matched.push(p);
        }
    }

    let mut report = Report {
        matched: matched.len(),
        sample: matched
            .iter()
            .take(SAMPLE_SIZE)
            .map(|p| {
                let id =
                    p.id.as_ref()
                        .map(crate::storage::format_point_id)
                        .unwrap_or_default();
                let content =
                    crate::storage::extract_string_pub(&p.payload, "content").unwrap_or_default();
                SampleEntry {
                    id,
                    source: crate::storage::extract_string_pub(&p.payload, "source"),
                    content_preview: preview(&content, 160),
                }
            })
            .collect(),
        ..Default::default()
    };

    if !opts.apply {
        return Ok(report);
    }

    if !crate::storage::is_registered(&opts.to) {
        crate::storage::register_library(&opts.to)
            .with_context(|| format!("creating destination library '{}'", opts.to))?;
        crate::audit::record(
            crate::audit::AuditEvent::new(
                crate::audit::AuditOp::LibraryCreate,
                opts.to.clone(),
                opts.to.clone(),
            )
            .actor("relibrary"),
        );
        report.created_library = true;
    }

    let mut id_mapping: HashMap<String, String> = HashMap::new();

    for p in &matched {
        let Some(old_id) = p.id.as_ref().map(crate::storage::format_point_id) else {
            continue;
        };
        let Some(content) = crate::storage::extract_string_pub(&p.payload, "content") else {
            continue;
        };
        let new_id = crate::storage::quarantine_id_for(&opts.to, &content);

        let existing = crate::storage::get_full_points(
            &client,
            crate::storage::MEMORIES_COLLECTION,
            std::slice::from_ref(&new_id),
        )
        .await;

        if let Some(target) = existing.get(&new_id) {
            // Someone (or a previous, interrupted run of THIS move) is already
            // sitting at the destination id. Tell the two cases apart by the
            // marker this module always stamps: if it says it came from the
            // exact point we are processing, the upsert half of this move
            // already landed — finish it by deleting the stale source, and do
            // not report it as a conflict. Anything else is a real,
            // independent duplicate: leave both points alone and report it.
            let marker = crate::storage::extract_string_pub(&target.payload, SOURCE_ID_FIELD);
            if marker.as_deref() == Some(old_id.as_str()) {
                crate::storage::delete_memories(config, std::slice::from_ref(&old_id)).await?;
                id_mapping.insert(old_id.clone(), new_id.clone());
                report.moved += 1;
            } else {
                report.skipped_collision += 1;
                report.collisions.push(Collision {
                    old_id: old_id.clone(),
                    new_id: new_id.clone(),
                });
            }
            continue;
        }

        let Some(vectors) = crate::storage::named_vectors_of(p) else {
            anyhow::bail!(
                "relibrary: point {old_id} in '{}' carries no vectors — refusing to drop it \
                 silently; investigate before re-running",
                opts.from
            );
        };
        let mut payload = p.payload.clone();
        payload.insert("library".into(), opts.to.clone().into());
        payload.insert(SOURCE_ID_FIELD.into(), old_id.clone().into());
        payload.insert(SOURCE_LIBRARY_FIELD.into(), opts.from.clone().into());

        crate::storage::upsert_full_point(&client, &new_id, vectors, payload).await?;
        crate::storage::delete_memories(config, std::slice::from_ref(&old_id)).await?;

        crate::audit::record(
            crate::audit::AuditEvent::new(
                crate::audit::AuditOp::Relibrary,
                opts.to.clone(),
                new_id.clone(),
            )
            .actor("relibrary")
            .note(format!("moved from {}:{old_id}", opts.from)),
        );

        id_mapping.insert(old_id.clone(), new_id.clone());
        report.moved += 1;
    }

    if !id_mapping.is_empty() {
        report.signals_rewritten = rewrite_cited_by(config, &client, &id_mapping).await?;
        report.access_rekeyed = crate::access::rekey_ids(&id_mapping);
        report.kv_warnings = scan_kv_store_for_ids(id_mapping.keys());
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::RegexBuilder;

    fn ci(pattern: &str) -> Regex {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .unwrap()
    }

    fn base_opts() -> Options {
        Options {
            from: "projects".into(),
            to: "creepidota".into(),
            ..Default::default()
        }
    }

    // ---- validate -----------------------------------------------------

    #[test]
    fn validate_rejects_same_from_and_to() {
        let mut o = base_opts();
        o.to = o.from.clone();
        o.content_match = Some(ci("x"));
        assert!(validate(&o).is_err());
    }

    #[test]
    fn validate_rejects_no_selector() {
        let o = base_opts();
        assert!(validate(&o).is_err());
    }

    #[test]
    fn validate_rejects_reserved_library() {
        let mut o = base_opts();
        o.to = "_procedures".into();
        o.content_match = Some(ci("x"));
        assert!(validate(&o).is_err());
    }

    #[test]
    fn validate_accepts_a_single_selector() {
        let mut o = base_opts();
        o.source_match = Some(ci("creepidota"));
        assert!(validate(&o).is_ok());
    }

    // ---- matches (selector semantics) ----------------------------------

    #[test]
    fn matches_by_content_regex_case_insensitively() {
        let mut o = base_opts();
        o.content_match = Some(ci("fraylon|pandion"));
        assert!(matches("id1", None, "the old name was Pandion", &o));
        assert!(matches("id1", None, "FRAYLON history", &o));
        assert!(!matches("id1", None, "unrelated note", &o));
    }

    #[test]
    fn matches_by_source_regex() {
        let mut o = base_opts();
        o.source_match = Some(ci("^creepidota-"));
        assert!(matches("id1", Some("creepidota-desktop-recon"), "x", &o));
        assert!(!matches("id1", Some("other-source"), "x", &o));
        // No source tag on the point at all -> never matches a source filter.
        assert!(!matches("id1", None, "x", &o));
    }

    #[test]
    fn matches_by_explicit_ids() {
        let mut o = base_opts();
        o.ids = Some(vec!["a".into(), "b".into()]);
        assert!(matches("a", None, "x", &o));
        assert!(!matches("c", None, "x", &o));
    }

    #[test]
    fn combined_selectors_are_and_not_or() {
        let mut o = base_opts();
        o.source_match = Some(ci("^creepidota-"));
        o.content_match = Some(ci("fraylon"));
        // Matches source but not content.
        assert!(!matches("id1", Some("creepidota-x"), "unrelated", &o));
        // Matches content but not source.
        assert!(!matches("id1", Some("other"), "fraylon lore", &o));
        // Matches both.
        assert!(matches("id1", Some("creepidota-x"), "fraylon lore", &o));
    }

    // ---- preview --------------------------------------------------------

    #[test]
    fn preview_collapses_whitespace_and_truncates() {
        assert_eq!(preview("hello\n  world", 100), "hello world");
        let long = "a".repeat(200);
        let p = preview(&long, 160);
        assert!(p.ends_with("..."));
        assert_eq!(p.chars().count(), 163); // 160 + "..."
    }

    // ---- id recompute: relibrary must use the exact production formula -

    #[test]
    fn new_id_is_the_same_formula_add_memory_would_use() {
        // `quarantine_id_for` IS `deterministic_id` (see storage.rs) — this
        // locks relibrary to that one formula so the two can never silently
        // drift apart. Same content, different library, must disagree.
        let content = "CreepiDota was called Fraylon before Pandion.";
        let a = crate::storage::quarantine_id_for("projects", content);
        let b = crate::storage::quarantine_id_for("creepidota", content);
        assert_ne!(a, b, "a library-addressed id must change with the library");
        // Deterministic: computing it again must land on the same id.
        let b2 = crate::storage::quarantine_id_for("creepidota", content);
        assert_eq!(b, b2);
    }

    #[test]
    fn new_id_is_trim_stable_like_the_write_path() {
        // add_memory stores the TRIMMED chunk and ids off that trimmed text
        // (storage.rs: "Trimming the STORED chunk ... keeps the
        // content-addressed id trim-stable"). relibrary reads back the
        // ALREADY-TRIMMED stored content, so padding here must not change
        // anything — this is a regression guard on that assumption.
        let a = crate::storage::quarantine_id_for("lib", "hello world");
        let b = crate::storage::quarantine_id_for("lib", "  hello world  ");
        assert_eq!(a, b);
    }

    // ---- reference rewrite: cited_by signals ----------------------------

    fn sig(ty: OutcomeSignal, source: &str) -> ExternalSignal {
        ExternalSignal {
            signal_type: ty,
            success: true,
            source: source.into(),
            ts: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn rewrite_signals_repoints_only_cited_by_to_moved_ids() {
        let mut mapping = HashMap::new();
        mapping.insert("old-a".to_string(), "new-a".to_string());

        let signals = vec![
            sig(OutcomeSignal::CitedBy, "old-a"),
            sig(OutcomeSignal::TestPassed, "old-a"), // not cited_by: untouched
            sig(OutcomeSignal::CitedBy, "unrelated-id"),
        ];
        let out = rewrite_signals(&signals, &mapping).expect("something changed");
        assert_eq!(out[0].source, "new-a");
        assert_eq!(out[1].source, "old-a", "only cited_by signals carry an id");
        assert_eq!(out[2].source, "unrelated-id");
    }

    #[test]
    fn rewrite_signals_returns_none_when_nothing_matches() {
        let mapping = HashMap::new();
        let signals = vec![sig(OutcomeSignal::CitedBy, "some-id")];
        assert!(rewrite_signals(&signals, &mapping).is_none());
    }

    #[test]
    fn rewrite_signals_is_idempotent_on_a_second_pass() {
        // After one rewrite, running the same mapping again must be a no-op —
        // a resumed/re-run relibrary pass must not keep "changing" things.
        let mut mapping = HashMap::new();
        mapping.insert("old-a".to_string(), "new-a".to_string());
        let signals = vec![sig(OutcomeSignal::CitedBy, "old-a")];
        let once = rewrite_signals(&signals, &mapping).unwrap();
        assert!(rewrite_signals(&once, &mapping).is_none());
    }

    // ---- kv scan ----------------------------------------------------------

    #[test]
    fn kv_scan_reports_literal_id_occurrences() {
        // MGIMIND_HOME_TEST_LOCK serializes every test in the crate that
        // overrides this process-global env var — see its doc comment in
        // config.rs. Required here since `cargo test` runs threads in parallel.
        let _guard = crate::config::MGIMIND_HOME_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("MGIMIND_HOME", dir.path()) };
        std::fs::write(
            dir.path().join("kv_store.json"),
            r#"{"some-agent-key": {"linked_memory": "old-id-123"}}"#,
        )
        .unwrap();

        let ids = ["old-id-123".to_string(), "unrelated-id".to_string()];
        let found = scan_kv_store_for_ids(ids.iter());
        assert_eq!(found, vec!["old-id-123".to_string()]);

        unsafe { std::env::remove_var("MGIMIND_HOME") };
    }

    #[test]
    fn kv_scan_is_empty_when_file_is_absent() {
        let _guard = crate::config::MGIMIND_HOME_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("MGIMIND_HOME", dir.path()) };
        let ids = ["whatever".to_string()];
        assert!(scan_kv_store_for_ids(ids.iter()).is_empty());
        unsafe { std::env::remove_var("MGIMIND_HOME") };
    }
}
