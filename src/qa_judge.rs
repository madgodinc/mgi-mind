//! QA-accuracy benchmark (phase Д-judge) — the **comparable** number.
//!
//! `bench.rs` measures retrieval recall (R@k): did the right session surface?
//! That is zero-API and honest, but it is NOT what mem0/Zep headline. They
//! report **QA accuracy**: an answerer-LLM writes an answer from the retrieved
//! context, then a judge-LLM scores it against the gold answer. To compare
//! apples-to-apples we must reproduce that pipeline.
//!
//! What this module is (and is NOT):
//!   * IS: retrieve (our pipeline) -> assemble context -> answerer-LLM ->
//!     judge-LLM -> yes/no -> accuracy. Paid API, opt-in, never on the default
//!     path. The zero-API R@k in `bench.rs` stays the on-brand headline.
//!   * IS NOT: a retrieval metric. R@k lives in `bench.rs`. Do not conflate.
//!
//! Strictness as a first-class axis (the product story — see
//! project challenger-positioning): the same run is scored under multiple
//! judge profiles so the published artifact is a *curve over strictness*, not
//! one number:
//!   * `Mem0`       — mem0's own judge prompt, verbatim. Lenient ("lean toward
//!                    yes"). Reproduces their 94.4% comparison 1:1.
//!   * `Canonical`  — LongMemEval paper's per-type judge prompts. Stricter.
//!   * `Strict`     — our own, stricter still. Formalized criteria, not "be
//!                    picky" (that would be noise).
//!
//! Symmetry rule (non-negotiable for a fair claim): if we score OUR system
//! under `Strict`, a head-to-head against mem0 must score THEIR baseline under
//! `Strict` too. A strict judge applied only to ourselves is asymmetric and
//! invalid. This module makes the profile a parameter precisely so the same
//! profile can grade any system's outputs.
//!
//! Provider-agnostic: answerer and judge are independent `LlmClient`s, so the
//! triple-judge plan (gpt-4o = their published judge, gpt-5 = their code
//! default + modern bar, claude-sonnet = cross-vendor check) is just three
//! judge configs over the same answers. API keys come from the environment
//! (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`) — never compiled in, never logged.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Judge strictness profile. Selects which prompt template family scores an
/// answer, and (for `Canonical`) whether per-question-type prompts are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeProfile {
    /// mem0's own unified judge prompt (lenient, single prompt for all types).
    Mem0,
    /// LongMemEval paper's canonical per-type judge prompts (stricter).
    Canonical,
    /// Our own formalized-strict judge (strictest).
    Strict,
}

impl JudgeProfile {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mem0" => Some(Self::Mem0),
            "canonical" | "paper" => Some(Self::Canonical),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mem0 => "mem0",
            Self::Canonical => "canonical",
            Self::Strict => "strict",
        }
    }
}

/// One chat message in the provider-neutral shape. Both OpenAI and Anthropic
/// reduce to (role, content); we keep system separate where the API wants it.
#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// Sampling knobs. `temperature: None` means "omit the field" — required for
/// gpt-5/o-series which reject non-default temperature (mem0 special-cases this
/// in llm_client.py; we mirror it).
#[derive(Debug, Clone)]
pub struct GenParams {
    pub temperature: Option<f32>,
    pub max_tokens: u32,
}

impl Default for GenParams {
    fn default() -> Self {
        // mem0's generate() defaults: temp 0, 4096 tokens (room for CoT).
        Self { temperature: Some(0.0), max_tokens: 4096 }
    }
}

/// Provider-neutral LLM client. One impl per backend; the harness holds two
/// (answerer + judge), which may be different providers.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Model id this client speaks to (for logging / result provenance).
    fn model_id(&self) -> &str;
    /// Single completion. `system` may be empty (mem0 sends "" and puts the
    /// whole prompt in the user turn).
    async fn generate(&self, system: &str, turns: &[ChatTurn], params: &GenParams)
        -> Result<String>;
}

/// Deterministic mock for offline harness tests — NO network, NO keys. Returns
/// canned text keyed by a substring match so the parser/accuracy logic can be
/// unit-tested before any paid call. Real providers (OpenAI/Anthropic) are
/// added behind the same trait once the mock-backed flow is green.
pub struct MockClient {
    pub id: String,
    /// (needle in prompt) -> canned response. First match wins.
    pub rules: Vec<(String, String)>,
    /// Fallback when no rule matches.
    pub default: String,
}

#[async_trait::async_trait]
impl LlmClient for MockClient {
    fn model_id(&self) -> &str {
        &self.id
    }
    async fn generate(
        &self,
        _system: &str,
        turns: &[ChatTurn],
        _params: &GenParams,
    ) -> Result<String> {
        let hay = turns.iter().map(|t| t.content.as_str()).collect::<Vec<_>>().join("\n");
        for (needle, resp) in &self.rules {
            if hay.contains(needle.as_str()) {
                return Ok(resp.clone());
            }
        }
        Ok(self.default.clone())
    }
}

/// File-backed client: a human (or another model in a separate process) plays
/// the LLM. Each `generate` writes the system+user prompt to
/// `{dir}/req_NNNN.txt` and blocks until `{dir}/resp_NNNN.txt` appears, then
/// returns its contents. This is the zero-cost dress rehearsal before paying
/// real judges: it exercises the entire pipeline (prompt assembly, response
/// parsing, SR/PR/IPA mapping, aggregation) against genuine model answers,
/// catching prompt/format bugs before a cent is spent.
pub struct FileClient {
    pub id: String,
    pub dir: std::path::PathBuf,
    /// Shared call counter so req/resp filenames are stable and ordered.
    pub counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Poll interval while waiting for the response file.
    pub poll: Duration,
}

impl FileClient {
    pub fn new(id: impl Into<String>, dir: impl Into<std::path::PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).context("create file-client dir")?;
        Ok(Self {
            id: id.into(),
            dir,
            counter: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            poll: Duration::from_millis(750),
        })
    }
}

#[async_trait::async_trait]
impl LlmClient for FileClient {
    fn model_id(&self) -> &str {
        &self.id
    }
    async fn generate(
        &self,
        system: &str,
        turns: &[ChatTurn],
        _params: &GenParams,
    ) -> Result<String> {
        let n = self.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let req = self.dir.join(format!("req_{n:04}.txt"));
        let resp = self.dir.join(format!("resp_{n:04}.txt"));
        let body = turns
            .iter()
            .map(|t| format!("[{}]\n{}", t.role, t.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        let payload = if system.is_empty() {
            body
        } else {
            format!("[system]\n{system}\n\n{body}")
        };
        std::fs::write(&req, &payload).context("write file-client request")?;

        // Block until the responder writes resp_NNNN.txt.
        loop {
            if resp.exists() {
                let s = std::fs::read_to_string(&resp).context("read file-client response")?;
                if !s.trim().is_empty() {
                    return Ok(s);
                }
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

/// Parse the judge's yes/no verdict, mirroring mem0's `_parse_yes_no_judgment`:
/// only the region AFTER the closing CoT tag counts; scan that region
/// bottom-up for a line that is exactly "yes"/"no"; else last `\b(yes|no)\b`;
/// else startswith("yes"). Pure -> unit-tested without any API.
pub fn parse_yes_no(raw: &str) -> bool {
    // 1. Restrict to the post-CoT region if a closing tag is present.
    let region = ["</judge_thinking>", "</thinking>"]
        .iter()
        .find_map(|tag| raw.rsplit_once(tag).map(|(_, after)| after))
        .unwrap_or(raw);

    // 2. Bottom-up: first line exactly "yes"/"no".
    for line in region.lines().rev() {
        match line.trim().to_ascii_lowercase().as_str() {
            "yes" => return true,
            "no" => return false,
            _ => {}
        }
    }

    // 3. Last standalone yes/no token anywhere in the region.
    let lower = region.to_ascii_lowercase();
    let mut last: Option<bool> = None;
    for tok in lower.split(|c: char| !c.is_ascii_alphabetic()) {
        match tok {
            "yes" => last = Some(true),
            "no" => last = Some(false),
            _ => {}
        }
    }
    if let Some(v) = last {
        return v;
    }

    // 4. Fallback.
    lower.trim_start().starts_with("yes")
}

/// Per-question QA outcome, serialized to the run's raw.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaItemResult {
    pub question_id: String,
    pub question_type: String,
    pub question: String,
    pub gold_answer: String,
    pub generated_answer: String,
    /// verdict per judge profile that was run, e.g. {"mem0": true, "strict": false}
    pub verdicts: std::collections::BTreeMap<String, bool>,
}

/// Aggregate accuracy = correct / total, flat & unweighted (matches mem0's
/// compute_longmemeval_metrics). One tally per judge profile.
#[derive(Debug, Default)]
pub struct QaTally {
    pub total: usize,
    pub correct: usize,
}

impl QaTally {
    pub fn record(&mut self, correct: bool) {
        self.total += 1;
        if correct {
            self.correct += 1;
        }
    }
    pub fn accuracy_pct(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.correct as f64 / self.total as f64 * 100.0
        }
    }
}

// ===== Real provider clients =====
//
// Two backends behind the same `LlmClient` trait so the STALE/QA harness can
// mix providers freely (answerer = OpenAI gpt-4o-mini per A1; judge = Gemini
// flash-lite per the STALE paper). Keys come from the environment, never
// compiled in, never logged. Both reuse a process-wide reqwest client.

static HTTP_QA: once_cell::sync::OnceCell<reqwest::Client> = once_cell::sync::OnceCell::new();

fn qa_http() -> Result<reqwest::Client> {
    if let Some(c) = HTTP_QA.get() {
        return Ok(c.clone());
    }
    let c = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("build qa_judge reqwest client")?;
    let _ = HTTP_QA.set(c.clone());
    Ok(c)
}

/// OpenAI chat-completions client. Used as the **answerer** (gpt-4o-mini, A1)
/// and as an optional judge profile. Reads `OPENAI_API_KEY`.
pub struct OpenAiClient {
    model: String,
    api_key: String,
    /// Override base URL for tests / Azure / proxies. Default OpenAI.
    base_url: String,
}

impl OpenAiClient {
    /// Construct from env. Errors if `OPENAI_API_KEY` is unset so a missing key
    /// fails loudly at setup, not mid-run after spending on the answerer.
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| anyhow!("OPENAI_API_KEY not set (answerer needs it)"))?;
        Ok(Self {
            model: model.into(),
            api_key,
            base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
        })
    }
}

#[async_trait::async_trait]
impl LlmClient for OpenAiClient {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn generate(
        &self,
        system: &str,
        turns: &[ChatTurn],
        params: &GenParams,
    ) -> Result<String> {
        let mut messages: Vec<serde_json::Value> = Vec::with_capacity(turns.len() + 1);
        if !system.is_empty() {
            messages.push(serde_json::json!({"role": "system", "content": system}));
        }
        for t in turns {
            messages.push(serde_json::json!({"role": t.role, "content": t.content}));
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": params.max_tokens,
        });
        // gpt-5 / o-series reject an explicit temperature; `None` = omit (A-note
        // in module header, mirrors mem0's llm_client.py special-case).
        if let Some(temp) = params.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        let resp = qa_http()?
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("openai chat/completions request")?;

        let status = resp.status();
        let text = resp.text().await.context("read openai response body")?;
        if !status.is_success() {
            // Never echo the key; the body may contain a useful error code.
            return Err(anyhow!("openai HTTP {status}: {text}"));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).context("parse openai response json")?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("openai response missing choices[0].message.content: {text}"))?;
        Ok(content.to_string())
    }
}

/// Google Gemini client (generativelanguage v1beta `generateContent`). Used as
/// the **judge** (gemini flash-lite, STALE paper, 95.8% human agreement). Reads
/// `GEMINI_API_KEY`. Gemini has no system role; the system prompt is sent via
/// `system_instruction`.
pub struct GeminiClient {
    model: String,
    api_key: String,
    base_url: String,
    /// When true, request `application/json` response MIME (judge wants JSON).
    json_mode: bool,
    /// When true, let the model think (gemini-flash is a thinking model). Off by
    /// default: form-filling extraction wants the token budget on the answer,
    /// not deliberation. The cross-axis adjudicator needs it ON to reason about
    /// fact incompatibility (desert climate ⊥ rainy-city residence).
    thinking: bool,
}

impl GeminiClient {
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let api_key = std::env::var("GEMINI_API_KEY")
            .map_err(|_| anyhow!("GEMINI_API_KEY not set (judge needs it)"))?;
        Ok(Self {
            model: model.into(),
            api_key,
            base_url: std::env::var("GEMINI_BASE_URL").unwrap_or_else(|_| {
                "https://generativelanguage.googleapis.com/v1beta".to_string()
            }),
            json_mode: false,
            thinking: false,
        })
    }

    /// Enable JSON response MIME — the STALE judge returns a strict JSON object.
    pub fn json_mode(mut self, on: bool) -> Self {
        self.json_mode = on;
        self
    }

    /// Let the model deliberate. Needed by the cross-axis adjudicator (it must
    /// reason that arid climate is incompatible with a rainy-city residence);
    /// leave off for form-filling extraction so the token budget hits the answer.
    pub fn thinking(mut self, on: bool) -> Self {
        self.thinking = on;
        self
    }
}

#[async_trait::async_trait]
impl LlmClient for GeminiClient {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn generate(
        &self,
        system: &str,
        turns: &[ChatTurn],
        params: &GenParams,
    ) -> Result<String> {
        // Gemini "contents" are user/model turns; roles map user->user,
        // assistant->model. System goes in system_instruction.
        let contents: Vec<serde_json::Value> = turns
            .iter()
            .map(|t| {
                let role = if t.role == "assistant" { "model" } else { "user" };
                serde_json::json!({"role": role, "parts": [{"text": t.content}]})
            })
            .collect();

        let mut gen_config = serde_json::json!({
            "maxOutputTokens": params.max_tokens,
        });
        if let Some(temp) = params.temperature {
            gen_config["temperature"] = serde_json::json!(temp);
        }
        if self.json_mode {
            gen_config["responseMimeType"] = serde_json::json!("application/json");
        }
        // gemini-*-flash-latest now resolves to a THINKING model (gemini-3.5-flash)
        // that spends the whole maxOutputTokens budget on internal reasoning and
        // returns an empty/truncated body. Form-filling extraction wants the
        // budget on the answer, so we disable thinking there (thinkingBudget=0)
        // — otherwise slot-extract returns 0 facts. The cross-axis adjudicator,
        // by contrast, NEEDS thinking to infer incompatibility, so it sets
        // .thinking(true) and we leave the budget at the model default.
        if !self.thinking {
            gen_config["thinkingConfig"] = serde_json::json!({ "thinkingBudget": 0 });
        }

        let mut body = serde_json::json!({
            "contents": contents,
            "generationConfig": gen_config,
        });
        if !system.is_empty() {
            body["systemInstruction"] =
                serde_json::json!({"parts": [{"text": system}]});
        }

        let url = format!(
            "{}/models/{}:generateContent",
            self.base_url, self.model
        );
        let resp = qa_http()?
            .post(&url)
            // Key as header, not query string — keeps it out of any URL logging.
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("gemini generateContent request")?;

        let status = resp.status();
        let text = resp.text().await.context("read gemini response body")?;
        if !status.is_success() {
            return Err(anyhow!("gemini HTTP {status}: {text}"));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).context("parse gemini response json")?;
        // candidates[0].content.parts[*].text — concatenate parts.
        let parts = v["candidates"][0]["content"]["parts"]
            .as_array()
            .ok_or_else(|| anyhow!("gemini response missing candidates[0].content.parts: {text}"))?;
        let mut out = String::new();
        for p in parts {
            if let Some(s) = p["text"].as_str() {
                out.push_str(s);
            }
        }
        if out.is_empty() {
            return Err(anyhow!("gemini returned empty text: {text}"));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_yes_no_after_cot_tag() {
        let raw = "<judge_thinking>the answer says no degree but...</judge_thinking>\nyes";
        assert!(parse_yes_no(raw));
    }

    #[test]
    fn parse_yes_no_ignores_no_inside_cot() {
        // "no" appears in CoT, verdict after tag is yes -> must be yes.
        let raw = "<judge_thinking>could be no, but actually correct</judge_thinking>\nYes";
        assert!(parse_yes_no(raw));
    }

    #[test]
    fn parse_yes_no_bottom_up() {
        let raw = "yes this is right\nno wait\nno";
        assert!(!parse_yes_no(raw));
    }

    #[test]
    fn parse_yes_no_fallback_startswith() {
        assert!(parse_yes_no("yes, correct"));
        assert!(!parse_yes_no("nope nothing here either"));
    }

    #[test]
    fn tally_accuracy() {
        let mut t = QaTally::default();
        t.record(true);
        t.record(true);
        t.record(false);
        t.record(true);
        assert!((t.accuracy_pct() - 75.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn mock_client_matches_rule() {
        let c = MockClient {
            id: "mock".into(),
            rules: vec![("Business Administration".into(), "yes".into())],
            default: "no".into(),
        };
        let turns = vec![ChatTurn {
            role: "user".into(),
            content: "gold: Business Administration, response: BA degree".into(),
        }];
        let out = c.generate("", &turns, &GenParams::default()).await.unwrap();
        assert_eq!(out, "yes");
    }

    #[test]
    fn profile_roundtrip() {
        for p in [JudgeProfile::Mem0, JudgeProfile::Canonical, JudgeProfile::Strict] {
            assert_eq!(JudgeProfile::from_str(p.as_str()), Some(p));
        }
        assert_eq!(JudgeProfile::from_str("paper"), Some(JudgeProfile::Canonical));
        assert_eq!(JudgeProfile::from_str("bogus"), None);
    }
}
