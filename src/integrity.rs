//! Pinned SHA-256 checksums for downloaded runtime artifacts (audit #6).
//!
//! `util::download_file` verifies fail-closed against these. Artifacts without a
//! pin (e.g. a user-chosen custom embedding model, or a non-Linux-x64 platform)
//! download with a printed warning instead - never with a silently-trusted blob.

/// ONNX Runtime archive - linux x64, v1.24.2.
pub const ORT_LINUX_X64_1_24_2: &str =
    "43725474ba5663642e17684717946693850e2005efbd724ac72da278fead25e6";
/// Qdrant server archive - linux x64 (gnu), v1.18.1.
///
/// Note: the gnu build requires glibc 2.38 (Ubuntu 24.10+). It silently fails
/// on every Ubuntu LTS older than 24.10 because mgimind launches it in the
/// background and only sees "Qdrant started but not responding". Production
/// path is the musl pin below; this gnu pin is kept for downstream tools that
/// pick up the constant by name.
pub const QDRANT_LINUX_X64_1_18_1: &str =
    "e359f322a65eb6662bf5ad12ae2228bc94fde77761461c4179ba12f137b8c76d";
/// Qdrant server archive - linux x64 (musl), v1.18.1.
///
/// Statically linked, no glibc dependency. Default for `doctor --fix` on Linux
/// x64 so the bundled Qdrant works on Ubuntu 22.04 LTS, Debian 12, etc.
pub const QDRANT_LINUX_X64_1_18_1_MUSL: &str =
    "4df4cdfa9db20fcb49b470b35d0d001a9c4ec7ac28bfb3df4f24545524271e67";
/// Legacy embedding model (sentence-transformers/all-MiniLM-L6-v2) ONNX weights.
pub const MODEL_MINILM_ONNX: &str =
    "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452";
/// Legacy embedding model tokenizer.json.
pub const MODEL_MINILM_TOKENIZER: &str =
    "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037";

/// Default embedding model (Xenova/multilingual-e5-base) quantized ONNX weights.
/// CPU variant (INT8). Falls back to CPU on the ORT CUDA EP — `MatMulInteger`
/// is not implemented on CUDA, so this is the wrong variant for GPU runs.
pub const MODEL_E5_BASE_ONNX: &str =
    "df7a9a29309e3ad491e1783adf8baee710262cc06079c7cbab63c630277fac94";
/// FP16 variant of the same model. Use this on GPU (`MGIMIND_MODEL_VARIANT=gpu`);
/// the whole graph stays on the device instead of falling back to CPU.
/// Pinned 2026-06-04 from `Xenova/multilingual-e5-base/onnx/model_fp16.onnx`
/// (the file used in the v0.14.3 GPU R@5 = 99.2% headline run).
pub const MODEL_E5_BASE_ONNX_FP16: &str =
    "5d760477f691b665da2b94e1528eb6938b795f76064d9392e6af7118b8a3f54a";
/// Default embedding model tokenizer.json. Shared between CPU/GPU variants.
pub const MODEL_E5_BASE_TOKENIZER: &str =
    "62c24cdc13d4c9952d63718d6c9fa4c287974249e16b7ade6d5a85e7bbb75626";
/// MiniLM-L6-v2 FP16 variant. Optional GPU companion to `MODEL_MINILM_ONNX`
/// (which is fp32 — kept as the baseline file shipped from sentence-transformers).
/// Empty until we pin the FP16 mirror; today `--variant=gpu` for MiniLM falls
/// back to fp32 with the standard download path. Kept as a reserved slot so
/// the eventual pin lands as a one-line change.
#[allow(dead_code)]
pub const MODEL_MINILM_ONNX_FP16: &str = "";
/// Default reranker (Xenova/bge-reranker-base) quantized ONNX weights.
pub const RERANK_BGE_BASE_ONNX: &str =
    "dd98f3e67837d23210a6b7550c08cced4f61845b940ac45be3565840a10f3244";
/// Default reranker tokenizer.json.
pub const RERANK_BGE_BASE_TOKENIZER: &str =
    "48564c5c7d3fa64d85d95e65414a542385f88b0f128fd8d4163fd7a57f2be05c";

// ===== v1.4 Phase 5: local LLM auto-extractor (Qwen 2.5 family GGUF) =====
//
// Both variants come from the Qwen team's official HuggingFace release
// (Qwen/Qwen2.5-{1.5B,3B}-Instruct-GGUF). Pins are filled in after
// step 5.2 downloads the artifacts and computes their sha256; until
// then `pin()` returns None and `util::download_file` issues a warning
// rather than failing fast — the v1.4 Phase 5 install path is
// explicitly opt-in, so an unpinned download under a clear warning is
// acceptable for the bootstrap window.

/// Qwen 2.5 1.5B Instruct Q4_K_M GGUF — the Lite variant of the
/// auto-extractor. ~990 MB on disk, ~1.5 GB RAM loaded.
/// Pinned 2026-06-04 from Qwen/Qwen2.5-1.5B-Instruct-GGUF official release.
pub const EXTRACTOR_QWEN_1_5B_Q4_K_M: &str =
    "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e";

/// Qwen 2.5 3B Instruct Q4_K_M GGUF — the Default variant of the
/// auto-extractor. ~1.93 GB on disk, ~2.5 GB RAM loaded.
/// Pinned 2026-06-04 from Qwen/Qwen2.5-3B-Instruct-GGUF official release.
pub const EXTRACTOR_QWEN_3B_Q4_K_M: &str =
    "626b4a6678b86442240e33df819e00132d3ba7dddfe1cdc4fbb18e0a9615c62d";

/// llama.cpp Vulkan-enabled prebuilt binary archive for Linux x86_64
/// (build b9496). Vulkan was chosen as the GPU backend because it works
/// on both NVIDIA and AMD without a separate per-vendor build, and the
/// upstream llama.cpp project does NOT ship a CUDA-Linux prebuilt — only
/// CUDA-Windows. Vulkan keeps us prebuilt-only across platforms.
///
/// On a single-binary distribution model, `mgimind extractor install`
/// downloads this tarball, extracts the `llama-server` binary + its
/// shared libraries into `$MGIMIND_HOME/bin/extractor/`, and pins this
/// hash for fail-closed verification.
pub const LLAMA_CPP_LINUX_VULKAN_B9496: &str =
    "e4956d4945b4929cf412e9954712267f58d8d179c44f8b6b65d372c4725a5350";

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
            QDRANT_LINUX_X64_1_18_1_MUSL,
        ] {
            assert!(pin(h).is_some(), "default artifact must be pinned");
            assert_eq!(h.len(), 64, "sha256 hex must be 64 chars");
        }
    }
}
