//! Recent-activity vectors, the corpus mean, and the origin-context store.
//!
//! Three things the doubt window needs that the store did not have.
//!
//! **Recent activity.** A rolling buffer of the query embeddings this process
//! has served. Its centroid is "what this session is about right now", which is
//! the `current_centroid_vec` half of `doubt::is_context_drifted`.
//!
//! **The corpus mean.** Drift is measured on *centered* vectors, and that is not
//! a refinement, it is what makes the comparison work at all. Measured on the
//! live store (400 memories, 79800 pairs, multilingual-e5-base): raw cosine ran
//! from 0.666 to 0.972 with a median of 0.779, and not one pair fell below 0.6.
//! The mean vector of that sample had norm 0.88, so almost all of the cosine was
//! a common-mode component shared by every embedding rather than anything about
//! the text. Same-library and cross-library pairs were indistinguishable (median
//! 0.779 against 0.776). Subtracting the mean separates them: same-library pairs
//! then reach 0.869 while cross-library ones stop at 0.378. e5 anisotropy is
//! well known; the cost of ignoring it here was a drift threshold that could
//! never fire.
//!
//! **Origin contexts.** Facts are vectorless by design (audit #6) and that stays
//! true: a fact stores a short `origin_context_id`, not 768 floats. The vectors
//! live in one side table, and because every fact written during one stretch of
//! work shares the same context, that table grows per session rather than per
//! fact.

use crate::config::MindConfig;
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// How many recent query embeddings define "the current context". Large enough
/// that one off-topic lookup cannot swing the centroid, small enough that the
/// buffer still turns over within a working session.
pub const RECENT_CAP: usize = 64;

/// How many memories to sample when computing the corpus mean. The mean of a
/// few hundred vectors is already stable to three decimals in this space; the
/// sample exists to keep startup cheap on a large store, not for accuracy.
pub const CORPUS_SAMPLE_N: usize = 512;

/// Recompute the corpus mean when the cached one is older than this. The mean
/// moves only as the subject matter of the whole store moves, which is slow.
const CORPUS_MEAN_MAX_AGE_HOURS: i64 = 24 * 7;

/// Cap on the origin-context side table. One entry per distinct context, so
/// this holds roughly the last few hundred working sessions.
const CONTEXT_STORE_CAP: usize = 512;

/// How many drift observations to keep for calibration. Percentiles over a few
/// thousand samples are stable enough to set a threshold from.
const DRIFT_SAMPLES_CAP: usize = 4096;

/// Flush the drift journal after this many new observations.
const DRIFT_FLUSH_EVERY: usize = 32;

static RECENT: OnceCell<Mutex<VecDeque<Vec<f32>>>> = OnceCell::new();
static CORPUS_MEAN: OnceCell<Option<Vec<f32>>> = OnceCell::new();
static CONTEXTS: OnceCell<Mutex<HashMap<String, ContextEntry>>> = OnceCell::new();
static DRIFT: OnceCell<Mutex<DriftJournal>> = OnceCell::new();

fn recent() -> &'static Mutex<VecDeque<Vec<f32>>> {
    RECENT.get_or_init(|| Mutex::new(VecDeque::with_capacity(RECENT_CAP)))
}

// ===== Recent activity =====

/// Record one query embedding. Called from the search path with the vector it
/// already computed, so this costs no inference.
pub fn record_query(vector: &[f32]) {
    if vector.is_empty() {
        return;
    }
    let mut buf = recent().lock();
    if buf.len() == RECENT_CAP {
        buf.pop_front();
    }
    buf.push_back(vector.to_vec());
}

/// Centroid of the recent-activity buffer, or `None` when nothing has been
/// searched yet in this process. `None` means "no signal", and every caller
/// treats that as "not drifted" rather than guessing.
pub fn current_centroid() -> Option<Vec<f32>> {
    let buf = recent().lock();
    if buf.is_empty() {
        return None;
    }
    let vectors: Vec<Vec<f32>> = buf.iter().cloned().collect();
    drop(buf);
    let c = crate::doubt::centroid(&vectors);
    if c.is_empty() { None } else { Some(c) }
}

/// How many query vectors are currently buffered. Reported by `calibrate` so a
/// reader can tell an empty buffer from a genuinely non-drifting one.
pub fn recent_len() -> usize {
    recent().lock().len()
}

// ===== Corpus mean =====

#[derive(Serialize, Deserialize)]
struct CorpusMeanFile {
    dim: usize,
    sampled: usize,
    computed_at: String,
    /// Norm of the mean before normalisation. 0 would be an isotropic space; the
    /// closer to 1, the more of every cosine in this store is common-mode. Kept
    /// because it is the number that justifies centering at all.
    anisotropy: f32,
    vector: Vec<f32>,
}

fn corpus_mean_path(config: &MindConfig) -> std::path::PathBuf {
    config.data_dir.join("corpus_mean.json")
}

/// The corpus mean vector, sampled from the memories collection and cached on
/// disk. Returns `None` on an empty store, which disables drift rather than
/// centering against nothing.
pub async fn corpus_mean(config: &MindConfig) -> Option<Vec<f32>> {
    if let Some(cached) = CORPUS_MEAN.get() {
        return cached.clone();
    }
    let computed = load_or_compute_corpus_mean(config).await;
    // A concurrent caller may have won the race; its value is equally valid.
    let _ = CORPUS_MEAN.set(computed.clone());
    computed
}

async fn load_or_compute_corpus_mean(config: &MindConfig) -> Option<Vec<f32>> {
    let path = corpus_mean_path(config);
    if let Some(file) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<CorpusMeanFile>(&s).ok())
        && file.dim == config.vector_size as usize
        && !is_stale(&file.computed_at)
    {
        return Some(file.vector);
    }

    let sample = crate::storage::sample_dense_vectors(config, CORPUS_SAMPLE_N)
        .await
        .ok()?;
    if sample.is_empty() {
        return None;
    }
    let dim = sample[0].len();
    let mut mean = vec![0.0f32; dim];
    for v in &sample {
        if v.len() != dim {
            continue;
        }
        for (i, x) in v.iter().enumerate() {
            mean[i] += x;
        }
    }
    let n = sample.len() as f32;
    for x in &mut mean {
        *x /= n;
    }
    let anisotropy = mean.iter().map(|x| x * x).sum::<f32>().sqrt();

    let file = CorpusMeanFile {
        dim,
        sampled: sample.len(),
        computed_at: chrono::Utc::now().to_rfc3339(),
        anisotropy,
        vector: mean.clone(),
    };
    if let Ok(json) = serde_json::to_string(&file) {
        let _ = crate::util::atomic_write_str(&path, &json);
    }
    Some(mean)
}

fn is_stale(computed_at: &str) -> bool {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(computed_at) else {
        return true;
    };
    chrono::Utc::now()
        .signed_duration_since(then.with_timezone(&chrono::Utc))
        .num_hours()
        > CORPUS_MEAN_MAX_AGE_HOURS
}

/// Anisotropy of the cached corpus mean: the norm of the mean vector before
/// normalisation. Reported by `calibrate`. `None` when no mean is cached yet.
pub fn cached_anisotropy(config: &MindConfig) -> Option<f32> {
    std::fs::read_to_string(corpus_mean_path(config))
        .ok()
        .and_then(|s| serde_json::from_str::<CorpusMeanFile>(&s).ok())
        .map(|f| f.anisotropy)
}

// ===== Origin-context side table =====

#[derive(Clone, Serialize, Deserialize)]
struct ContextEntry {
    vector: Vec<f32>,
    created_at: String,
}

fn contexts_path(config: &MindConfig) -> std::path::PathBuf {
    config.data_dir.join("doubt_contexts.json")
}

fn contexts(config: &MindConfig) -> &'static Mutex<HashMap<String, ContextEntry>> {
    CONTEXTS.get_or_init(|| {
        let loaded = std::fs::read_to_string(contexts_path(config))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

/// Content-address a context vector. Identical contexts collapse to one entry,
/// which is what keeps the table per-session rather than per-fact.
fn context_id(vector: &[f32]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for x in vector {
        hasher.update(x.to_le_bytes());
    }
    hex::encode(hasher.finalize())[..16].to_string()
}

/// Store a context vector and return its id, for the `origin_context_id` field
/// on a fact. Returns `None` for an empty vector so a caller with no context
/// signal writes no field at all.
pub fn store_origin_context(config: &MindConfig, vector: &[f32]) -> Option<String> {
    if vector.is_empty() {
        return None;
    }
    let id = context_id(vector);
    let snapshot = {
        let mut map = contexts(config).lock();
        if !map.contains_key(&id) {
            map.insert(
                id.clone(),
                ContextEntry {
                    vector: vector.to_vec(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            );
            evict_oldest(&mut map);
        }
        map.clone()
    };
    // Serialise and write outside the lock: a reader on the retrieval path must
    // never wait behind an fsync.
    if let Ok(json) = serde_json::to_string(&snapshot) {
        let _ = crate::util::atomic_write_str(&contexts_path(config), &json);
    }
    Some(id)
}

fn evict_oldest(map: &mut HashMap<String, ContextEntry>) {
    while map.len() > CONTEXT_STORE_CAP {
        let Some(oldest) = map
            .iter()
            .min_by(|a, b| a.1.created_at.cmp(&b.1.created_at))
            .map(|(k, _)| k.clone())
        else {
            return;
        };
        map.remove(&oldest);
    }
}

/// Look up a stored origin context. A miss (evicted, or a fact written before
/// this existed) means the fact has no origin signal and is never counted as
/// drifted.
pub fn load_origin_context(config: &MindConfig, id: &str) -> Option<Vec<f32>> {
    contexts(config).lock().get(id).map(|e| e.vector.clone())
}

// ===== Drift observations (shadow mode) =====

#[derive(Default, Serialize, Deserialize)]
struct DriftJournal {
    /// Ring of recent drift values, capped at `DRIFT_SAMPLES_CAP`.
    samples: VecDeque<f32>,
    /// Lifetime count, which keeps meaning after the ring wraps.
    observed: u64,
    #[serde(skip)]
    unflushed: usize,
}

fn drift_path(config: &MindConfig) -> std::path::PathBuf {
    config.data_dir.join("doubt_drift_samples.json")
}

fn drift(config: &MindConfig) -> &'static Mutex<DriftJournal> {
    DRIFT.get_or_init(|| {
        let loaded = std::fs::read_to_string(drift_path(config))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

/// Record one observed drift value. This is the whole of shadow mode: the
/// number is measured and kept, and nothing acts on it. Calibration reads these
/// samples to propose a threshold, which is the only honest way to set one.
pub fn observe_drift(config: &MindConfig, value: f32) {
    let snapshot = {
        let mut j = drift(config).lock();
        if j.samples.len() >= DRIFT_SAMPLES_CAP {
            j.samples.pop_front();
        }
        j.samples.push_back(value);
        j.observed += 1;
        j.unflushed += 1;
        if j.unflushed < DRIFT_FLUSH_EVERY {
            return;
        }
        j.unflushed = 0;
        DriftJournal {
            samples: j.samples.clone(),
            observed: j.observed,
            unflushed: 0,
        }
    };
    if let Ok(json) = serde_json::to_string(&snapshot) {
        let _ = crate::util::atomic_write_str(&drift_path(config), &json);
    }
}

/// Observed drift values, oldest first. Empty until the mechanism has run.
pub fn drift_samples(config: &MindConfig) -> Vec<f32> {
    drift(config).lock().samples.iter().copied().collect()
}

/// Lifetime count of drift observations, which survives the ring wrapping.
pub fn drift_observed(config: &MindConfig) -> u64 {
    drift(config).lock().observed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_buffer_is_bounded_and_keeps_the_newest() {
        for i in 0..(RECENT_CAP + 10) {
            record_query(&[i as f32, 0.0, 0.0]);
        }
        assert_eq!(recent_len(), RECENT_CAP);
        let buf = recent().lock();
        // The oldest entries were dropped, so the front is no longer vector 0.
        assert!(buf.front().unwrap()[0] >= 10.0);
    }

    #[test]
    fn empty_query_vector_is_ignored() {
        let before = recent_len();
        record_query(&[]);
        assert_eq!(recent_len(), before);
    }

    #[test]
    fn context_id_is_content_addressed() {
        let a = vec![0.1, 0.2, 0.3];
        let b = vec![0.1, 0.2, 0.3];
        let c = vec![0.1, 0.2, 0.4];
        assert_eq!(context_id(&a), context_id(&b));
        assert_ne!(context_id(&a), context_id(&c));
    }

    #[test]
    fn origin_context_round_trips_through_the_side_table() {
        // The side table is a process global keyed off the first config it
        // sees, so this test owns the whole round trip rather than asserting
        // across tests.
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::MindConfig {
            data_dir: dir.path().to_path_buf(),
            ..crate::config::MindConfig::default()
        };
        let vector = vec![0.3f32, 0.4, 0.5];
        let id = store_origin_context(&config, &vector).expect("a non-empty vector gets an id");
        assert_eq!(load_origin_context(&config, &id), Some(vector));
        assert_eq!(load_origin_context(&config, "no-such-context"), None);
        // An empty context is "no signal", not an entry.
        assert_eq!(store_origin_context(&config, &[]), None);
    }

    #[test]
    fn eviction_drops_the_oldest_entry() {
        let mut map = HashMap::new();
        for i in 0..(CONTEXT_STORE_CAP + 3) {
            map.insert(
                format!("id{i}"),
                ContextEntry {
                    vector: vec![i as f32],
                    created_at: format!("2026-01-01T00:00:{:02}Z", i % 60),
                },
            );
        }
        evict_oldest(&mut map);
        assert_eq!(map.len(), CONTEXT_STORE_CAP);
    }
}
