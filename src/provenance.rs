//! `mind_provenance_add` — provenance-tagged memory ingest.
//!
//! This is the strict variant of `mind_add`: same backend, same embeddings,
//! same recall — but the provenance fields are promoted from optional to
//! required and validated in Rust BEFORE [`crate::storage::add_memory`] runs.
//!
//! The contract is: "the agent just produced this snippet via a code-search /
//! doc-search MCP in the same session; make it durable, with citation". No
//! HTTP, no enrichment, no HTML stripping — that is the caller's job.
//!
//! See `docs/design/provenance-add.md` for the full design and rationale.
//! The validation rules here are §4 of that document, the embedded-content
//! format is §3, and the deterministic dedup id is §5.
//!
//! NOTE: the actual point id written to Qdrant is computed by
//! [`crate::storage::add_memory`] (UUIDv5 of `library + content`). Because our
//! formatted `content` already includes `origin_url` and `line_range`, the
//! storage-computed id is functionally equivalent to the spec id for dedup
//! purposes; storage's idempotent upsert collapses repeat saves on its own.
//! [`dedup_id`] is exposed as a stable, spec-shaped identifier for tests and
//! for any future consumer that wants to reason about provenance identity
//! without going through storage.

use std::sync::OnceLock;

use regex::Regex;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// Stable UUIDv5 namespace for provenance dedup ids. Hard-coded so the id of a
/// given `(library, snippet, origin_url, line_range)` tuple is reproducible
/// across builds and machines. Do NOT change this constant — every existing
/// provenance id would shift.
pub const NAMESPACE_PROVENANCE: Uuid = Uuid::from_u128(0x581bcc32_2e64_4c22_b72b_089b6bdfc2d7);

/// Default library when the caller does not specify one. Resolved from the
/// open question in §10 of the design doc.
pub const DEFAULT_LIBRARY: &str = "external-snippets";

/// Hosts allowed in `origin_url`. Case-insensitive, exact host match (no
/// subdomain wildcards in v1). See §4 of the design doc; widening this list is
/// a one-line PR with explicit review.
const ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "gitlab.com",
    "bitbucket.org",
    "sr.ht",
    "codeberg.org",
    "grep.app",
    "sourcegraph.com",
];

/// Owner/repo regex — same shape as e.g. `BurntSushi/ripgrep`.
fn repo_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\w.\-]+/[\w.\-]+$").expect("repo regex"))
}

/// Line-range regex — `42` or `42-58`.
fn line_range_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+(-\d+)?$").expect("line_range regex"))
}

/// All fields the MCP dispatcher hands to provenance. Lifetimes are tied to
/// the JSON arguments so we never copy big strings during validation.
#[derive(Debug, Clone)]
pub struct ProvenanceInput<'a> {
    pub library: &'a str,
    pub snippet: &'a str,
    pub origin_url: &'a str,
    pub repo: Option<&'a str>,
    pub file: Option<&'a str>,
    pub line_range: Option<&'a str>,
    pub lang: Option<&'a str>,
    pub search_tool_used: &'a str,
    pub note: Option<&'a str>,
}

/// Validation failures. Every variant carries enough context for the agent to
/// self-correct — no opaque "bad input".
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProvenanceError {
    #[error("snippet must not be empty")]
    EmptySnippet,
    #[error("snippet must not contain NUL bytes")]
    SnippetNul,
    #[error(
        "snippet looks marked up (found '<mark>' or '</mark>'); pass plain UTF-8 — \
         strip markup upstream"
    )]
    SnippetMarkup,
    #[error("origin_url could not be parsed as a URL: {0}")]
    UrlParse(String),
    #[error("origin_url must use https (got '{0}')")]
    UrlNotHttps(String),
    #[error("origin_url has no host")]
    UrlNoHost,
    #[error(
        "origin_url host '{0}' is not in the provenance allowlist; widen the list \
         in a follow-up PR if this is intentional"
    )]
    UrlHostNotAllowed(String),
    #[error("repo '{0}' must match ^[\\w.-]+/[\\w.-]+$ (e.g. owner/repo)")]
    BadRepo(String),
    #[error("file '{0}' must not start with '/'")]
    FileAbsolute(String),
    #[error("file '{0}' must not contain '..' segments")]
    FilePathTraversal(String),
    #[error("line_range '{0}' must match ^\\d+(-\\d+)?$ (e.g. '42' or '42-58')")]
    BadLineRange(String),
    #[error("provenance source unknown — use mind_add instead")]
    SearchToolMissing,
}

/// Run every validation rule from §4. Returns the first failure encountered;
/// callers surface that as the tool result text so the agent can fix it.
pub fn validate(input: &ProvenanceInput<'_>) -> Result<(), ProvenanceError> {
    // snippet ----------------------------------------------------------------
    if input.snippet.trim().is_empty() {
        return Err(ProvenanceError::EmptySnippet);
    }
    if input.snippet.contains('\0') {
        return Err(ProvenanceError::SnippetNul);
    }
    // Cheap HTML check — only catch the obvious grep.app / search-highlighter
    // tags. Full strip is explicitly out of scope (§8.5).
    if input.snippet.contains("<mark>") || input.snippet.contains("</mark>") {
        return Err(ProvenanceError::SnippetMarkup);
    }

    // search_tool_used -------------------------------------------------------
    if input.search_tool_used.trim().is_empty() {
        return Err(ProvenanceError::SearchToolMissing);
    }

    // origin_url -------------------------------------------------------------
    let parsed =
        Url::parse(input.origin_url).map_err(|e| ProvenanceError::UrlParse(e.to_string()))?;
    if parsed.scheme() != "https" {
        return Err(ProvenanceError::UrlNotHttps(parsed.scheme().to_string()));
    }
    let host = parsed
        .host_str()
        .ok_or(ProvenanceError::UrlNoHost)?
        .to_ascii_lowercase();
    if !ALLOWED_HOSTS.iter().any(|h| *h == host) {
        return Err(ProvenanceError::UrlHostNotAllowed(host));
    }

    // repo -------------------------------------------------------------------
    if let Some(repo) = input.repo
        && !repo_regex().is_match(repo)
    {
        return Err(ProvenanceError::BadRepo(repo.to_string()));
    }

    // file -------------------------------------------------------------------
    if let Some(file) = input.file {
        if file.starts_with('/') {
            return Err(ProvenanceError::FileAbsolute(file.to_string()));
        }
        // Reject `..` as a whole path segment OR as a literal substring. The
        // tool stores citations; there is no path resolution downstream, but
        // letting `..` through invites template-style abuse later.
        if file.split(['/', '\\']).any(|seg| seg == "..") || file.contains("..") {
            return Err(ProvenanceError::FilePathTraversal(file.to_string()));
        }
    }

    // line_range -------------------------------------------------------------
    if let Some(lr) = input.line_range
        && !line_range_regex().is_match(lr)
    {
        return Err(ProvenanceError::BadLineRange(lr.to_string()));
    }

    Ok(())
}

/// Build the embedded-content string per §3. No timestamp (would destroy
/// UUIDv5 stability and therefore dedup). Lines for absent optional fields
/// are omitted entirely — never present as empty.
pub fn format_content(input: &ProvenanceInput<'_>) -> String {
    let mut out = String::new();
    out.push_str("[external] ");
    out.push_str(input.origin_url);
    out.push('\n');
    if let Some(repo) = input.repo {
        out.push_str("repo: ");
        out.push_str(repo);
        out.push('\n');
    }
    if let Some(file) = input.file {
        out.push_str("file: ");
        out.push_str(file);
        out.push('\n');
    }
    if let Some(lr) = input.line_range {
        out.push_str("lines: ");
        out.push_str(lr);
        out.push('\n');
    }
    if let Some(lang) = input.lang {
        out.push_str("lang: ");
        out.push_str(lang);
        out.push('\n');
    }
    out.push_str("source: ");
    out.push_str(input.search_tool_used);
    out.push_str("\n\n");
    out.push_str(input.snippet);
    if let Some(note) = input.note {
        let trimmed = note.trim();
        if !trimmed.is_empty() {
            out.push_str("\n\nnote: ");
            out.push_str(trimmed);
        }
    }
    out
}

/// Deterministic dedup id per §5:
/// `uuid_v5(NAMESPACE_PROVENANCE, library || \0 || snippet || \0 || origin_url || \0 || line_range_or_empty)`.
///
/// Provenance is in the key on purpose: the same snippet legitimately appears
/// in many repos (Apache-2.0 headers, ubiquitous helpers); keying only on
/// `library + snippet` would collapse them all and silently drop every
/// provenance after the first.
pub fn dedup_id(library: &str, snippet: &str, origin_url: &str, line_range: Option<&str>) -> Uuid {
    let lr = line_range.unwrap_or("");
    let key = format!("{library}\u{0}{snippet}\u{0}{origin_url}\u{0}{lr}");
    Uuid::new_v5(&NAMESPACE_PROVENANCE, key.as_bytes())
}

/// Build a `source` tag for the storage payload's `source` column. The agent's
/// `search_tool_used` is the primary signal; `repo`/`file`/`line_range` give
/// the locator. Missing pieces are simply omitted.
pub fn source_tag(input: &ProvenanceInput<'_>) -> String {
    let mut s = input.search_tool_used.trim().to_string();
    if let Some(repo) = input.repo {
        s.push(':');
        s.push_str(repo);
    }
    if let Some(file) = input.file {
        s.push(':');
        s.push_str(file);
    }
    if let Some(lr) = input.line_range {
        s.push_str("#L");
        s.push_str(lr);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience builder: a valid input with only the required fields set.
    /// Tests mutate one field to exercise each rule.
    fn ok_input<'a>(snippet: &'a str, url: &'a str, tool: &'a str) -> ProvenanceInput<'a> {
        ProvenanceInput {
            library: DEFAULT_LIBRARY,
            snippet,
            origin_url: url,
            repo: None,
            file: None,
            line_range: None,
            lang: None,
            search_tool_used: tool,
            note: None,
        }
    }

    // ---- snippet ----------------------------------------------------------

    #[test]
    fn snippet_empty_rejected() {
        let i = ok_input("", "https://github.com/a/b", "ripgrep");
        assert_eq!(validate(&i), Err(ProvenanceError::EmptySnippet));
    }

    #[test]
    fn snippet_whitespace_only_rejected() {
        let i = ok_input("   \n\t ", "https://github.com/a/b", "ripgrep");
        assert_eq!(validate(&i), Err(ProvenanceError::EmptySnippet));
    }

    #[test]
    fn snippet_with_mark_tag_rejected() {
        let i = ok_input("fn foo<mark>bar</mark>()", "https://github.com/a/b", "rg");
        assert_eq!(validate(&i), Err(ProvenanceError::SnippetMarkup));
    }

    #[test]
    fn snippet_with_closing_mark_tag_rejected() {
        let i = ok_input("foo</mark>", "https://github.com/a/b", "rg");
        assert_eq!(validate(&i), Err(ProvenanceError::SnippetMarkup));
    }

    #[test]
    fn snippet_with_nul_rejected() {
        let i = ok_input("a\u{0}b", "https://github.com/a/b", "rg");
        assert_eq!(validate(&i), Err(ProvenanceError::SnippetNul));
    }

    #[test]
    fn snippet_with_angle_brackets_accepted() {
        // Generic Rust code uses `<` and `>` heavily; only literal <mark> trips.
        let i = ok_input(
            "fn pop<T: Clone>(v: &Vec<T>) -> Option<T> {}",
            "https://github.com/a/b",
            "rg",
        );
        assert!(validate(&i).is_ok(), "{:?}", validate(&i));
    }

    // ---- origin_url -------------------------------------------------------

    #[test]
    fn origin_url_https_only() {
        let i = ok_input("snip", "http://github.com/a/b", "rg");
        match validate(&i) {
            Err(ProvenanceError::UrlNotHttps(s)) => assert_eq!(s, "http"),
            other => panic!("expected UrlNotHttps, got {other:?}"),
        }
    }

    #[test]
    fn origin_url_host_allowlist_accepts_github() {
        let i = ok_input("snip", "https://github.com/rust-lang/rust", "rg");
        assert!(validate(&i).is_ok());
    }

    #[test]
    fn origin_url_host_allowlist_rejects_random() {
        let i = ok_input("snip", "https://evil.example.com/x", "rg");
        match validate(&i) {
            Err(ProvenanceError::UrlHostNotAllowed(h)) => {
                assert_eq!(h, "evil.example.com");
            }
            other => panic!("expected UrlHostNotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn origin_url_host_allowlist_is_case_insensitive() {
        let i = ok_input("snip", "https://GitHub.com/a/b", "rg");
        assert!(validate(&i).is_ok(), "{:?}", validate(&i));
    }

    #[test]
    fn origin_url_allowlist_does_not_match_subdomain() {
        // `evil.github.com` is NOT in the allowlist; only exact `github.com`.
        let i = ok_input("snip", "https://evil.github.com/x", "rg");
        assert!(matches!(
            validate(&i),
            Err(ProvenanceError::UrlHostNotAllowed(_))
        ));
    }

    #[test]
    fn origin_url_unparseable_rejected() {
        let i = ok_input("snip", "not a url", "rg");
        assert!(matches!(validate(&i), Err(ProvenanceError::UrlParse(_))));
    }

    // ---- repo --------------------------------------------------------------

    #[test]
    fn repo_regex_accepts_owner_repo() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.repo = Some("BurntSushi/ripgrep");
        assert!(validate(&i).is_ok());
    }

    #[test]
    fn repo_regex_accepts_dots_and_dashes() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.repo = Some("foo.bar/baz-quux");
        assert!(validate(&i).is_ok());
    }

    #[test]
    fn repo_regex_rejects_three_segments() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.repo = Some("a/b/c");
        assert!(matches!(validate(&i), Err(ProvenanceError::BadRepo(_))));
    }

    #[test]
    fn repo_regex_rejects_single_segment() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.repo = Some("ripgrep");
        assert!(matches!(validate(&i), Err(ProvenanceError::BadRepo(_))));
    }

    // ---- file --------------------------------------------------------------

    #[test]
    fn file_rejects_absolute() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.file = Some("/etc/passwd");
        assert!(matches!(
            validate(&i),
            Err(ProvenanceError::FileAbsolute(_))
        ));
    }

    #[test]
    fn file_rejects_path_traversal_segment() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.file = Some("src/../../etc/passwd");
        assert!(matches!(
            validate(&i),
            Err(ProvenanceError::FilePathTraversal(_))
        ));
    }

    #[test]
    fn file_rejects_path_traversal_classic() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.file = Some("../../etc/passwd");
        assert!(matches!(
            validate(&i),
            Err(ProvenanceError::FilePathTraversal(_))
        ));
    }

    #[test]
    fn file_accepts_normal_path() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.file = Some("crates/regex/src/util.rs");
        assert!(validate(&i).is_ok());
    }

    // ---- line_range -------------------------------------------------------

    #[test]
    fn line_range_accepts_single() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.line_range = Some("42");
        assert!(validate(&i).is_ok());
    }

    #[test]
    fn line_range_accepts_range() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.line_range = Some("42-58");
        assert!(validate(&i).is_ok());
    }

    #[test]
    fn line_range_rejects_trailing_dash() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.line_range = Some("42-");
        assert!(matches!(
            validate(&i),
            Err(ProvenanceError::BadLineRange(_))
        ));
    }

    #[test]
    fn line_range_rejects_letters() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.line_range = Some("abc");
        assert!(matches!(
            validate(&i),
            Err(ProvenanceError::BadLineRange(_))
        ));
    }

    #[test]
    fn line_range_rejects_commas() {
        let mut i = ok_input("snip", "https://github.com/a/b", "rg");
        i.line_range = Some("1,2");
        assert!(matches!(
            validate(&i),
            Err(ProvenanceError::BadLineRange(_))
        ));
    }

    // ---- search_tool_used -------------------------------------------------

    #[test]
    fn search_tool_used_empty_rejected_with_exact_message() {
        let i = ok_input("snip", "https://github.com/a/b", "");
        let err = validate(&i).unwrap_err();
        assert_eq!(err, ProvenanceError::SearchToolMissing);
        // The message is part of the contract — the agent reads it to decide
        // to fall back to mind_add.
        assert_eq!(
            err.to_string(),
            "provenance source unknown — use mind_add instead"
        );
    }

    #[test]
    fn search_tool_used_whitespace_only_rejected() {
        let i = ok_input("snip", "https://github.com/a/b", "   ");
        assert_eq!(validate(&i), Err(ProvenanceError::SearchToolMissing));
    }

    // ---- format_content ---------------------------------------------------

    #[test]
    fn format_omits_absent_fields() {
        let i = ok_input("body", "https://github.com/a/b", "rg");
        let s = format_content(&i);
        assert!(s.starts_with("[external] https://github.com/a/b\n"));
        assert!(!s.contains("repo:"));
        assert!(!s.contains("file:"));
        assert!(!s.contains("lines:"));
        assert!(!s.contains("lang:"));
        assert!(s.contains("source: rg\n"));
        assert!(s.ends_with("body"));
        assert!(!s.contains("note:"));
    }

    #[test]
    fn format_includes_present_optional_fields() {
        let i = ProvenanceInput {
            library: DEFAULT_LIBRARY,
            snippet: "fn x() {}",
            origin_url: "https://github.com/a/b",
            repo: Some("a/b"),
            file: Some("src/x.rs"),
            line_range: Some("10-20"),
            lang: Some("rust"),
            search_tool_used: "ripgrep",
            note: Some("worth keeping"),
        };
        let s = format_content(&i);
        assert!(s.contains("repo: a/b\n"));
        assert!(s.contains("file: src/x.rs\n"));
        assert!(s.contains("lines: 10-20\n"));
        assert!(s.contains("lang: rust\n"));
        assert!(s.contains("source: ripgrep\n"));
        assert!(s.contains("\nnote: worth keeping"));
    }

    #[test]
    fn format_has_no_timestamp_and_is_byte_stable() {
        let i = ok_input("body", "https://github.com/a/b", "rg");
        let a = format_content(&i);
        let b = format_content(&i);
        assert_eq!(a, b, "same input must produce byte-identical content");
        // No ISO-8601 wall-clock leakage.
        assert!(!a.contains("202"), "no 202x year prefix: {a}");
        assert!(!a.contains('T') || !a.contains('Z'));
        assert!(!a.contains("saved:"));
    }

    #[test]
    fn format_omits_empty_note() {
        let mut i = ok_input("body", "https://github.com/a/b", "rg");
        i.note = Some("   ");
        let s = format_content(&i);
        assert!(!s.contains("note:"));
    }

    // ---- dedup_id ---------------------------------------------------------

    #[test]
    fn dedup_id_stable_across_calls() {
        let a = dedup_id("lib", "snip", "https://github.com/a/b", Some("42"));
        let b = dedup_id("lib", "snip", "https://github.com/a/b", Some("42"));
        assert_eq!(a, b);
    }

    #[test]
    fn dedup_id_changes_with_origin_url() {
        let a = dedup_id("lib", "snip", "https://github.com/a/b", None);
        let b = dedup_id("lib", "snip", "https://gitlab.com/a/b", None);
        assert_ne!(a, b);
    }

    #[test]
    fn dedup_id_changes_with_line_range() {
        let a = dedup_id("lib", "snip", "https://github.com/a/b", Some("10"));
        let b = dedup_id("lib", "snip", "https://github.com/a/b", Some("11"));
        assert_ne!(a, b);
    }

    #[test]
    fn dedup_id_changes_with_library() {
        let a = dedup_id("lib1", "snip", "https://github.com/a/b", None);
        let b = dedup_id("lib2", "snip", "https://github.com/a/b", None);
        assert_ne!(a, b);
    }

    #[test]
    fn dedup_id_changes_with_snippet() {
        let a = dedup_id("lib", "snip-a", "https://github.com/a/b", None);
        let b = dedup_id("lib", "snip-b", "https://github.com/a/b", None);
        assert_ne!(a, b);
    }

    #[test]
    fn dedup_id_treats_missing_and_empty_line_range_equal() {
        let a = dedup_id("lib", "snip", "https://github.com/a/b", None);
        let b = dedup_id("lib", "snip", "https://github.com/a/b", Some(""));
        assert_eq!(a, b);
    }

    #[test]
    fn dedup_id_is_v5_not_v4() {
        let id = dedup_id("lib", "snip", "https://github.com/a/b", Some("1"));
        assert_eq!(id.get_version_num(), 5);
    }

    // ---- source_tag -------------------------------------------------------

    #[test]
    fn source_tag_assembles_from_all_pieces() {
        let i = ProvenanceInput {
            library: DEFAULT_LIBRARY,
            snippet: "x",
            origin_url: "https://github.com/a/b",
            repo: Some("a/b"),
            file: Some("src/x.rs"),
            line_range: Some("10-20"),
            lang: None,
            search_tool_used: "ripgrep",
            note: None,
        };
        assert_eq!(source_tag(&i), "ripgrep:a/b:src/x.rs#L10-20");
    }

    #[test]
    fn source_tag_just_the_tool_when_others_absent() {
        let i = ok_input("x", "https://github.com/a/b", "sourcegraph");
        assert_eq!(source_tag(&i), "sourcegraph");
    }
}
