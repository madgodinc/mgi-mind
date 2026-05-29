//! Pinned SHA-256 checksums for downloaded runtime artifacts (audit #6).
//!
//! `util::download_file` verifies fail-closed against these. Artifacts without a
//! pin (e.g. a user-chosen custom embedding model, or a non-Linux-x64 platform)
//! download with a printed warning instead — never with a silently-trusted blob.

/// ONNX Runtime archive — linux x64, v1.24.2.
pub const ORT_LINUX_X64_1_24_2: &str =
    "43725474ba5663642e17684717946693850e2005efbd724ac72da278fead25e6";
/// Qdrant server archive — linux x64 (gnu), v1.18.1.
pub const QDRANT_LINUX_X64_1_18_1: &str =
    "e359f322a65eb6662bf5ad12ae2228bc94fde77761461c4179ba12f137b8c76d";
/// Default embedding model (sentence-transformers/all-MiniLM-L6-v2) ONNX weights.
pub const MODEL_MINILM_ONNX: &str =
    "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452";
/// Default embedding model tokenizer.json.
pub const MODEL_MINILM_TOKENIZER: &str =
    "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037";

/// Treat the placeholder/empty as "no pin available".
pub fn pin(hash: &str) -> Option<&str> {
    if hash.is_empty() || hash == "PIN_ME" {
        None
    } else {
        Some(hash)
    }
}
