//! Hardware profiles: three factory presets that distribute the brain across
//! GPU / VRAM / RAM / CPU for different machines. One set of weights, three
//! deployment shapes — the user picks by their hardware instead of hand-tuning
//! six env vars.
//!
//!   Light    — weakest PCs (or no GPU): minimize VRAM, lean on RAM/CPU.
//!              Extractor mostly/fully on CPU, KV cache in RAM, embedder CPU
//!              (INT8), reranker off. Slow but runs on a laptop.
//!   Balanced — typical mid-range PC with a modest GPU (~6-8 GB): split the
//!              extractor across VRAM and RAM, KV in RAM, embedder on GPU
//!              (small), reranker on. Good speed without filling VRAM.
//!   Max      — top PC with a big GPU (12-16 GB+): everything on GPU, KV on
//!              GPU, full reranker. Fastest.
//!
//! Resolution order: an explicit env var (MGIMIND_NGL etc.) always wins, so
//! power users keep fine control; the profile only supplies the defaults.

/// Factory hardware preset. Selected via config `hardware_profile` or the
/// MGIMIND_PROFILE env var.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HardwareProfile {
    /// Weakest machines: minimize VRAM, lean on RAM/CPU.
    Light,
    /// Typical mid-range GPU: split for good speed at modest VRAM.
    #[default]
    Balanced,
    /// Top GPU: everything resident, maximum speed.
    Max,
}

/// Concrete settings a profile resolves to.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedHardware {
    /// Extractor GPU layers (`-ngl`). 0 = pure CPU; 99 = all on GPU.
    pub ngl: u32,
    /// Keep the extractor KV cache in RAM (`--no-kv-offload`) instead of VRAM.
    pub kv_on_ram: bool,
    /// Run the embedder on GPU (FP16). False = CPU (INT8, smaller, slower).
    pub embedder_on_gpu: bool,
    /// Run the cross-encoder reranker (quality boost, extra compute).
    pub reranker_enabled: bool,
}

impl HardwareProfile {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" | "low" | "min" | "cpu" => Some(Self::Light),
            "balanced" | "balance" | "mid" | "medium" | "default" | "" => Some(Self::Balanced),
            "max" | "high" | "full" | "performance" | "gpu" => Some(Self::Max),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Balanced => "balanced",
            Self::Max => "max",
        }
    }

    /// The concrete knob values for this profile. Granite-8B has ~40 layers; the
    /// Balanced split keeps the heavy compute on the GPU while the tail + KV sit
    /// in RAM, freeing VRAM for the (small) embedder and reranker.
    pub fn resolve(self) -> ResolvedHardware {
        match self {
            Self::Light => ResolvedHardware {
                ngl: 0,             // extractor on CPU/RAM
                kv_on_ram: true,    // KV in RAM too
                embedder_on_gpu: false, // CPU INT8
                reranker_enabled: false, // skip the extra model
            },
            Self::Balanced => ResolvedHardware {
                ngl: 24,            // ~60% of layers on GPU, rest in RAM
                kv_on_ram: true,    // KV in RAM keeps VRAM for embedder+reranker
                embedder_on_gpu: true,
                reranker_enabled: true,
            },
            Self::Max => ResolvedHardware {
                ngl: 99,            // whole extractor on GPU
                kv_on_ram: false,   // KV on GPU = fastest
                embedder_on_gpu: true,
                reranker_enabled: true,
            },
        }
    }
}

/// Active profile: explicit env (MGIMIND_PROFILE) wins, else the passed config
/// value, else the Balanced default.
pub fn active(config_profile: HardwareProfile) -> HardwareProfile {
    if let Ok(s) = std::env::var("MGIMIND_PROFILE") {
        if let Some(p) = HardwareProfile::parse(&s) {
            return p;
        }
    }
    config_profile
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_parse_round_trip() {
        for p in [HardwareProfile::Light, HardwareProfile::Balanced, HardwareProfile::Max] {
            assert_eq!(HardwareProfile::parse(p.as_str()), Some(p));
        }
        assert_eq!(HardwareProfile::parse("LOW"), Some(HardwareProfile::Light));
        assert_eq!(HardwareProfile::parse("performance"), Some(HardwareProfile::Max));
        assert_eq!(HardwareProfile::parse(""), Some(HardwareProfile::Balanced));
        assert_eq!(HardwareProfile::parse("garbage"), None);
    }

    #[test]
    fn light_minimizes_vram_max_maximizes() {
        let l = HardwareProfile::Light.resolve();
        let m = HardwareProfile::Max.resolve();
        assert_eq!(l.ngl, 0);
        assert!(l.kv_on_ram && !l.embedder_on_gpu);
        assert_eq!(m.ngl, 99);
        assert!(!m.kv_on_ram && m.embedder_on_gpu && m.reranker_enabled);
    }

    #[test]
    fn balanced_is_a_real_split() {
        let b = HardwareProfile::Balanced.resolve();
        assert!(b.ngl > 0 && b.ngl < 99); // genuinely split, not all-or-nothing
        assert!(b.kv_on_ram && b.embedder_on_gpu);
    }
}
