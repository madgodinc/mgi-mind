//! Pinned SHA-256 checksums for downloaded runtime artifacts (audit #6).
//!
//! `util::download_file` verifies fail-closed against these. Artifacts without a
//! pin (e.g. a user-chosen custom embedding model, or a non-Linux-x64 platform)
//! download with a printed warning instead - never with a silently-trusted blob.

/// ONNX Runtime archive - linux x64, v1.24.2.
pub const ORT_LINUX_X64_1_24_2: &str =
    "43725474ba5663642e17684717946693850e2005efbd724ac72da278fead25e6";
/// Qdrant server archive - linux x64 (gnu), v1.18.1.
pub const QDRANT_LINUX_X64_1_18_1: &str =
    "e359f322a65eb6662bf5ad12ae2228bc94fde77761461c4179ba12f137b8c76d";
/// Legacy embedding model (sentence-transformers/all-MiniLM-L6-v2) ONNX weights.
pub const MODEL_MINILM_ONNX: &str =
    "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452";
/// Legacy embedding model tokenizer.json.
pub const MODEL_MINILM_TOKENIZER: &str =
    "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037";

/// Default embedding model (Xenova/multilingual-e5-base) quantized ONNX weights.
pub const MODEL_E5_BASE_ONNX: &str =
    "df7a9a29309e3ad491e1783adf8baee710262cc06079c7cbab63c630277fac94";
/// Default embedding model tokenizer.json.
pub const MODEL_E5_BASE_TOKENIZER: &str =
    "62c24cdc13d4c9952d63718d6c9fa4c287974249e16b7ade6d5a85e7bbb75626";
/// Default reranker (Xenova/bge-reranker-base) quantized ONNX weights.
pub const RERANK_BGE_BASE_ONNX: &str =
    "dd98f3e67837d23210a6b7550c08cced4f61845b940ac45be3565840a10f3244";
/// Default reranker tokenizer.json.
pub const RERANK_BGE_BASE_TOKENIZER: &str =
    "48564c5c7d3fa64d85d95e65414a542385f88b0f128fd8d4163fd7a57f2be05c";

/// Treat the placeholder/empty as "no pin available".
pub fn pin(hash: &str) -> Option<&str> {
    if hash.is_empty() || hash == "PIN_ME" {
        None
    } else {
        Some(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_and_empty_are_unpinned() {
        assert_eq!(pin(""), None);
        assert_eq!(pin("PIN_ME"), None);
    }

    #[test]
    fn real_default_models_are_pinned() {
        // The actual default stack must be fail-closed (audit #6 regression guard).
        for h in [
            MODEL_E5_BASE_ONNX,
            MODEL_E5_BASE_TOKENIZER,
            RERANK_BGE_BASE_ONNX,
            RERANK_BGE_BASE_TOKENIZER,
            ORT_LINUX_X64_1_24_2,
            QDRANT_LINUX_X64_1_18_1,
        ] {
            assert!(pin(h).is_some(), "default artifact must be pinned");
            assert_eq!(h.len(), 64, "sha256 hex must be 64 chars");
        }
    }
}
