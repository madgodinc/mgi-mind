//! v1.4 Phase 5: local LLM auto-extractor for knowledge-graph facts.
//!
//! Why this exists — the gap the Phase 1 migration surfaced. The
//! published memory products (mem0, Zep, supermemory) run an LLM
//! extractor on every ingested message via cloud API. Our local-first
//! contract forbids paid API in the default path. Without an
//! extractor of our own, the knowledge graph stays empty: users write
//! to `mind_add` (memories) but rarely call `mind_fact_add` by hand,
//! so the Phase 2 duel rule has nothing to operate on.
//!
//! The auto-extractor closes that gap with a **local LLM that ships
//! out of the box**. After a successful `mind_add`, a background pass
//! runs the new memory through the extractor; resulting subject-
//! predicate-object triples are written to the knowledge graph via
//! `mind_fact_add`. The user sees memories appear; facts accumulate
//! silently underneath.
//!
//! ## Model variants (this commit)
//!
//! - **Lite** — Qwen 2.5 1.5B Instruct Q4_K_M, ~990 MB on disk,
//!   ~1.5 GB RAM, ~7 sec per extraction on a current x86 CPU.
//!   Weaker multilingual; default choice for small machines.
//! - **Default** — Qwen 2.5 3B Instruct Q4_K_M, ~1.93 GB on disk,
//!   ~2.5 GB RAM, ~12 sec per extraction on the same CPU. Native
//!   RU+EN+ZH support; recommended for the author's mixed-language
//!   base.
//!
//! Same Qwen 2.5 family for both: identical chat template, identical
//! tokenizer behaviour, identical output structure. Switching between
//! the two is a config flag, not a code path.
//!
//! ## Why CPU-only default (mirrors the embedder decision)
//!
//! - Distribution: one binary, no CUDA toolkit at build time, no
//!   NVIDIA driver requirement at runtime. Works on every Mac, every
//!   Linux, every Windows.
//! - VRAM stays free for embedder/reranker/games/anything else the
//!   user is running.
//! - CPU inference of 1.5B/3B Q4 on modern hardware is "background-
//!   task fast" — slow for an interactive prompt but fine for an
//!   async post-ingest pass.
//! - Optional CUDA backend lands as a feature flag in a follow-up
//!   commit (the same pattern the embedder uses).
//!
//! ## TODO(phase-5-step-2..5)
//!
//! This commit lands the variant enum, the pinned-hash slots, and
//! the CLI surface contract. The actual GGUF download, model load,
//! prompt template, JSON parse, and `mind_ingest` auto-integration
//! land as separate bisectable commits on this same branch:
//!
//! - Step 5.2: `llama-cpp-2` cargo dependency + GGUF download via
//!   `util::download_file` (same pattern as the embedder ONNX and
//!   the bundled Qdrant binary).
//! - Step 5.3: lazy model load in a `OnceCell<Mutex<LlamaModel>>`,
//!   ChatML prompt template, JSON parser for the triples.
//! - Step 5.4: `mind_extractor` consolidated MCP tool +
//!   `mgimind extractor install/info/uninstall` CLI.
//! - Step 5.5: hook from `mind_ingest` write-path to enqueue a
//!   background extraction task per accepted memory.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

// ===== Variant selection =====

/// Which Qwen 2.5 weight set to use. Both are Q4_K_M for the same
/// inference path and the same prompt template; the difference is
/// parameter count, which trades disk/RAM/quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtractorVariant {
    /// Qwen 2.5 1.5B Instruct Q4_K_M. ~990 MB on disk, ~1.5 GB RAM.
    /// Faster, weaker multilingual. Suitable for laptops with limited
    /// RAM or for EN-only content.
    Lite,
    /// Qwen 2.5 3B Instruct Q4_K_M. ~1.93 GB on disk, ~2.5 GB RAM.
    /// Default. Recommended for mixed RU+EN+ZH content, including
    /// the author's own base.
    Default,
}

impl Default for ExtractorVariant {
    fn default() -> Self {
        ExtractorVariant::Default
    }
}

impl ExtractorVariant {
    /// Parse the variant from a CLI / config string. Case- and
    /// whitespace-tolerant. Returns `None` on unknown values so the
    /// caller can prompt or fall back explicitly instead of silently
    /// picking a default that may not be what the user expected.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lite" | "1.5b" | "small" | "qwen-1.5b" => Some(ExtractorVariant::Lite),
            "default" | "3b" | "qwen-3b" | "" => Some(ExtractorVariant::Default),
            _ => None,
        }
    }

    /// Lowercase wire format. Stable from v1.4 — do not rename existing
    /// variants; new ones append.
    pub fn as_str(self) -> &'static str {
        match self {
            ExtractorVariant::Lite => "lite",
            ExtractorVariant::Default => "default",
        }
    }

    /// Filename the GGUF model is stored under inside
    /// `$MGIMIND_HOME/models/extractor/`. Different per variant so a
    /// user can swap without re-downloading the variant they already
    /// have.
    pub fn gguf_filename(self) -> &'static str {
        match self {
            ExtractorVariant::Lite => "qwen2.5-1.5b-instruct-q4_k_m.gguf",
            ExtractorVariant::Default => "qwen2.5-3b-instruct-q4_k_m.gguf",
        }
    }

    /// HuggingFace download URL for the GGUF artifact. Both come from
    /// the same Qwen team upload — `Qwen/Qwen2.5-*-Instruct-GGUF` —
    /// so the user is reading from the model authors' own release
    /// channel, not a third-party rehosting.
    pub fn hf_url(self) -> &'static str {
        match self {
            ExtractorVariant::Lite => {
                "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf"
            }
            ExtractorVariant::Default => {
                "https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf"
            }
        }
    }

    /// On-disk size in megabytes (rounded). Surfaced to the user
    /// during install so they confirm the download size before the
    /// long wait.
    pub fn approx_size_mb(self) -> u32 {
        match self {
            ExtractorVariant::Lite => 990,
            ExtractorVariant::Default => 1930,
        }
    }

    /// Approximate RAM used by the model + KV cache at default
    /// context length when loaded. Surfaced before install so the user
    /// understands the operational cost, not just the disk cost.
    pub fn approx_ram_mb(self) -> u32 {
        match self {
            ExtractorVariant::Lite => 1500,
            ExtractorVariant::Default => 2500,
        }
    }

    /// Pinned SHA-256 for the GGUF file. Used by `util::download_file`
    /// fail-closed verification (audit #6 pattern). Both pins live in
    /// `integrity.rs`; this method just routes the variant to its pin.
    pub fn pinned_hash(self) -> Option<&'static str> {
        match self {
            ExtractorVariant::Lite => {
                crate::integrity::pin(crate::integrity::EXTRACTOR_QWEN_1_5B_Q4_K_M)
            }
            ExtractorVariant::Default => {
                crate::integrity::pin(crate::integrity::EXTRACTOR_QWEN_3B_Q4_K_M)
            }
        }
    }

    /// One-line operator-facing summary printed by `mgimind extractor
    /// install` before the download starts and by `info` after.
    pub fn describe(self) -> String {
        format!(
            "Qwen 2.5 {} Instruct Q4_K_M — {} MB on disk, ~{} MB RAM loaded",
            match self {
                ExtractorVariant::Lite => "1.5B",
                ExtractorVariant::Default => "3B",
            },
            self.approx_size_mb(),
            self.approx_ram_mb()
        )
    }

    /// Multilingual warning surfaced when the Lite variant is selected.
    /// Empty for Default. The text is wired into the install CLI so the
    /// user is told before they commit to the smaller variant.
    pub fn multilingual_warning(self) -> &'static str {
        match self {
            ExtractorVariant::Lite => {
                "Note: lite variant has weaker non-English extraction. \
                 For Russian / Chinese / mixed-language content, consider \
                 --variant default (~1.93 GB on disk)."
            }
            ExtractorVariant::Default => "",
        }
    }
}

// ===== Tests =====

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_lowercase_canonical() {
        assert_eq!(
            ExtractorVariant::parse("lite"),
            Some(ExtractorVariant::Lite)
        );
        assert_eq!(
            ExtractorVariant::parse("default"),
            Some(ExtractorVariant::Default)
        );
    }

    #[test]
    fn parse_accepts_size_aliases() {
        // Aliases let the user write "1.5b" or "3b" — friendlier than
        // forcing them to remember "lite" vs "default" especially
        // when they're choosing by spec rather than role.
        assert_eq!(
            ExtractorVariant::parse("1.5b"),
            Some(ExtractorVariant::Lite)
        );
        assert_eq!(
            ExtractorVariant::parse("3b"),
            Some(ExtractorVariant::Default)
        );
        assert_eq!(
            ExtractorVariant::parse("small"),
            Some(ExtractorVariant::Lite)
        );
    }

    #[test]
    fn parse_is_case_and_whitespace_tolerant() {
        assert_eq!(
            ExtractorVariant::parse("  LITE  "),
            Some(ExtractorVariant::Lite)
        );
        assert_eq!(
            ExtractorVariant::parse("Default"),
            Some(ExtractorVariant::Default)
        );
    }

    #[test]
    fn parse_empty_returns_default_variant() {
        // Empty input maps to Default rather than None so a CLI that
        // forgets to pass --variant lands on the recommended choice.
        assert_eq!(
            ExtractorVariant::parse(""),
            Some(ExtractorVariant::Default)
        );
    }

    #[test]
    fn parse_unknown_returns_none() {
        // None — not silent fallback — so the caller logs the unknown
        // value and decides (warning + Default fallback, hard error,
        // etc.) explicitly. Mirrors the Cardinality::parse contract.
        assert_eq!(ExtractorVariant::parse("turbo"), None);
        assert_eq!(ExtractorVariant::parse("qwen-72b"), None);
    }

    #[test]
    fn default_variant_is_default() {
        assert_eq!(ExtractorVariant::default(), ExtractorVariant::Default);
    }

    #[test]
    fn wire_format_round_trips_via_as_str_parse() {
        for v in [ExtractorVariant::Lite, ExtractorVariant::Default] {
            assert_eq!(ExtractorVariant::parse(v.as_str()), Some(v));
        }
    }

    #[test]
    fn gguf_filenames_distinguish_variants() {
        // Important: the two variants store under different filenames
        // so a user can swap without redownloading whichever they
        // already have. If this collapses, the migration path breaks.
        let a = ExtractorVariant::Lite.gguf_filename();
        let b = ExtractorVariant::Default.gguf_filename();
        assert_ne!(a, b);
        assert!(a.ends_with(".gguf"));
        assert!(b.ends_with(".gguf"));
    }

    #[test]
    fn hf_urls_come_from_qwen_official_team() {
        // Defensive: if a refactor ever rewires us to a third-party
        // mirror we want the test to scream. The Qwen team's own
        // GGUF release is the authoritative source.
        for v in [ExtractorVariant::Lite, ExtractorVariant::Default] {
            let url = v.hf_url();
            assert!(
                url.starts_with("https://huggingface.co/Qwen/"),
                "extractor variant {v:?} must download from Qwen's official HF org"
            );
            assert!(url.ends_with(".gguf"));
        }
    }

    #[test]
    fn approx_sizes_are_in_expected_bands() {
        // The numbers we promise the user in `describe()` and the
        // install confirmation. If a future requantization changes
        // the bytes we want the tests to flag it so the user-facing
        // copy stays accurate.
        assert!(ExtractorVariant::Lite.approx_size_mb() < 1100);
        assert!(ExtractorVariant::Default.approx_size_mb() < 2100);
        assert!(ExtractorVariant::Default.approx_size_mb() > ExtractorVariant::Lite.approx_size_mb());
    }

    #[test]
    fn lite_carries_multilingual_warning_default_does_not() {
        // Multilingual warning is a behaviour contract surfaced by
        // the install CLI. If a future refactor accidentally clears
        // it on Lite, the user gets the smaller model without being
        // told about the trade-off.
        assert!(!ExtractorVariant::Lite
            .multilingual_warning()
            .is_empty());
        assert_eq!(
            ExtractorVariant::Default.multilingual_warning(),
            ""
        );
    }

    #[test]
    fn describe_contains_size_and_ram() {
        // describe() is what the user sees in the install prompt; we
        // want both numbers in the same line so the trade-off is
        // visible at the moment of confirmation, not after.
        let s = ExtractorVariant::Default.describe();
        assert!(s.contains("MB on disk"));
        assert!(s.contains("MB RAM"));
        assert!(s.contains("3B"));
    }
}
