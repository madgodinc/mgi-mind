//! Conservative secret detector — the "secret scrub" companion to auto-write
//! (phase Д2). Auto-ingest (and a careless `mind_add`) would otherwise suck a
//! `.env`, an SSH key, or an API token straight into searchable memory, where it
//! would sit in plaintext and surface in future searches. So every write path
//! scans content here FIRST and refuses anything that looks like a live secret,
//! pointing the user at the terminal-only vault instead.
//!
//! No `regex` dependency (Mad keeps the dep list small): detection is manual
//! prefix/keyword scanning. Tuned for LOW false positives — it must never
//! silently drop legitimate prose, so every rule requires a strong signal (a
//! known token shape, a PEM header, or a secret-named key with a non-placeholder
//! value). The detected secret value is NEVER echoed back (not into the reason,
//! not into logs) — only a short, value-free reason.

/// Why a piece of content was flagged. Deliberately value-free: naming the kind
/// of secret is enough to act on; the secret itself must not leak anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHit {
    pub reason: &'static str,
}

/// Key names that, when assigned a non-trivial value (`KEY = value`, `KEY: value`),
/// indicate a secret. Matched case-insensitively against the key part of a line.
const SECRET_KEY_HINTS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "secret",
    "secret_key",
    "client_secret",
    "password",
    "passwd",
    "pwd",
    "token",
    "auth_token",
    "access_token",
    "refresh_token",
    "private_key",
    "access_key",
    "secret_access_key",
    "bearer",
    "passphrase",
];

/// Obvious placeholders that must NOT count as a real secret value, so example
/// configs and docs don't trip the detector.
const PLACEHOLDERS: &[&str] = &[
    "xxx",
    "xxxx",
    "...",
    "changeme",
    "your_key_here",
    "your-key-here",
    "yourkey",
    "todo",
    "example",
    "placeholder",
    "redacted",
    "none",
    "null",
    "test",
];

/// Scan `text` for anything that looks like a live secret. Returns the first hit
/// (value-free reason) or `None`. Scans the WHOLE text first (PEM blocks span
/// lines) before falling back to per-line and per-token rules.
pub fn scan(text: &str) -> Option<SecretHit> {
    // 1. PEM private key block (spans multiple lines).
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        return Some(SecretHit {
            reason: "PEM private key block",
        });
    }

    // 2. Known token shapes anywhere in the text (whitespace-delimited tokens).
    for raw in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        let tok = raw.trim_matches(|c| c == ',' || c == ';' || c == ')' || c == '(');
        if let Some(reason) = token_shape(tok) {
            return Some(SecretHit { reason });
        }
    }

    // 3. `.env`-style secret assignment: a secret-named key with a real value.
    for line in text.lines() {
        if let Some(reason) = env_assignment(line) {
            return Some(SecretHit { reason });
        }
    }

    None
}

/// Recognize a token by its well-known prefix + shape. Each rule demands enough
/// length/charset that ordinary words can't match.
fn token_shape(tok: &str) -> Option<&'static str> {
    let len = tok.len();

    // AWS access key id: AKIA/ASIA/AGPA + 16 uppercase alphanumerics.
    if len == 20
        && (tok.starts_with("AKIA") || tok.starts_with("ASIA") || tok.starts_with("AGPA"))
        && tok[4..]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Some("AWS access key id");
    }

    // GitHub tokens: ghp_/gho_/ghu_/ghs_/ghr_ + >=36 base62, or github_pat_.
    if (tok.starts_with("ghp_")
        || tok.starts_with("gho_")
        || tok.starts_with("ghu_")
        || tok.starts_with("ghs_")
        || tok.starts_with("ghr_"))
        && len >= 40
        && tok[4..].chars().all(is_token_char)
    {
        return Some("GitHub token");
    }
    if tok.starts_with("github_pat_") && len >= 40 {
        return Some("GitHub fine-grained token");
    }

    // Slack tokens.
    if (tok.starts_with("xoxb-")
        || tok.starts_with("xoxa-")
        || tok.starts_with("xoxp-")
        || tok.starts_with("xoxr-")
        || tok.starts_with("xoxs-"))
        && len >= 20
    {
        return Some("Slack token");
    }

    // GitLab personal access token.
    if tok.starts_with("glpat-") && len >= 26 {
        return Some("GitLab token");
    }

    // OpenAI / Anthropic style keys: sk-... (incl. sk-proj-, sk-ant-).
    if tok.starts_with("sk-") && len >= 20 && tok[3..].chars().all(is_token_char) {
        return Some("API key (sk- prefix)");
    }

    // Google API key: AIza + 35 of [A-Za-z0-9_-].
    if tok.starts_with("AIza") && len == 39 && tok[4..].chars().all(is_token_char) {
        return Some("Google API key");
    }

    // JWT: three base64url segments separated by dots, header starts "eyJ".
    if tok.starts_with("eyJ") {
        let segs: Vec<&str> = tok.split('.').collect();
        if segs.len() == 3
            && segs
                .iter()
                .all(|s| s.len() >= 8 && s.chars().all(is_token_char))
        {
            return Some("JWT");
        }
    }

    None
}

/// `[A-Za-z0-9_-]` — the charset shared by base64url tokens and most API keys.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Detect a `.env`-style `SECRET_KEY = value` / `SECRET_KEY: value` line: a
/// secret-named key assigned a real (non-placeholder, long enough) value.
fn env_assignment(line: &str) -> Option<&'static str> {
    let line = line.trim();
    // Skip comments.
    if line.starts_with('#') || line.starts_with("//") {
        return None;
    }
    // Split on the first '=' or ':'.
    let (key, value) = line.split_once('=').or_else(|| line.split_once(':'))?;

    let key_raw = key.trim().trim_start_matches("export ").trim();
    let key_norm = key_raw.to_lowercase();
    // A real `.env` key looks like an identifier: one token (no spaces) that is
    // either an exact hint, or an identifier-shaped name ending in a hint
    // (DB_PASSWORD, api-key). Plain prose like "password: must be 12 chars" has
    // key "password" — an exact hint, but the value-side checks below (and the
    // fact a sentence rarely assigns a long token-only value) keep false hits
    // low. The guard here is: reject multi-word keys outright (a sentence
    // fragment before ':' is not an env key).
    if key_raw.contains(char::is_whitespace) {
        return None;
    }
    let exact = SECRET_KEY_HINTS.iter().any(|h| key_norm == *h);
    // `ends_with` only for identifier-shaped keys: snake/kebab (DB_PASSWORD,
    // api-key) OR a single concatenated alphanumeric token (MYTOKEN, dbpassword,
    // MYAPIKEY). The whitespace guard above already rejected sentence fragments,
    // so a single bare token ending in a hint is a real env key, not prose.
    let id_shaped = key_raw.contains('_')
        || key_raw.contains('-')
        || key_raw.chars().all(|c| c.is_ascii_alphanumeric());
    let suffix = id_shaped && SECRET_KEY_HINTS.iter().any(|h| key_norm.ends_with(h));
    if !exact && !suffix {
        return None;
    }

    // Strip surrounding quotes / whitespace from the value.
    let value = value.trim().trim_matches('"').trim_matches('\'').trim();
    // Drop a trailing inline comment (` # prod`, ` // note`) — a common `.env`
    // form `KEY=secret # comment`. Only a SPACE-prefixed marker counts, so we
    // don't chop a `#`/`/` that's part of the token itself (e.g. a URL value).
    let value = value
        .split_once(" #")
        .map(|(v, _)| v.trim_end())
        .unwrap_or(value);
    let value = value
        .split_once(" //")
        .map(|(v, _)| v.trim_end())
        .unwrap_or(value);
    if value.len() < 8 {
        return None;
    }
    // A real secret value is a single opaque token. Prose like "must be twelve
    // chars" or "rotated last week" has spaces — it's a sentence, not a secret.
    // (The inline comment was already stripped above, so a real `KEY=tok # note`
    // survives this check.)
    if value.contains(char::is_whitespace) {
        return None;
    }
    let value_lower = value.to_lowercase();
    if PLACEHOLDERS.iter().any(|p| value_lower == *p)
        || value.starts_with('<')
        || value.starts_with("${")
        || value.starts_with("$(")
        || value.chars().all(|c| c == '*' || c == 'x' || c == 'X')
    {
        return None;
    }

    Some("secret-named key with a value")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_pem_private_key() {
        let t = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(scan(t).unwrap().reason, "PEM private key block");
    }

    #[test]
    fn flags_aws_access_key() {
        assert_eq!(
            scan("creds: AKIAIOSFODNN7EXAMPLE here").unwrap().reason,
            "AWS access key id"
        );
    }

    #[test]
    fn flags_github_token() {
        let t = "use ghp_1234567890abcdefghijklmnopqrstuvwxyzAB to auth";
        assert_eq!(scan(t).unwrap().reason, "GitHub token");
    }

    #[test]
    fn flags_openai_style_key() {
        let t = "OPENAI key sk-proj-abcdefghijklmnopqrstuvwxyz0123";
        assert!(scan(t).is_some());
    }

    #[test]
    fn flags_jwt() {
        let t = "token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N";
        assert_eq!(scan(t).unwrap().reason, "JWT");
    }

    #[test]
    fn flags_env_assignment() {
        assert!(scan("API_KEY=slkdjf9823lkjsdf9283").is_some());
        assert!(scan("export DB_PASSWORD = 'hunter2hunter2'").is_some());
        assert!(scan("AWS_SECRET_ACCESS_KEY: wJalrXUtnFEMI0987abcdef").is_some());
    }

    // --- Negative cases: ordinary prose must NEVER trip the detector ---------

    #[test]
    fn ignores_normal_prose() {
        assert!(scan("We decided to use Rust for the new project.").is_none());
        assert!(scan("The staging DB is Postgres 16 on db-staging:5432").is_none());
        assert!(scan("My token of appreciation goes to the whole team").is_none());
    }

    #[test]
    fn ignores_placeholders_and_examples() {
        assert!(scan("API_KEY=your_key_here").is_none());
        assert!(scan("PASSWORD=changeme").is_none());
        assert!(scan("TOKEN=<your-token>").is_none());
        assert!(scan("SECRET=${VAULT_SECRET}").is_none());
        assert!(scan("# API_KEY=realbutcommentedout1234").is_none());
    }

    #[test]
    fn ignores_short_values() {
        // Too short to be a real secret.
        assert!(scan("PASSWORD=1234").is_none());
    }

    #[test]
    fn ignores_prose_with_secret_word_as_key() {
        // Prose where a secret-ish word sits before a colon but the value is a
        // sentence, not a token. These were false-positives that hard-refused
        // legitimate memory writes.
        assert!(scan("password: must be twelve chars minimum and rotated monthly").is_none());
        assert!(scan("token: refresh broke after the 2026 upgrade").is_none());
        assert!(scan("secret: the real secret is that there is no secret").is_none());
        // Multi-word key before the colon is a sentence fragment, not an env key.
        assert!(scan("the password field must accept unicode characters too").is_none());
    }

    #[test]
    fn still_flags_real_env_secrets() {
        // The tightening must not let real token-shaped secrets through.
        assert!(scan("DB_PASSWORD=Xk29slfjLKJ23oiu").is_some());
        assert!(scan("password=Xk29slfjLKJ23oiu").is_some());
    }

    #[test]
    fn flags_env_secret_with_inline_comment() {
        // A real `.env` line often has a trailing comment; the token before it is
        // still a secret and must not slip through the prose whitespace guard.
        assert!(scan("API_KEY=realbutopaquekey123456 # production").is_some());
        assert!(scan("PASSWORD=Sup3rS3cr3tValue99 // do not commit").is_some());
        assert!(scan("DB_PASSWORD=hunter2hunter2  # staging only").is_some());
    }

    #[test]
    fn ignores_prose_with_long_first_word() {
        // No inline-comment marker, so the value stays multi-word and the
        // whitespace guard rejects it even when the first word is long.
        assert!(scan("password: rotateThisMonthly please and rotate again").is_none());
        assert!(scan("secret: authenticateFirstly before touching the cluster").is_none());
    }

    #[test]
    fn flags_concatenated_token_keys() {
        // Single-word env keys (no `_`/`-`) ending in a hint are still real
        // secrets — the suffix match must accept them, not only snake/kebab.
        assert!(scan("MYTOKEN=abcdef1234567890abcd").is_some());
        assert!(scan("userpassword=Xk29slfjLKJ23oiu").is_some());
        assert!(scan("MYAPIKEY=sk-abcdef1234567890abcd").is_some());
    }
}
