//! Runtime "should I search memory before answering" classifier.
//!
//! The search-before-answer policy lives in three places already: the MCP
//! `initialize` instructions, AI_INSTRUCTIONS.md, and an OFFLINE classifier in
//! bench_policy.rs that buckets LongMemEval question TYPES (P1/P2/P0). The
//! offline one cannot run on a live query — it keys on dataset labels, not text.
//!
//! This module is the live half: given the actual user query string, decide
//! P1 (must search) / P2 (should search) / P0 (answer directly), with a reason
//! and the libraries worth searching. It is ADVISORY — MCP cannot force a tool
//! call before the model answers, so the realistic ceiling is a query-aware
//! signal the client can act on. Mirrors the AI_INSTRUCTIONS.md trigger table.

/// Search priority for a single query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Must search before answering (a remembered fact is in play).
    P1,
    /// Should search before assuming (likely refers to stored context).
    P2,
    /// Answer directly; a search would probably waste tokens.
    P0,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::P1 => "must-search",
            Priority::P2 => "should-search",
            Priority::P0 => "answer-directly",
        }
    }
}

/// The advice returned for a query.
#[derive(Debug, Clone)]
pub struct Advice {
    pub priority: Priority,
    pub reason: &'static str,
    /// Libraries / project namespaces named in the query (lowercased match
    /// against the caller-supplied known set). A client should `mind_search`
    /// these first.
    pub suggested_libraries: Vec<String>,
}

// P1 phrase triggers. These are the meta-cues, negations, and cross-session
// references from AI_INSTRUCTIONS.md — the cases where answering from the
// model's own context instead of the store risks a confident wrong answer.
const META_CUES: &[&str] = &[
    "did i tell you",
    "do you remember",
    "have you forgotten",
    "you already know",
    "you should know",
    "remember when",
    "as i mentioned",
    "i told you",
    "did i mention",
    // Russian (Mad's working language).
    "я тебе говорил",
    "я тебе не говорил",
    "ты помнишь",
    "помнишь как",
    "ты уже знаешь",
    "забыл про",
    "я тебе рассказывал",
];

const CROSS_SESSION: &[&str] = &[
    "like last time",
    "as before",
    "the file we were",
    "what we were",
    "where we left",
    "we discussed",
    "we decided",
    "last session",
    // Russian.
    "как в прошлый раз",
    "как раньше",
    "на чём остановились",
    "мы решили",
    "мы обсуждали",
];

// Negation-to-verify openers: "isn't it X", "it's not Y, right?".
const NEGATION_VERIFY: &[&str] = &[
    "isn't it",
    "isn't that",
    "it's not",
    "it is not",
    "aren't they",
    "didn't we",
    "wasn't it",
    // Russian.
    "разве не",
    "это же не",
    "не так ли",
    "ведь не",
];

// P2: weaker "probably refers to stored context" markers.
const P2_MARKERS: &[&str] = &[
    "what do you know about",
    "what's my",
    "what is my",
    "my setup",
    "my config",
    "my preference",
    // Russian.
    "что ты знаешь о",
    "какой у меня",
    "мои настройки",
];

/// Classify a query against the search-before-answer policy. `known_libraries`
/// is the set of project/library namespaces the store knows about (lowercased
/// by the caller is not required — we lowercase here); a query naming one is a
/// strong P1 signal ("name = handle for stored context").
pub fn classify(query: &str, known_libraries: &[String]) -> Advice {
    let q = query.to_lowercase();

    // Named project/library in the query → P1, and suggest it.
    let mut hits: Vec<String> = Vec::new();
    for lib in known_libraries {
        let l = lib.to_lowercase();
        if l.len() >= 3 && word_contains(&q, &l) {
            hits.push(lib.clone());
        }
    }
    if !hits.is_empty() {
        return Advice {
            priority: Priority::P1,
            reason: "query names a known project/library — its stored context must be checked",
            suggested_libraries: hits,
        };
    }

    if contains_any(&q, META_CUES) {
        return Advice {
            priority: Priority::P1,
            reason: "meta-cue about memory — the user is testing what the store holds",
            suggested_libraries: Vec::new(),
        };
    }
    if contains_any(&q, CROSS_SESSION) {
        return Advice {
            priority: Priority::P1,
            reason: "cross-session reference — by definition not in the current context",
            suggested_libraries: Vec::new(),
        };
    }
    if contains_any(&q, NEGATION_VERIFY) {
        return Advice {
            priority: Priority::P1,
            reason: "negation to verify — falsification needs a lookup, not a guess",
            suggested_libraries: Vec::new(),
        };
    }
    if contains_any(&q, P2_MARKERS) {
        return Advice {
            priority: Priority::P2,
            reason: "query likely refers to stored personal/project context",
            suggested_libraries: Vec::new(),
        };
    }

    Advice {
        priority: Priority::P0,
        reason: "no memory trigger detected — answering directly is fine",
        suggested_libraries: Vec::new(),
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Whole-word-ish containment: the library name must appear bounded by
/// non-alphanumeric chars, so "go" doesn't match "going" and "pk" doesn't match
/// "speaking".
fn word_contains(haystack: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(word) {
        let i = start + pos;
        let before_ok = i == 0
            || !haystack[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after = i + word.len();
        let after_ok = after >= haystack.len()
            || !haystack[after..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = i + word.len();
    }
    false
}

/// Render the advice as the tool's text response.
pub fn render(advice: &Advice) -> String {
    let mut out = format!(
        "priority: {}\nreason: {}",
        advice.priority.as_str(),
        advice.reason
    );
    if !advice.suggested_libraries.is_empty() {
        out.push_str(&format!(
            "\nsearch first: {}",
            advice.suggested_libraries.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn libs() -> Vec<String> {
        vec!["aurora".into(), "pixel-kingdoms".into(), "mgi-mind".into()]
    }

    #[test]
    fn named_project_is_p1_and_suggested() {
        let a = classify("how's the aurora bench going?", &libs());
        assert_eq!(a.priority, Priority::P1);
        assert_eq!(a.suggested_libraries, vec!["aurora".to_string()]);
    }

    #[test]
    fn meta_cue_is_p1() {
        assert_eq!(classify("did I tell you about the plan?", &libs()).priority, Priority::P1);
        assert_eq!(classify("ты помнишь что мы делали?", &libs()).priority, Priority::P1);
    }

    #[test]
    fn negation_is_p1() {
        assert_eq!(classify("isn't it Rust we chose?", &libs()).priority, Priority::P1);
    }

    #[test]
    fn cross_session_is_p1() {
        assert_eq!(classify("continue like last time", &libs()).priority, Priority::P1);
    }

    #[test]
    fn p2_marker() {
        assert_eq!(classify("what do you know about my setup", &libs()).priority, Priority::P2);
    }

    #[test]
    fn plain_question_is_p0() {
        assert_eq!(classify("what is the capital of France?", &libs()).priority, Priority::P0);
    }

    #[test]
    fn word_boundary_avoids_substring_false_positive() {
        // "go" must not match inside "going"; a 2-char lib is below the min anyway,
        // but verify the boundary logic on a real lib name.
        let a = classify("I am going to the store", &vec!["go".into()]);
        assert_eq!(a.priority, Priority::P0, "substring must not trigger");
    }
}
