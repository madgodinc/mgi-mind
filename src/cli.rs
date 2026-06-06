use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mgimind")]
#[command(about = "MGI-Mind - AI-native second brain")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize MGI-Mind (creates ~/mgimind/)
    Init,

    /// Check system health and auto-fix issues
    Doctor {
        /// Automatically fix found issues
        #[arg(long)]
        fix: bool,
    },

    /// Create a new library
    Create {
        /// Library name
        name: String,
    },

    /// Delete a library
    Drop {
        /// Library name
        name: String,
    },

    /// List all libraries
    List,

    /// Add a memory entry to a library
    Add {
        /// Library name
        library: String,
        /// Content to store
        content: String,
        /// Optional source tag
        #[arg(long)]
        source: Option<String>,
    },

    /// Semantic search across memories
    Search {
        /// Search query
        query: String,
        /// Filter by library
        #[arg(long)]
        library: Option<String>,
        /// Max results (default: 5)
        #[arg(long, default_value = "5")]
        limit: usize,
        /// Retrieval tier: 1=facts, 2=summaries, 3=full
        #[arg(long, default_value = "2")]
        tier: u8,
    },

    /// Delete a specific memory by ID
    Delete {
        /// Library name
        library: String,
        /// Memory ID (from search results)
        id: String,
    },

    /// Generate compact context briefing for AI session start
    Context,

    /// Show recent additions chronologically
    History {
        /// Max entries (default: 10)
        #[arg(long, default_value = "10")]
        limit: usize,
    },

    /// Read a webpage and optionally save to memory
    Web {
        /// URL to read
        url: String,
        /// Save to this library (optional - just prints if omitted)
        #[arg(long)]
        save: Option<String>,
    },

    /// Knowledge graph operations
    Fact {
        #[command(subcommand)]
        action: FactAction,
    },

    /// Session management
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },

    /// Backup all data
    Backup {
        /// Output file path
        output: String,
    },

    /// Restore from backup
    Restore {
        /// Backup file path
        input: String,
    },

    /// Export data
    Export {
        /// Format: json or md
        #[arg(long, default_value = "json")]
        format: String,
        /// Output directory
        #[arg(long)]
        output: Option<String>,
    },

    /// Secure vault for passwords and secrets
    Vault {
        #[command(subcommand)]
        action: VaultAction,
    },

    /// Import from external sources
    Import {
        /// Source: obsidian, markdown
        source: String,
        /// Path to vault/directory
        path: String,
        /// Target library
        #[arg(long, default_value = "imported")]
        library: String,
        /// Actually mutate the store. Without it: dry-run that prints the plan
        /// (what's new / what would replace existing) and exits. md import is
        /// an escape hatch — running it unintentionally over an automated
        /// store is exactly what the dry-run default protects against.
        #[arg(long)]
        apply: bool,
    },

    /// Show memory statistics. Default output is human-readable;
    /// pass --json for machine-parseable output (useful for
    /// monitoring scripts that poll fact counts post-extraction).
    Stats {
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Start bundled Qdrant server
    Serve,

    /// Stop bundled Qdrant server
    Stop,

    /// Run as an MCP server over stdio. One process is the whole server and
    /// stays warm for the session - no daemon, no Unix socket, no Node wrapper.
    Mcp,

    /// v1.4 Phase 5: install, inspect, or uninstall the local LLM auto-extractor
    /// (Qwen 2.5 family GGUF). The extractor populates the knowledge graph
    /// automatically from new memories via background extraction.
    #[cfg(feature = "extractor")]
    Extractor {
        #[command(subcommand)]
        what: ExtractorCmd,
    },

    /// Migrate legacy per-library collections into the single `memories`
    /// collection (audit #18). Idempotent; re-embeds from stored content.
    Migrate {
        /// Delete the old per-library collections after a successful copy
        #[arg(long)]
        purge: bool,
    },

    /// v1.4 Phase 1: prepare the existing memory base for the validity
    /// model. Computes dependant counts per fact, proposes predicate
    /// cardinalities, and backfills confirmation history where derivable.
    /// All operations are idempotent and read-only by default; use --apply
    /// to write the results back into the store.
    MigrateV14 {
        #[command(subcommand)]
        what: MigrateV14Cmd,
    },

    /// v1.5 Phase 6: inspect and edit runtime config (currently:
    /// install-mode profile that selects per-mode confidence-score
    /// anchors per §6 of the validity-model synthesis).
    Config {
        #[command(subcommand)]
        what: ConfigCmd,
    },

    /// v1.6.2: inspect the knowledge-graph facts collection. Stats
    /// tells you how many; this lets you see what.
    Facts {
        #[command(subcommand)]
        what: FactsAction,
    },

    /// v1.5 Phase 7: record a typed external-signal outcome on any
    /// memory. Closes the CLI gap — `mind_outcome` was MCP-only
    /// before. Useful for debugging guardrail / confidence_score
    /// behaviour from a terminal.
    Outcome {
        /// Target memory id.
        memory_id: String,
        /// Signal type: test_passed | code_compiled | user_confirmed | cited_by.
        signal_type: String,
        /// Whether the signal was positive (default true) or negative.
        #[arg(long, default_value_t = true, value_name = "BOOL")]
        success: bool,
        /// Stable source identifier used for idempotency (default
        /// `cli` so the dedup key is well-defined; pass a meaningful
        /// value like `ci.github.com/run/N` or `user-mad` in real use).
        #[arg(long, default_value = "cli")]
        source: String,
    },

    /// Retrieval benchmark (phase Д1): measure R@k retrieval recall on a dataset
    /// (LongMemEval). Zero-API — no LLM, no keys. NOT QA accuracy.
    Bench {
        /// Path to the dataset JSON (e.g. longmemeval_s.json)
        dataset: String,
        /// Dataset format
        #[arg(long, default_value = "longmemeval")]
        format: String,
        /// Run only the first N questions (smoke test; full runs are long on CPU)
        #[arg(long)]
        limit: Option<usize>,
        /// Write raw per-question results to this JSON file
        #[arg(long)]
        output: Option<String>,
    },

    /// Procedural memory benchmark (phase Д6): measure recall@k on a dataset of
    /// (error, fix) pairs. Learns each pair into an isolated bench library,
    /// then recalls by error signature and reports overall + per-stratum R@k.
    /// Zero-API. The dataset is JSONL with fields {error, fix, language, stratum, id?, context?}.
    BenchProcedural {
        /// Path to the dataset JSONL (e.g. procedural-dataset.jsonl)
        dataset: String,
        /// Run only the first N pairs (smoke test)
        #[arg(long)]
        limit: Option<usize>,
        /// Write raw per-pair results to this JSON file
        #[arg(long)]
        output: Option<String>,
    },

    /// Counterfactual A/B for the retrieval policy: take a `mgimind bench`
    /// raw.json output, classify each question by the trigger table (P1
    /// must-search, P2 should-search, P0 no-search), and report ΔR@k with
    /// vs without the search-before-answer policy. Zero-API. Measures
    /// **structural** value of the policy, not LLM generation quality.
    BenchPolicy {
        /// Path to the raw.json produced by `mgimind bench --output raw.json`
        input: String,
    },

    /// STALE bench scaffold (Phase 4): runs the bench-stale harness over
    /// a single configuration. The actual STALE protocol adapter is not
    /// implemented yet — this is the CLI surface so calibration sweep
    /// tooling can be developed against the type contracts.
    BenchStale {
        /// Path to the STALE dataset JSON (T1_T2_400_FULL.json).
        dataset: String,
        /// Answerer (backbone) model. A1: must be gpt-4o-mini for comparability
        /// to the paper's memory-framework column. Reads OPENAI_API_KEY.
        #[arg(long, default_value = "gpt-4o-mini")]
        backbone: String,
        /// Judge model (STALE paper uses a Gemini flash-lite). Reads
        /// GEMINI_API_KEY.
        #[arg(long, default_value = "gemini-flash-latest")]
        judge: String,
        /// Extractor variant for ingest (default / lite). Production path.
        #[arg(long, default_value = "default")]
        extractor: String,
        /// Use the focused user-state extraction prompt (recommended for STALE:
        /// the general prompt loses durable user facts in noisy dialogue).
        #[arg(long)]
        focused: bool,
        /// Dry-run the pipeline with deterministic mock LLMs (no API, no keys).
        /// Used for the D2/D4 gates — verifies ingest→duel→retrieve before
        /// spending a cent.
        #[arg(long)]
        mock: bool,
        /// Route answerer + judge through a file exchange in this dir (a human
        /// or another model writes resp_NNNN.txt for each req_NNNN.txt). Free
        /// dress rehearsal of the full pipeline before paying real judges.
        #[arg(long, value_name = "DIR")]
        file_judge: Option<String>,
        /// Route EXTRACTION through a file exchange in this dir (a human/stronger
        /// model returns triples for each chunk). Isolates duel/retrieve/judge
        /// from the local 2B model's recall ceiling — the upper-bound number.
        #[arg(long, value_name = "DIR")]
        file_extract: Option<String>,
        /// Extract via the OpenAI backbone (gpt-4o-mini) instead of local
        /// Granite — the "with LLM extraction (mechanism)" upper-bound number,
        /// comparable to how mem0/Zep extract. NOT end-to-end product (A2).
        #[arg(long)]
        llm_extract: bool,
        /// Haystack ingest mode: `full` (all 50 sessions, ~190K tok, honest
        /// retrieval-in-noise) or `reduced` (relevant sessions ± window, fast
        /// first-pass — MUST be disclosed as reduced-haystack in the writeup).
        #[arg(long, default_value = "full")]
        haystack: String,
        /// Neighbour window for `--haystack reduced` (sessions each side of the
        /// M_old/M_new sessions).
        #[arg(long, default_value = "2")]
        window: usize,
        /// Override DUEL_FLIP_RATIO for this run (sweep tooling sets
        /// this; default = production constant).
        #[arg(long, value_name = "F")]
        duel_flip_ratio: Option<f32>,
        /// Override DUEL_CONTESTED_RATIO for this run.
        #[arg(long, value_name = "F")]
        duel_contested_ratio: Option<f32>,
        /// Override DOUBT_DRIFT_THRESHOLD for this run.
        #[arg(long, value_name = "F")]
        doubt_drift_threshold: Option<f32>,
        /// Run only the first N scenarios (smoke test).
        #[arg(long)]
        limit: Option<usize>,
        /// Write the result report to this JSON path.
        #[arg(long, default_value = "stale-result.json")]
        output: String,
    },

    /// Deterministic duel-rule validity check (LLM-free). Ingests clean
    /// conflicting triples directly and asserts the duel hides the stale value.
    /// This is the defensible belief-revision number, isolated from extractor
    /// recall.
    DuelValidity,

    /// STALE bench sweep: walk a small grid of constant overrides and
    /// emit per-run results into a directory. Scaffold — wraps the
    /// existing bench-stale single-run harness so calibration tooling
    /// has a CLI surface ready when the harness adapter lands.
    BenchStaleSweep {
        /// Path to the STALE dataset JSON.
        dataset: String,
        /// LLM judge model identifier.
        #[arg(long, default_value = "gpt-4o-mini")]
        judge: String,
        /// Output directory. Each run writes a JSON file inside.
        #[arg(long, default_value = "stale-sweep-out")]
        output_dir: String,
        /// Run only the first N scenarios per configuration.
        #[arg(long)]
        limit: Option<usize>,
    },

    /// Consolidate memory: merge duplicates / near-duplicates and report cold
    /// (old, unused) entries (phase Д2). Dry-run unless --apply.
    Consolidate {
        /// Actually mutate the store (delete merged duplicates). Without it, only reports.
        #[arg(long)]
        apply: bool,
        /// Scope to one library (default: all)
        #[arg(long)]
        library: Option<String>,
        /// Cosine threshold for "near-duplicate" (0..1, default 0.97)
        #[arg(long, default_value = "0.97")]
        near_dup_threshold: f32,
        /// A memory older than this many days with zero accesses is "cold" (default 180)
        #[arg(long, default_value = "180")]
        decay_days: i64,
        /// Also DELETE cold memories (requires --apply; off by default)
        #[arg(long)]
        prune_cold: bool,
    },
    /// Inspect the audit log of mutations (add/update/delete/library/etc).
    /// Read-only — the log itself is append-only and never edited by hand.
    Audit {
        #[command(subcommand)]
        action: AuditAction,
    },
    /// Ephemeral local viewer over the memory store. Brings up an HTTP server
    /// on 127.0.0.1 on a random free port, prints the URL, exits on Ctrl-C.
    /// Static frontend baked into the binary — no Node, no extra runtime.
    Viewer {
        /// Don't auto-open the browser. Useful when running on a headless box
        /// over SSH or when scripting integration tests.
        #[arg(long)]
        no_open: bool,
    },
    /// Auto-extract & ingest memory candidates (phase Д2). Routes filtered
    /// candidates through the v0.11 relevance gate: clearly low-signal
    /// items are sent to the quarantine layer (still retrievable for
    /// re-submission), not dropped. Re-asserting a quarantined item promotes
    /// it to ordinary memory.
    Ingest {
        /// Target library
        #[arg(long, default_value = "default")]
        library: String,
        /// Raw text to run heuristic extraction on (backstop path).
        /// Mutually exclusive with --memory.
        #[arg(long)]
        raw: Option<String>,
        /// One or more memory candidates to ingest (agent-driven path).
        /// Each becomes a `Candidate::Memory`. Repeatable.
        #[arg(long, value_name = "TEXT")]
        memory: Vec<String>,
    },
    /// Inspect and manage the v0.11 quarantine layer. The relevance gate
    /// routes low-signal candidates here instead of dropping them; from this
    /// surface you can see what was filtered, why, and promote entries back
    /// to ordinary memory by id.
    Quarantine {
        #[command(subcommand)]
        action: QuarantineAction,
    },
    /// Ingest a closed Claude Code transcript (`.jsonl` under
    /// `~/.claude/projects/<encoded-cwd>/`) into long-term memory. Pulls
    /// user/assistant text blocks (NOT tool_use / tool_result / thinking),
    /// then routes them through the same relevance gate as live ingest —
    /// short reactions get quarantined, paraphrases of stored content fail
    /// the novelty check, and only the substantive material lands. Zero
    /// LLM: no summarization, no compression. The gate IS the filter.
    IngestSession {
        /// Path to the transcript JSONL file.
        path: String,
        /// Target library (default: `sessions`).
        #[arg(long, default_value = "sessions")]
        library: String,
    },
}

#[derive(Subcommand)]
pub enum MigrateV14Cmd {
    /// For every fact in the knowledge graph, count how many memories in the
    /// store semantically depend on it (cosine ≥ 0.7 against the fact's
    /// (subject, predicate, object) vector). Prints a distribution histogram
    /// (min / p10 / p50 / p90 / max) and, with --apply, writes a
    /// `dependants_count` field to each fact's payload.
    Dependants {
        /// Cosine threshold for "definitely related". 0.7 is conservative.
        #[arg(long, default_value = "0.7")]
        threshold: f32,
        /// Write the counts back to fact payloads. Without this flag the
        /// command is read-only and prints the histogram only.
        #[arg(long)]
        apply: bool,
    },
    /// Inspect every distinct predicate in the knowledge graph and propose a
    /// cardinality (Single / TemporalSingle / Multi) based on observed usage.
    /// Writes proposals to a local JSON file for the user to review before
    /// committing to the cardinality registry.
    Cardinality {
        /// Where to write the proposals JSON. Defaults to
        /// `$MGIMIND_HOME/migration/cardinality-proposals.json`.
        #[arg(long)]
        output: Option<String>,
        /// v1.6.3: bulk-register every High-confidence proposal
        /// from the JSON file via knowledge::register_cardinality.
        /// Skips Low-confidence entries (user reviews those by
        /// hand). The walk still writes the file first, so
        /// --output and --apply compose: walk + write + register.
        #[arg(long)]
        apply: bool,
    },
    /// Backfill `confirmations_count` for memories that have a derivable
    /// confirmation signal (linked to mind_procedure_outcome(worked=true) or
    /// multi-source provenance). Memories without a derivable signal stay at
    /// 0 and accumulate confirmations going forward.
    Confirmations {
        /// Write the counts back. Read-only without this flag.
        #[arg(long)]
        apply: bool,
    },
    /// v1.7 (#111): re-judge every (subject, predicate) pair against the
    /// current cardinality registry. For Single/TemporalSingle predicates
    /// with >1 active facts, runs the duel rule across the cluster and
    /// dampens all losers. Fixes legacy data from before PR #26, where
    /// duel-rule writes silently failed and multiple Active facts coexisted
    /// for the same Single axis.
    RedoDuels {
        /// Write the dampenings back. Read-only without this flag (prints
        /// the cluster plan only).
        #[arg(long)]
        apply: bool,
        /// Optional limit on the number of clusters to inspect. Useful
        /// for sampling on a huge base before the full apply.
        #[arg(long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Print the current install-mode and the auto-detect recommendation.
    /// The recommendation is informational only — it is never auto-applied.
    InstallMode,
    /// Set the install-mode profile. Accepts `chat-only`, `dev-with-ci`,
    /// or `multi-tenant`. Restart `mgimind serve` for the change to take
    /// effect across long-lived MCP sessions.
    SetInstallMode {
        /// New install-mode value.
        mode: String,
    },
}

#[cfg(feature = "extractor")]
#[derive(Subcommand)]
pub enum ExtractorCmd {
    /// Install the llama-server binary + chosen Qwen 2.5 GGUF model.
    /// Idempotent; safe to re-run. Default variant is the 3B model.
    Install {
        /// Variant to download: `lite` (Qwen 1.5B, ~990 MB), `default`
        /// (Qwen 3B, ~1.93 GB). Both download the same llama-server binary.
        #[arg(long, default_value = "default")]
        variant: String,
    },
    /// Show what's installed and whether the server is running.
    Info,
    /// Shut down the running llama-server subprocess (does not remove
    /// the binary or model). Server restarts on the next extraction call.
    Unload,
    /// Remove the llama-server binary and both GGUF variants from disk.
    Uninstall,
    /// Run a one-shot extraction on a piece of text from the command
    /// line. Useful for smoke-testing the install without going through
    /// the auto-ingest pipeline.
    Test {
        text: String,
        #[arg(long, default_value = "default")]
        variant: String,
    },
    /// v1.5 retroactive backfill: walk every memory in `library`,
    /// extract triples, write them into the facts collection. Uses
    /// the same in-process llama-server (one warm load), so 10k+
    /// memories complete in minutes instead of hours. Prints progress
    /// every 100 memories and a final stats line.
    BatchFromLibrary {
        /// Source library name (e.g. `projects`).
        library: String,
        /// Variant to load.
        #[arg(long, default_value = "default")]
        variant: String,
        /// Stop after N memories — handy for staged runs. 0 = all.
        #[arg(long, default_value = "0")]
        limit: usize,
        /// Dry run — extract but do NOT write triples into the facts
        /// collection. Prints the same stats so you can size the run.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum FactsAction {
    /// List facts in the knowledge graph. Optional `--predicate`
    /// filters to one axis; default sort is by dependants_count
    /// descending so load-bearing facts surface first.
    List {
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Only show facts whose predicate matches this value.
        #[arg(long, value_name = "P")]
        predicate: Option<String>,
        /// Sort: `dependants` (default) or `created`.
        #[arg(long, default_value = "dependants")]
        sort: String,
        /// Print fact ids alongside subject/predicate/object. Off
        /// by default because UUIDs are wide; turn on when you
        /// plan to feed an id into `mgimind facts show`.
        #[arg(long, default_value_t = false)]
        with_id: bool,
    },
    /// Show one fact by id, including payload (dependants_count,
    /// confidence_score, doubt counter, …).
    Show { id: String },
}

#[derive(Subcommand)]
pub enum AuditAction {
    /// Show the N most recent audit events (default 20). Optional
    /// filters trim the list before display.
    List {
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Only events whose `op` matches the given variant
        /// (snake_case). Examples: `retest_promote`,
        /// `retest_recover`, `fact_add`. See AuditOp in source.
        #[arg(long, value_name = "OP")]
        op: Option<String>,
        /// Only events newer than this many hours ago. 24 = last
        /// day, 168 = last week. Omit for "all time".
        #[arg(long, value_name = "HOURS")]
        since_hours: Option<i64>,
    },
    /// Show audit events whose `target` matches the given id (memory id,
    /// library name, etc).
    Show { id: String },
}

#[derive(Subcommand)]
pub enum QuarantineAction {
    /// List quarantined entries, newest first. Scope to one library with
    /// `--library`, otherwise lists across all libraries.
    List {
        #[arg(long)]
        library: Option<String>,
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Show a single quarantined entry by id with its full content and
    /// the gate reason that filtered it.
    Show { id: String },
    /// Manually promote a quarantined entry to ordinary memory by id. The
    /// usual promotion path is automatic (re-asserting the same content via
    /// ingest); this is the explicit override when you know what you want.
    Promote { id: String },
}

#[derive(Subcommand)]
pub enum FactAction {
    /// Add a fact: subject -> predicate -> object
    Add {
        subject: String,
        predicate: String,
        object: String,
    },
    /// Query facts about a subject
    Query { subject: String },
    /// Invalidate a fact
    Invalidate {
        /// Fact ID
        id: String,
    },
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// Start a new session
    Start {
        /// Agent name (e.g., claude-code, cursor)
        #[arg(long, default_value = "unknown")]
        agent: String,
    },
    /// Show last session summary (optionally scoped to an agent)
    Last {
        /// Only consider this agent's sessions
        #[arg(long)]
        agent: Option<String>,
    },
    /// End the active session for an agent with a summary
    End {
        /// Agent name whose session to end
        #[arg(long, default_value = "unknown")]
        agent: String,
        /// Session summary
        #[arg(long)]
        summary: String,
    },
}

#[derive(Subcommand)]
pub enum VaultAction {
    /// Store a secret (password, API key, SSH credentials)
    Store {
        /// Unique key name (e.g., "ssh-server-1", "github-token")
        key: String,
        /// Secret value
        value: String,
        /// Category: password, ssh, api-key, token, other
        #[arg(long, default_value = "other")]
        category: String,
        /// Description of what this secret is for
        #[arg(long, default_value = "")]
        desc: String,
    },
    /// Retrieve a secret (REQUIRES user confirmation)
    Get {
        /// Key name
        key: String,
        /// Skip confirmation (use with caution)
        #[arg(long)]
        yes: bool,
    },
    /// List all stored keys (values are hidden)
    List,
    /// Delete a secret
    Delete {
        /// Key name
        key: String,
    },
}

pub async fn run(cli: Cli) -> Result<()> {
    // Audit log lives under mind_home so it follows MGIMIND_HOME isolation
    // automatically (tests + bench instances each get their own log without
    // polluting prod).
    crate::audit::init(Some(crate::config::mind_home().join("audit.log")));

    match cli.command {
        Commands::Init => cmd_init().await,
        Commands::Doctor { fix } => cmd_doctor(fix).await,
        Commands::Create { name } => cmd_create(&name).await,
        Commands::Drop { name } => cmd_drop(&name).await,
        Commands::List => cmd_list().await,
        Commands::Add {
            library,
            content,
            source,
        } => cmd_add(&library, &content, source.as_deref()).await,
        Commands::Search {
            query,
            library,
            limit,
            tier,
        } => cmd_search(&query, library.as_deref(), limit, tier).await,
        Commands::Delete { library, id } => cmd_delete(&library, &id).await,
        Commands::Context => cmd_context().await,
        Commands::History { limit } => cmd_history(limit).await,
        Commands::Web { url, save } => cmd_web(&url, save.as_deref()).await,
        Commands::Fact { action } => match action {
            FactAction::Add {
                subject,
                predicate,
                object,
            } => cmd_fact_add(&subject, &predicate, &object).await,
            FactAction::Query { subject } => cmd_fact_query(&subject).await,
            FactAction::Invalidate { id } => cmd_fact_invalidate(&id).await,
        },
        Commands::Session { action } => match action {
            SessionAction::Start { agent } => cmd_session_start(&agent).await,
            SessionAction::Last { agent } => cmd_session_last(agent.as_deref()).await,
            SessionAction::End { agent, summary } => cmd_session_end(&agent, &summary).await,
        },
        Commands::Vault { action } => match action {
            VaultAction::Store {
                key,
                value,
                category,
                desc,
            } => cmd_vault_store(&key, &value, &category, &desc).await,
            VaultAction::Get { key, yes } => cmd_vault_get(&key, yes).await,
            VaultAction::List => cmd_vault_list().await,
            VaultAction::Delete { key } => cmd_vault_delete(&key).await,
        },
        Commands::Import {
            source,
            path,
            library,
            apply,
        } => cmd_import(&source, &path, &library, apply).await,
        Commands::Stats { json } => cmd_stats(json).await,
        Commands::Backup { output } => cmd_backup(&output).await,
        Commands::Restore { input } => cmd_restore(&input).await,
        Commands::Export { format, output } => cmd_export(&format, output.as_deref()).await,
        Commands::Serve => cmd_serve().await,
        Commands::Stop => cmd_stop().await,
        Commands::Mcp => crate::mcp::serve().await,
        #[cfg(feature = "extractor")]
        Commands::Extractor { what } => cmd_extractor(what).await,
        Commands::Migrate { purge } => cmd_migrate(purge).await,
        Commands::Config { what } => cmd_config(what).await,
        Commands::Outcome {
            memory_id,
            signal_type,
            success,
            source,
        } => cmd_outcome(&memory_id, &signal_type, success, &source).await,
        Commands::Facts { what } => match what {
            FactsAction::List {
                limit,
                predicate,
                sort,
                with_id,
            } => cmd_facts_list(limit, predicate.as_deref(), &sort, with_id).await,
            FactsAction::Show { id } => cmd_facts_show(&id).await,
        },
        Commands::MigrateV14 { what } => match what {
            MigrateV14Cmd::Dependants { threshold, apply } => {
                cmd_migrate_v14_dependants(threshold, apply).await
            }
            MigrateV14Cmd::Cardinality { output, apply } => {
                cmd_migrate_v14_cardinality(output.as_deref(), apply).await
            }
            MigrateV14Cmd::Confirmations { apply } => cmd_migrate_v14_confirmations(apply).await,
            MigrateV14Cmd::RedoDuels { apply, limit } => {
                cmd_migrate_v14_redo_duels(apply, limit).await
            }
        },
        Commands::Bench {
            dataset,
            format,
            limit,
            output,
        } => cmd_bench(&dataset, &format, limit, output.as_deref()).await,
        Commands::BenchProcedural {
            dataset,
            limit,
            output,
        } => cmd_bench_procedural(&dataset, limit, output.as_deref()).await,
        Commands::BenchPolicy { input } => cmd_bench_policy(&input).await,
        Commands::BenchStale {
            dataset,
            backbone,
            judge,
            extractor,
            focused,
            mock,
            file_judge,
            file_extract,
            llm_extract,
            haystack,
            window,
            duel_flip_ratio,
            duel_contested_ratio,
            doubt_drift_threshold,
            limit,
            output,
        } => {
            #[cfg(not(feature = "extractor"))]
            {
                let _ = (
                    &dataset, &backbone, &judge, &extractor, focused, mock, &file_judge,
                    &file_extract, llm_extract, &haystack, window, duel_flip_ratio,
                    duel_contested_ratio, doubt_drift_threshold, limit, &output,
                );
                anyhow::bail!(
                    "bench-stale needs the production extractor; rebuild with \
                     `--features extractor`"
                );
            }
            #[cfg(feature = "extractor")]
            {
            use crate::qa_judge::{FileClient, GeminiClient, LlmClient, MockClient, OpenAiClient};

            let overrides = crate::bench_stale::CalibrationOverrides {
                duel_flip_ratio,
                duel_contested_ratio,
                doubt_drift_threshold,
                ..Default::default()
            };

            let extract_cfg = crate::extractor::ExtractConfig {
                variant: crate::extractor::ExtractorVariant::parse(&extractor)
                    .unwrap_or_default(),
                focused,
                ..Default::default()
            };

            let mode = match haystack.trim().to_ascii_lowercase().as_str() {
                "reduced" => crate::bench_stale::HaystackMode::Reduced { window },
                "full" | "" => crate::bench_stale::HaystackMode::Full,
                other => anyhow::bail!("unknown --haystack mode '{other}' (full|reduced)"),
            };

            // Three modes: file_judge (human/model plays LLM via files — free
            // rehearsal), mock (deterministic, gate only), or real API.
            let (answerer, judge_client): (Box<dyn LlmClient>, Box<dyn LlmClient>) =
                if let Some(dir) = &file_judge {
                    // Shared dir + shared counter so answerer and judge requests
                    // interleave into one ordered req_/resp_ stream.
                    let answerer = FileClient::new("file-answerer", dir)?;
                    let counter = answerer.counter.clone();
                    let mut judge = FileClient::new("file-judge", dir)?;
                    judge.counter = counter;
                    (Box::new(answerer), Box::new(judge))
                } else if mock {
                    let mk = || {
                        Box::new(MockClient {
                            id: "mock".into(),
                            rules: vec![],
                            default: "{\"dim1_eval\":{\"reasoning\":\"mock\",\"pass\":false},\
                                       \"dim2_eval\":{\"reasoning\":\"mock\",\"pass\":false},\
                                       \"dim3_eval\":{\"reasoning\":\"mock\",\"pass\":false}}"
                                .into(),
                        })
                    };
                    (mk(), mk())
                } else {
                    (
                        Box::new(OpenAiClient::from_env(&backbone)?),
                        Box::new(GeminiClient::from_env(&judge)?.json_mode(true)),
                    )
                };

            // LLM-extractor: a separate OpenAI client (gpt-4o-mini) used for
            // extraction — the oracle/mechanism upper-bound path.
            let llm_extractor: Option<Box<dyn LlmClient>> = if llm_extract {
                Some(Box::new(OpenAiClient::from_env("gpt-4o-mini")?))
            } else {
                None
            };

            let report = crate::bench_stale::run(
                std::path::PathBuf::from(&dataset),
                answerer.as_ref(),
                judge_client.as_ref(),
                &extract_cfg,
                mode,
                limit,
                overrides,
                std::path::PathBuf::from(&output),
                file_extract.map(std::path::PathBuf::from),
                llm_extractor.as_deref(),
            )
            .await?;
            println!("STALE run done. Total scenarios: {}", report.scenarios_run);
            println!("Overall: {:.1}%", report.overall_pct);
            println!(
                "State-resolution rate: {:.1}%",
                report.by_metric.state_resolution_pct
            );
            println!(
                "Premise-resistance rate: {:.1}%",
                report.by_metric.premise_resistance_pct
            );
            Ok(())
            }
        }
        Commands::DuelValidity => {
            #[cfg(not(feature = "extractor"))]
            {
                anyhow::bail!("duel-validity is built behind the bench module; rebuild with `--features extractor`");
            }
            #[cfg(feature = "extractor")]
            {
                let config = crate::config::MindConfig::load()?;
                let (passed, total) = crate::bench_stale::run_duel_validity(&config).await?;
                println!("Duel-rule validity: {passed}/{total}");
                if passed != total {
                    std::process::exit(1);
                }
                Ok(())
            }
        }
        Commands::BenchStaleSweep {
            dataset,
            judge,
            output_dir,
            limit,
        } => cmd_bench_stale_sweep(&dataset, &judge, &output_dir, limit).await,
        Commands::Consolidate {
            apply,
            library,
            near_dup_threshold,
            decay_days,
            prune_cold,
        } => {
            cmd_consolidate(crate::consolidate::Options {
                apply,
                library,
                near_dup_threshold,
                decay_days,
                prune_cold,
            })
            .await
        }
        Commands::Audit { action } => match action {
            AuditAction::List {
                limit,
                op,
                since_hours,
            } => cmd_audit_list(limit, op.as_deref(), since_hours).await,
            AuditAction::Show { id } => cmd_audit_show(&id).await,
        },
        Commands::Viewer { no_open } => {
            let config = crate::config::MindConfig::load()
                .context("Failed to load config — run `mgimind init` first")?;
            crate::viewer::run(config, !no_open).await
        }
        Commands::Ingest {
            library,
            raw,
            memory,
        } => cmd_ingest(&library, raw.as_deref(), memory).await,
        Commands::Quarantine { action } => match action {
            QuarantineAction::List { library, limit } => {
                cmd_quarantine_list(library.as_deref(), limit).await
            }
            QuarantineAction::Show { id } => cmd_quarantine_show(&id).await,
            QuarantineAction::Promote { id } => cmd_quarantine_promote(&id).await,
        },
        Commands::IngestSession { path, library } => cmd_ingest_session(&path, &library).await,
    }
}

async fn cmd_ingest_session(path: &str, library: &str) -> Result<()> {
    let config = crate::config::MindConfig::load()
        .context("Failed to load config — run `mgimind init` first")?;
    // Ensure target library exists (idempotent).
    let _ = crate::storage::create_library(&config, library).await;
    let report =
        crate::session_ingest::ingest_transcript(&config, std::path::Path::new(path), library)
            .await?;
    print!("{}", report.render());
    Ok(())
}

async fn cmd_quarantine_list(library: Option<&str>, limit: usize) -> Result<()> {
    let config = crate::config::load_cached()?;
    let entries = crate::storage::quarantine_list(&config, library, limit).await?;
    print!("{}", render_quarantine_list(&entries));
    Ok(())
}

async fn cmd_quarantine_show(id: &str) -> Result<()> {
    let config = crate::config::load_cached()?;
    match crate::storage::quarantine_get(&config, id).await? {
        Some(e) => print!("{}", render_quarantine_entry(&e)),
        None => println!(
            "No quarantined entry with id '{id}' (it may be a regular memory or unknown id)."
        ),
    }
    Ok(())
}

async fn cmd_quarantine_promote(id: &str) -> Result<()> {
    let config = crate::config::load_cached()?;
    if crate::storage::promote_from_quarantine(&config, id).await? {
        println!("Promoted '{id}' from quarantine to ordinary memory.");
    } else {
        println!("Nothing to promote — '{id}' is not in quarantine.");
    }
    Ok(())
}

pub(crate) fn render_quarantine_list(entries: &[crate::storage::QuarantineEntry]) -> String {
    use std::fmt::Write;
    if entries.is_empty() {
        return "No quarantined entries.\n".to_string();
    }
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        let _ = writeln!(
            out,
            "{}. [{}] id: {}  reason: {}",
            i + 1,
            e.library,
            e.id,
            e.reason
        );
        let _ = writeln!(out, "   {}", e.content);
        if let Some(src) = &e.source {
            let _ = writeln!(out, "   source: {src}");
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "{} quarantined entry/entries.", entries.len());
    out
}

pub(crate) fn render_quarantine_entry(e: &crate::storage::QuarantineEntry) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "id:       {}", e.id);
    let _ = writeln!(out, "library:  {}", e.library);
    let _ = writeln!(out, "reason:   {}", e.reason);
    if let Some(src) = &e.source {
        let _ = writeln!(out, "source:   {src}");
    }
    if let Some(ts) = &e.created_at {
        let _ = writeln!(out, "created:  {ts}");
    }
    let _ = writeln!(out, "content:");
    let _ = writeln!(out, "{}", e.content);
    out
}

async fn cmd_ingest(library: &str, raw: Option<&str>, memory: Vec<String>) -> Result<()> {
    let config = crate::config::MindConfig::load()
        .context("Failed to load config — run `mgimind init` first")?;
    // Ensure target library exists (idempotent).
    let _ = crate::storage::create_library(&config, library).await;
    let candidates: Vec<crate::ingest::Candidate> = memory
        .into_iter()
        .map(|content| crate::ingest::Candidate::Memory { content })
        .collect();
    let report = crate::ingest::run_ingest(&config, raw, candidates, library).await?;
    println!("{}", report.render());
    Ok(())
}

async fn cmd_audit_list(
    limit: usize,
    op_filter: Option<&str>,
    since_hours: Option<i64>,
) -> Result<()> {
    // When filters are present we have to load all, filter, then
    // truncate — `recent(limit)` would skip relevant events outside
    // the first N. For "no filters" path we keep the fast `recent`
    // call so a typical `mgimind audit list` stays cheap.
    let events = if op_filter.is_some() || since_hours.is_some() {
        let cutoff = since_hours.map(|h| chrono::Utc::now() - chrono::Duration::hours(h));
        let all = crate::audit::load_all()?;
        let filtered: Vec<crate::audit::AuditEvent> = all
            .into_iter()
            .filter(|ev| match op_filter {
                None => true,
                Some(want) => serde_json::to_string(&ev.op)
                    .map(|s| s.trim_matches('"') == want)
                    .unwrap_or(false),
            })
            .filter(|ev| match cutoff {
                None => true,
                Some(c) => chrono::DateTime::parse_from_rfc3339(&ev.ts)
                    .map(|dt| dt.with_timezone(&chrono::Utc) >= c)
                    .unwrap_or(false),
            })
            .collect();
        // Take the most recent `limit` from the tail (load_all is
        // append-order so the tail is newest).
        let take_from = filtered.len().saturating_sub(limit);
        filtered[take_from..].to_vec()
    } else {
        crate::audit::recent(limit)?
    };

    if events.is_empty() {
        println!("No audit events matching filters.");
        return Ok(());
    }
    for ev in &events {
        print_audit_event(ev);
    }
    println!("\n{} event(s).", events.len());
    Ok(())
}

async fn cmd_audit_show(id: &str) -> Result<()> {
    let events = crate::audit::for_target(id)?;
    if events.is_empty() {
        println!("No audit events for target '{id}'.");
        return Ok(());
    }
    for ev in &events {
        print_audit_event(ev);
    }
    println!("\n{} event(s) for '{id}'.", events.len());
    Ok(())
}

fn print_audit_event(ev: &crate::audit::AuditEvent) {
    let op = serde_json::to_string(&ev.op)
        .unwrap_or_else(|_| "?".into())
        .trim_matches('"')
        .to_string();
    let lib = if ev.library.is_empty() {
        "-".to_string()
    } else {
        ev.library.clone()
    };
    let tgt = if ev.target.is_empty() {
        "-".to_string()
    } else {
        ev.target.clone()
    };
    println!(
        "[{}] {:<18} lib={} target={} actor={}",
        ev.ts, op, lib, tgt, ev.actor
    );
    if let Some(before) = &ev.before {
        println!("  before: {}", first_line(before));
    }
    if let Some(after) = &ev.after {
        println!("  after:  {}", first_line(after));
    }
    if let Some(note) = &ev.note {
        println!("  note:   {note}");
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

async fn cmd_bench(
    dataset: &str,
    format: &str,
    limit: Option<usize>,
    output: Option<&str>,
) -> Result<()> {
    let config = crate::config::load_cached()?;
    let report = match format {
        "longmemeval" => crate::bench::run_longmemeval(&config, dataset, limit, output).await?,
        other => anyhow::bail!("unknown bench format '{other}' (supported: longmemeval)"),
    };
    println!("{report}");
    Ok(())
}

async fn cmd_bench_procedural(
    dataset: &str,
    limit: Option<usize>,
    output: Option<&str>,
) -> Result<()> {
    let config = crate::config::load_cached()?;
    let report = crate::bench_procedural::run(&config, dataset, limit, output).await?;
    println!("{report}");
    Ok(())
}

async fn cmd_bench_policy(input: &str) -> Result<()> {
    let report = crate::bench_policy::run(std::path::Path::new(input))?;
    println!("{report}");
    Ok(())
}

async fn cmd_bench_stale_sweep(
    dataset: &str,
    judge: &str,
    output_dir: &str,
    limit: Option<usize>,
) -> Result<()> {
    #[cfg(not(feature = "extractor"))]
    {
        let _ = (dataset, judge, output_dir, limit);
        anyhow::bail!(
            "bench-stale-sweep needs the production extractor; rebuild with \
             `--features extractor`"
        );
    }
    #[cfg(feature = "extractor")]
    {
    use crate::bench_stale::CalibrationOverrides;
    use crate::qa_judge::{GeminiClient, LlmClient, OpenAiClient};

    std::fs::create_dir_all(output_dir)?;
    eprintln!("STALE sweep: writing per-run reports into {output_dir}");
    eprintln!("             judge model: {judge}");
    eprintln!("             dataset:     {dataset}");

    // The sweep tunes duel constants — it needs real LLMs (mock would make
    // every cell identical). Build answerer (gpt-4o-mini, A1) + judge once.
    let answerer: Box<dyn LlmClient> = Box::new(OpenAiClient::from_env("gpt-4o-mini")?);
    let judge_client: Box<dyn LlmClient> = Box::new(GeminiClient::from_env(judge)?.json_mode(true));
    let extract_cfg = crate::extractor::ExtractConfig::default();

    // v1.6.3 sweep grid: ±25% / ±50% around the production defaults
    // for the three thresholds Phase 4 calibration cares about most.
    // 3 thresholds × 3 multipliers + baseline = 10 runs. Future
    // expansion adds dependants weighting + age decay sweeps.
    let multipliers: &[(&str, f32)] = &[
        ("baseline", 1.00),
        ("flip-low", 0.50),
        ("flip-high", 1.50),
        ("contested-low", 0.50),
        ("contested-high", 1.50),
        ("doubt-low", 0.50),
        ("doubt-high", 1.50),
    ];

    let mut summary = serde_json::json!({
        "judge": judge,
        "dataset": dataset,
        "runs": [],
    });

    for &(name, mult) in multipliers {
        let mut overrides = CalibrationOverrides::default();
        if name.starts_with("flip-") {
            overrides.duel_flip_ratio = Some(1.5 * mult);
        } else if name.starts_with("contested-") {
            overrides.duel_contested_ratio = Some(0.5 * mult);
        } else if name.starts_with("doubt-") {
            overrides.doubt_drift_threshold = Some(0.4 * mult);
        }

        let out_path = std::path::Path::new(output_dir).join(format!("stale-{name}.json"));
        eprintln!(
            "STALE sweep: running '{name}' (overrides: {}) → {}",
            overrides.tag(),
            out_path.display()
        );

        let report = crate::bench_stale::run(
            std::path::PathBuf::from(dataset),
            answerer.as_ref(),
            judge_client.as_ref(),
            &extract_cfg,
            crate::bench_stale::HaystackMode::Full,
            limit,
            overrides,
            out_path.clone(),
            None,
            None,
        )
        .await?;

        let entry = serde_json::json!({
            "name": name,
            "output": out_path.display().to_string(),
            "scenarios": report.scenarios_run,
            "overall_pct": report.overall_pct,
            "state_resolution_pct": report.by_metric.state_resolution_pct,
            "premise_resistance_pct": report.by_metric.premise_resistance_pct,
        });
        if let Some(arr) = summary["runs"].as_array_mut() {
            arr.push(entry);
        }
    }

    let summary_path = std::path::Path::new(output_dir).join("summary.json");
    std::fs::write(&summary_path, serde_json::to_string_pretty(&summary)?)?;
    println!(
        "STALE sweep done. {} runs. Summary: {}",
        multipliers.len(),
        summary_path.display()
    );
    Ok(())
    }
}

async fn cmd_consolidate(opts: crate::consolidate::Options) -> Result<()> {
    let config = crate::config::load_cached()?;
    let apply = opts.apply;
    let prune_cold = opts.prune_cold;
    if !apply {
        println!("Consolidation DRY-RUN (no changes). Re-run with --apply to act.\n");
    }
    let r = crate::consolidate::run(&config, opts).await?;
    println!("Scanned:              {}", r.scanned);
    println!("Exact duplicates:     {}", r.exact_dups_removed);
    println!("Near-duplicates:      {}", r.near_dups_removed);
    println!("Cold (old + unused):  {}", r.cold_candidates);
    if apply {
        let removed = r.exact_dups_removed + r.near_dups_removed + r.cold_pruned;
        println!("\nApplied: removed {removed} memories.");
        if r.cold_candidates > 0 && !prune_cold {
            println!(
                "Kept {} cold entries (pass --prune-cold to delete them too).",
                r.cold_candidates
            );
        }
    } else if r.exact_dups_removed + r.near_dups_removed > 0 {
        println!(
            "\nWould remove {} duplicate(s) with --apply.",
            r.exact_dups_removed + r.near_dups_removed
        );
    }
    Ok(())
}

async fn cmd_migrate(purge: bool) -> Result<()> {
    let config = crate::config::load_cached()?;
    println!(
        "Migrating legacy per-library collections into '{}'...",
        crate::storage::MEMORIES_COLLECTION
    );
    let (moved, skipped, libs) = crate::storage::migrate(&config, purge).await?;
    if libs.is_empty() {
        println!("No legacy collections found - nothing to migrate.");
    } else {
        println!(
            "Migrated {moved} entries from libraries: {}",
            libs.join(", ")
        );
        if skipped > 0 {
            println!("Skipped {skipped} entries that failed (see warnings above).");
        }
        if purge {
            println!("Old per-library collections were purged.");
        } else {
            println!("Old collections kept. Re-run with --purge to delete them once verified.");
        }
    }
    Ok(())
}

// ===== v1.4 Phase 1 migrations =====

async fn cmd_migrate_v14_dependants(threshold: f32, apply: bool) -> Result<()> {
    let config = crate::config::load_cached()?;
    println!(
        "v1.4 Phase 1.1 — counting dependants per fact (cosine threshold = {threshold}{}).",
        if apply {
            ", writing back to payloads"
        } else {
            ", read-only"
        }
    );
    let (counts, summary) = crate::migrate_v14::run_dependants(&config, threshold, apply).await?;
    println!("\n{}", summary.render("dependants per fact"));
    println!(
        "formula-shape recommendation: {}",
        summary.recommended_formula_shape()
    );
    if !apply && !counts.is_empty() {
        println!("\nRun again with --apply to write the counts back into fact payloads.");
    }
    if !apply && counts.is_empty() {
        println!("\n(walk implementation still landing in step 1.1 commit 2; CLI scaffold ready.)");
    }
    Ok(())
}

async fn cmd_migrate_v14_cardinality(output: Option<&str>, apply: bool) -> Result<()> {
    let config = crate::config::load_cached()?;
    let default_path = crate::config::mind_home()
        .join("migration")
        .join("cardinality-proposals.json");
    let output_path = output.map(std::path::PathBuf::from).unwrap_or(default_path);
    println!(
        "v1.4 Phase 1.2 — inferring predicate cardinalities → {}",
        output_path.display()
    );
    let n = crate::migrate_v14::run_cardinality_inference(&config, output_path.clone()).await?;
    if n == 0 {
        println!("(walk implementation still landing in step 1.2 commit; CLI scaffold ready.)");
        return Ok(());
    }
    println!("Wrote {n} proposals to {}.", output_path.display(),);
    if !apply {
        println!(
            "Review the JSON, then re-run with --apply to bulk-register every High-confidence proposal, or commit each by hand via `mgimind mcp` tool `mind_predicate(action=\"register\")`."
        );
        return Ok(());
    }

    // --apply: parse the JSON we just wrote, register each
    // High-confidence proposal via knowledge::register_cardinality.
    // Low-confidence entries are skipped — those need user review.
    let raw = std::fs::read_to_string(&output_path).with_context(|| {
        format!(
            "failed to re-read proposals JSON at {}",
            output_path.display()
        )
    })?;
    let proposals: serde_json::Value = serde_json::from_str(&raw)?;
    let Some(obj) = proposals.as_object() else {
        anyhow::bail!("proposals JSON is not an object");
    };

    let mut registered = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;
    for (predicate, entry) in obj {
        let confidence = entry
            .get("confidence")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if confidence != "High" {
            skipped += 1;
            continue;
        }
        let proposed = entry.get("proposed").and_then(|v| v.as_str()).unwrap_or("");
        let cardinality = match crate::knowledge::Cardinality::parse(proposed) {
            Some(c) => c,
            None => {
                eprintln!("skipped '{predicate}': unknown cardinality '{proposed}'");
                errors += 1;
                continue;
            }
        };
        match crate::knowledge::register_cardinality(&config, predicate, cardinality).await {
            Ok(_) => registered += 1,
            Err(e) => {
                eprintln!("failed to register '{predicate}': {e}");
                errors += 1;
            }
        }
    }
    println!(
        "Registered {registered} High-confidence proposals; skipped {skipped} Low-confidence; {errors} errors."
    );
    Ok(())
}

async fn cmd_migrate_v14_redo_duels(apply: bool, limit: Option<usize>) -> Result<()> {
    use crate::knowledge::{Cardinality, list_all_facts, list_cardinalities};
    use std::collections::HashMap;

    let config = crate::config::load_cached()?;

    println!(
        "v1.7 #111 — re-judging legacy facts against current cardinality registry{}.",
        if apply { ", writing back" } else { ", dry-run" }
    );

    // Build a (predicate → cardinality) map. Predicates not in the registry
    // default to Multi (the safe choice — no conflict possible), so we don't
    // touch them.
    let cards = list_cardinalities(&config).await?;
    let mut card_map: HashMap<String, Cardinality> = HashMap::new();
    for (pred, c) in cards {
        card_map.insert(pred, c);
    }

    // Walk every Active fact and bucket by (subject, predicate). list_all_facts
    // already excludes status=stale post-#26, so a cluster of size > 1 here is
    // exactly the "legacy bug residue" we're cleaning up.
    let facts = list_all_facts(&config).await?;
    let mut clusters: HashMap<(String, String), Vec<crate::knowledge::Fact>> = HashMap::new();
    for f in facts {
        clusters
            .entry((f.subject.clone(), f.predicate.clone()))
            .or_default()
            .push(f);
    }

    // Keep only conflict-bearing clusters: registered Single/TemporalSingle
    // AND size > 1. Multi predicates never duel — coexistence is the contract.
    //
    // For Single: the loser is dampened (status=stale) — it lost a duel.
    // For TemporalSingle: older entries are marked superseded (status=superseded)
    // — they were canonical at their time but are no longer current. The two
    // statuses differ in how mind_history / explanation tools surface them
    // (see EntryStatus docs and the §6 model invariants).
    let mut work: Vec<((String, String), Vec<crate::knowledge::Fact>, Cardinality)> = clusters
        .into_iter()
        .filter_map(|((s, p), group)| {
            if group.len() < 2 {
                return None;
            }
            let card = *card_map.get(&p)?;
            if !card.admits_conflict() {
                return None;
            }
            Some(((s, p), group, card))
        })
        .collect();

    // Stable order: largest clusters first, then alpha. Helps the user see the
    // worst offenders at the top of the dry-run.
    work.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| a.0.0.cmp(&b.0.0))
            .then_with(|| a.0.1.cmp(&b.0.1))
    });

    let total_clusters = work.len();
    if let Some(n) = limit {
        work.truncate(n);
    }

    if total_clusters == 0 {
        println!("\nNo conflict-bearing clusters found. Knowledge graph is already canonical.");
        return Ok(());
    }

    println!(
        "\nFound {total_clusters} conflict-bearing cluster(s){}:",
        match limit {
            Some(n) if n < total_clusters => format!(" (showing first {n})"),
            _ => String::new(),
        }
    );

    let mut total_dampened = 0usize;
    let mut total_kept = 0usize;
    for ((subject, predicate), group, card) in &work {
        // Winner policy: keep the most recently added (highest created_at);
        // dampen the rest. This matches the temporal-most-recent heuristic
        // a TemporalSingle predicate already implies, and is also reasonable
        // for Single — a user who re-asserts is updating their belief, not
        // forking it.
        let mut sorted: Vec<crate::knowledge::Fact> = (*group).clone();
        sorted.sort_by(|a, b| {
            let av = a.created_at.as_deref().unwrap_or("");
            let bv = b.created_at.as_deref().unwrap_or("");
            bv.cmp(av) // newest first
        });
        let winner = &sorted[0];
        let losers = &sorted[1..];
        total_kept += 1;
        total_dampened += losers.len();

        println!(
            "\n  [{card:?}] {subject:?} -> {predicate:?} ({} active)",
            group.len()
        );
        let verb = match card {
            Cardinality::Single => "dampen",
            Cardinality::TemporalSingle => "supersede",
            Cardinality::Multi => unreachable!("Multi already filtered out above"),
        };
        println!(
            "    keep      {} (created {})",
            winner.object,
            winner.created_at.as_deref().unwrap_or("?")
        );
        for l in losers {
            println!(
                "    {verb:9} {} (created {})",
                l.object,
                l.created_at.as_deref().unwrap_or("?")
            );
        }

        if apply {
            for l in losers {
                match card {
                    Cardinality::Single => crate::duel::dampen_loser(&config, &l.id).await?,
                    Cardinality::TemporalSingle => {
                        crate::duel::mark_superseded(&config, &l.id).await?
                    }
                    Cardinality::Multi => unreachable!(),
                }
            }
        }
    }

    println!(
        "\nSummary: {} cluster(s) processed, {total_kept} winner(s) kept, {total_dampened} loser(s) {}.",
        work.len(),
        if apply {
            "dampened"
        } else {
            "would be dampened (dry-run)"
        }
    );
    if !apply {
        println!("Re-run with --apply to write the dampenings.");
    }

    Ok(())
}

async fn cmd_migrate_v14_confirmations(apply: bool) -> Result<()> {
    let config = crate::config::load_cached()?;
    println!(
        "v1.4 Phase 1.3 — backfilling confirmations from derivable signals{}.",
        if apply {
            ", writing back"
        } else {
            ", read-only"
        }
    );
    let (n_backfilled, summary) = crate::migrate_v14::run_confirmations(&config, apply).await?;
    println!(
        "\n{}",
        summary.render("confirmations per memory (where derivable)")
    );
    if n_backfilled == 0 {
        println!("\n(walk implementation still landing in step 1.3 commit; CLI scaffold ready.)");
    } else if !apply {
        println!("\nRun again with --apply to write {n_backfilled} backfills.");
    } else {
        println!(
            "\nBackfilled {n_backfilled} memories. Others stay at 0 and accumulate going forward."
        );
    }
    Ok(())
}

async fn cmd_init() -> Result<()> {
    print!("{}", run_init().await?);
    Ok(())
}

/// Initialize MGI-Mind and return the summary as text (no direct stdout, so it
/// can be embedded in the `doctor` report and reused off the MCP path).
pub(crate) async fn run_init() -> Result<String> {
    use crate::config::{self, MindConfig};
    use crate::storage;
    use std::fmt::Write;

    if config::is_initialized() {
        return Ok(format!(
            "MGI-Mind is already initialized at {}\n",
            config::mind_home().display()
        ));
    }

    let config = MindConfig::default();

    // Create directories
    std::fs::create_dir_all(config::sessions_dir())?;
    std::fs::create_dir_all(config::models_dir())?;

    // Save config
    config.save()?;

    let mut out = String::new();

    // Try to initialize storage (Qdrant may not be running yet)
    if is_qdrant_running()
        && let Err(e) = storage::init(&config).await
    {
        let _ = writeln!(out, "  Note: Could not initialize Qdrant collections: {e}");
        let _ = writeln!(out, "  Collections will be created on first use.");
    }

    let _ = writeln!(
        out,
        "MGI-Mind initialized at {}",
        config::mind_home().display()
    );
    let _ = writeln!(out, "  Data:     {}", config.data_dir.display());
    let _ = writeln!(out, "  Sessions: {}", config::sessions_dir().display());
    let _ = writeln!(out, "  Models:   {}", config::models_dir().display());
    let _ = writeln!(
        out,
        "\nReady. Connect your AI assistant via MCP or use CLI directly."
    );

    Ok(out)
}

async fn cmd_doctor(fix: bool) -> Result<()> {
    // Progress from any downloads goes to stderr (inside the download fns); the
    // report itself is the returned text. Print it to stdout for the CLI.
    print!("{}", run_doctor(fix).await?);
    Ok(())
}

/// Diagnose a download that reported success but left no usable file on disk -
/// almost always antivirus / Windows SmartScreen quarantine (1.2). Returned as
/// report text so both CLI and MCP surface the same actionable diagnosis
/// instead of silently looping on `--fix` while the AV keeps eating the file.
fn av_quarantine_hint(what: &str) -> String {
    format!(
        "       '{what}' reported as downloaded, but no usable file is on disk.\n\
         \x20\x20\x20\x20\x20\x20 This usually means antivirus or Windows SmartScreen quarantined it.\n\
         \x20\x20\x20\x20\x20\x20 Allow mgimind and its model cache in your AV, then re-run `mgimind doctor --fix`."
    )
}

/// Run the health checks, optionally fixing, and return the full report as text.
/// Shared by the `doctor` CLI command and the `mind_doctor` MCP tool, so neither
/// writes to stdout directly (the MCP stdout channel is JSON-RPC only).
pub(crate) async fn run_doctor(fix: bool) -> Result<String> {
    use crate::config;
    use std::fmt::Write;

    let mut out = String::new();
    let mut issues = 0;
    let mut fixed = 0;

    // Check initialization
    if !config::is_initialized() {
        let _ = writeln!(out, "[FAIL] MGI-Mind not initialized");
        if fix {
            out.push_str(&run_init().await?);
            fixed += 1;
        } else {
            issues += 1;
        }
    } else {
        let _ = writeln!(out, "[OK]   Config exists");
    }

    // Check directories
    for (name, path) in [
        ("Sessions dir", config::sessions_dir()),
        ("Models dir", config::models_dir()),
    ] {
        if path.exists() {
            let _ = writeln!(out, "[OK]   {name}");
        } else {
            let _ = writeln!(out, "[FAIL] {name} missing: {}", path.display());
            if fix {
                std::fs::create_dir_all(&path)?;
                let _ = writeln!(out, "       Fixed: created {}", path.display());
                fixed += 1;
            } else {
                issues += 1;
            }
        }
    }

    // Check Qdrant data
    let qdrant_dir = config::mind_home().join("qdrant");
    if qdrant_dir.exists() {
        let _ = writeln!(out, "[OK]   Qdrant data directory");
    } else {
        let _ = writeln!(out, "[FAIL] Qdrant data directory missing");
        if fix {
            std::fs::create_dir_all(&qdrant_dir)?;
            let _ = writeln!(out, "       Fixed: created {}", qdrant_dir.display());
            fixed += 1;
        } else {
            issues += 1;
        }
    }

    // Check Qdrant binary
    if is_qdrant_available() {
        let _ = writeln!(out, "[OK]   Qdrant binary");
    } else {
        let _ = writeln!(out, "[FAIL] Qdrant binary not found");
        if fix {
            let _ = writeln!(out, "       Downloading Qdrant...");
            match download_qdrant().await {
                Ok(()) if is_qdrant_available() => {
                    let _ = writeln!(out, "       Fixed: Qdrant downloaded");
                    fixed += 1;
                }
                // Download "succeeded" but the binary isn't there -> AV/quarantine.
                Ok(()) => {
                    let _ = writeln!(out, "{}", av_quarantine_hint("Qdrant binary"));
                    issues += 1;
                }
                Err(e) => {
                    let _ = writeln!(out, "       Download failed: {e}");
                    issues += 1;
                }
            }
        } else {
            issues += 1;
        }
    }

    // Check Qdrant running
    if is_qdrant_running() {
        let _ = writeln!(out, "[OK]   Qdrant server (running)");
    } else {
        let _ = writeln!(
            out,
            "[WARN] Qdrant server not running. Start with `mgimind serve`"
        );
    }

    // Check ONNX Runtime
    if crate::embedder::is_ort_available() {
        let _ = writeln!(out, "[OK]   ONNX Runtime");
    } else {
        let _ = writeln!(out, "[FAIL] ONNX Runtime not found");
        if fix {
            let _ = writeln!(out, "       Installing ONNX Runtime...");
            match crate::embedder::download_ort_runtime().await {
                Ok(()) if crate::embedder::is_ort_available() => {
                    let _ = writeln!(out, "       Fixed: ONNX Runtime installed");
                    fixed += 1;
                }
                Ok(()) => {
                    let _ = writeln!(out, "{}", av_quarantine_hint("ONNX Runtime"));
                    issues += 1;
                }
                Err(e) => {
                    let _ = writeln!(out, "       Install failed: {e}");
                    issues += 1;
                }
            }
        } else {
            issues += 1;
        }
    }

    // Check embedding model
    if config::is_initialized() {
        let cfg = crate::config::load_cached()?;
        let variant = crate::embedder::ModelVariant::from_env();
        if crate::embedder::is_model_downloaded(&cfg) {
            let _ = writeln!(out, "[OK]   Embedding model ({variant:?})");
        } else {
            let _ = writeln!(
                out,
                "[FAIL] Embedding model not downloaded (variant={variant:?})"
            );
            if fix {
                let _ = writeln!(out, "       Downloading model...");
                match crate::embedder::download_model(&cfg).await {
                    Ok(()) if crate::embedder::is_model_downloaded(&cfg) => {
                        let _ = writeln!(out, "       Fixed: model downloaded");
                        fixed += 1;
                    }
                    Ok(()) => {
                        let _ = writeln!(out, "{}", av_quarantine_hint("Embedding model"));
                        issues += 1;
                    }
                    Err(e) => {
                        let _ = writeln!(out, "       Download failed: {e}");
                        issues += 1;
                    }
                }
            } else {
                issues += 1;
            }
        }

        // Reranker model (audit #22) - only when reranking is enabled.
        if cfg.rerank_enabled {
            if crate::reranker::is_model_downloaded(&cfg) {
                let _ = writeln!(out, "[OK]   Reranker model");
            } else {
                let _ = writeln!(out, "[FAIL] Reranker model not downloaded");
                if fix {
                    let _ = writeln!(out, "       Downloading reranker...");
                    match crate::reranker::download_model(&cfg).await {
                        Ok(()) if crate::reranker::is_model_downloaded(&cfg) => {
                            let _ = writeln!(out, "       Fixed: reranker downloaded");
                            fixed += 1;
                        }
                        Ok(()) => {
                            let _ = writeln!(out, "{}", av_quarantine_hint("Reranker model"));
                            issues += 1;
                        }
                        Err(e) => {
                            let _ = writeln!(out, "       Download failed: {e}");
                            issues += 1;
                        }
                    }
                } else {
                    issues += 1;
                }
            }
        }
    }

    // v0.13 liveness check: stale "active" sessions (idle > 30 min) are zombies
    // — they will be auto-closed on the next `session_start` of the same agent,
    // but surfacing them here makes the leak visible the moment the user runs
    // `doctor`. Diagnostic only; we don't auto-close from doctor (the recovery
    // path is `session_start`, deliberately).
    let zombies = crate::session::list_zombies(crate::session::DEFAULT_IDLE_THRESHOLD_MINUTES);
    if zombies.is_empty() {
        let _ = writeln!(out, "[OK]   No zombie sessions");
    } else {
        let _ = writeln!(
            out,
            "[WARN] {} zombie session(s) (active for >{} min, heartbeat stale):",
            zombies.len(),
            crate::session::DEFAULT_IDLE_THRESHOLD_MINUTES
        );
        for z in &zombies {
            let _ = writeln!(
                out,
                "       agent={} idle={}min last_active={}",
                z.agent_sanitized,
                z.age_minutes,
                z.last_active_at.to_rfc3339()
            );
        }
        let _ = writeln!(
            out,
            "       Auto-closed on next `mgimind session start --agent <agent>`."
        );
    }

    // v1.4: surface the predicate-cardinality registry and any pending
    // duel events. Counts only — `mgimind doctor` is a snapshot, not a
    // resolver. The Phase 2 duel rule will offer interactive resolution.
    if config::is_initialized() {
        let cfg = crate::config::load_cached()?;
        if let Ok(predicates) = crate::knowledge::list_cardinalities(&cfg).await {
            let registered = predicates.len();
            let _ = writeln!(
                out,
                "[OK]   v1.4 predicate registry: {registered} predicate(s) with explicit cardinality"
            );

            // v1.6.4: detect High-confidence cardinality proposals
            // still in the JSON file. If any are pending, surface them
            // and offer `--fix` to bulk-register.
            let proposals_path = crate::config::mind_home()
                .join("migration")
                .join("cardinality-proposals.json");
            if proposals_path.exists() {
                let raw = std::fs::read_to_string(&proposals_path).unwrap_or_default();
                let parsed: serde_json::Value =
                    serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
                let pending_high: Vec<(String, String)> = parsed
                    .as_object()
                    .map(|obj| {
                        obj.iter()
                            .filter(|(_, v)| {
                                v.get("confidence").and_then(|c| c.as_str()) == Some("High")
                            })
                            .filter(|(name, _)| !predicates.iter().any(|(p, _)| p == *name))
                            .map(|(name, v)| {
                                let proposed = v
                                    .get("proposed")
                                    .and_then(|p| p.as_str())
                                    .unwrap_or("?")
                                    .to_string();
                                (name.clone(), proposed)
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                if !pending_high.is_empty() {
                    let n_pending = pending_high.len();
                    if fix {
                        // --fix path: register every pending High-
                        // confidence proposal via the same path
                        // `migrate-v14 cardinality --apply` uses.
                        let mut registered_now = 0usize;
                        let mut errs = 0usize;
                        for (predicate, proposed) in &pending_high {
                            let card = match crate::knowledge::Cardinality::parse(proposed) {
                                Some(c) => c,
                                None => {
                                    errs += 1;
                                    continue;
                                }
                            };
                            if crate::knowledge::register_cardinality(&cfg, predicate, card)
                                .await
                                .is_ok()
                            {
                                registered_now += 1;
                            } else {
                                errs += 1;
                            }
                        }
                        fixed += registered_now;
                        let _ = writeln!(
                            out,
                            "[FIX]  registered {registered_now} pending High-confidence cardinality proposal(s) ({errs} errors)"
                        );
                    } else {
                        issues += 1;
                        let _ = writeln!(
                            out,
                            "[INFO] {n_pending} High-confidence cardinality proposal(s) waiting — run `mgimind doctor --fix` or `mgimind migrate-v14 cardinality --apply`"
                        );
                    }
                }
            }
        }
        if let Ok((contested, shadowed)) = crate::knowledge::count_pending_conflicts(&cfg).await {
            if contested == 0 && shadowed == 0 {
                let _ = writeln!(out, "[OK]   No pending fact conflicts");
            } else {
                let _ = writeln!(
                    out,
                    "[INFO] {contested} contested (Type I) + {shadowed} propagation-shadowed (Type II); duel rule resolution arrives in Phase 2"
                );
            }
        }
        // v1.4 Phase 3 step 4: surface the inheritance-flag registry size.
        // Counts facts the current process flagged as
        // "came in from memory, not from the live session." Cleared
        // automatically at session-end and at process restart.
        let inherited = crate::doubt::inherited_count();
        if inherited == 0 {
            let _ = writeln!(out, "[OK]   No inherited-unverified facts in this session");
        } else {
            let _ = writeln!(
                out,
                "[INFO] {inherited} fact(s) flagged inherited-unverified (will clear on session end)"
            );
        }

        // v1.5 Phase 7 step 7.3: surface the error-rate guardrail count.
        // ≥3 failed test_passed signals in last 7 days promotes a fact
        // here. The Phase 8 background loop will consume the flag and
        // apply the doubt-window state transition.
        let flagged = crate::doubt::doubt_window_flag_count();
        if flagged == 0 {
            let _ = writeln!(out, "[OK]   No facts flagged by error-rate guardrail");
        } else {
            let _ = writeln!(
                out,
                "[INFO] {flagged} fact(s) flagged for doubt window by error-rate guardrail"
            );
        }

        // v1.5 Phase 6 step 6.4: surface the install-mode profile and
        // the auto-detect recommendation. The line is informational —
        // doctor never auto-applies the recommendation (§10 q6
        // mis-classification cost is silent quality drift).
        let detect_inputs = crate::install_detect::collect(&cfg).await;
        let recommendation = crate::install_mode::recommend(detect_inputs);
        let weights = cfg.install_mode.weights();
        if recommendation == cfg.install_mode {
            let _ = writeln!(
                out,
                "[OK]   install-mode: {} [d={:.2} c={:.2} e={:.2}] (matches auto-detect)",
                cfg.install_mode.as_str(),
                weights.dependants,
                weights.confirmations,
                weights.external,
            );
        } else {
            let _ = writeln!(
                out,
                "[INFO] install-mode: {} [d={:.2} c={:.2} e={:.2}] (auto-detect recommends: {} — run `mgimind config set-install-mode {}` to apply)",
                cfg.install_mode.as_str(),
                weights.dependants,
                weights.confirmations,
                weights.external,
                recommendation.as_str(),
                recommendation.as_str()
            );
        }
    }

    if issues == 0 && fixed == 0 {
        let _ = write!(out, "\nAll checks passed.");
    } else if fix && issues == 0 {
        let _ = write!(out, "\nFixed {fixed} issue(s).");
    } else if fix {
        let _ = write!(
            out,
            "\nFixed {fixed} issue(s); {issues} still need attention (see above)."
        );
    } else {
        let _ = write!(
            out,
            "\n{issues} issue(s) found. Run `mgimind doctor --fix` to fix."
        );
    }

    Ok(out)
}

async fn cmd_create(name: &str) -> Result<()> {
    println!("{}", run_create(name).await?);
    Ok(())
}

pub(crate) async fn run_create(name: &str) -> Result<String> {
    let config = crate::config::load_cached()?;
    crate::storage::create_library(&config, name).await?;
    Ok(format!("Library '{name}' created."))
}

async fn cmd_drop(name: &str) -> Result<()> {
    let config = crate::config::load_cached()?;
    crate::storage::drop_library(&config, name).await?;
    println!("Library '{name}' dropped.");
    Ok(())
}

async fn cmd_list() -> Result<()> {
    println!("{}", run_list().await?);
    Ok(())
}

pub(crate) async fn run_list() -> Result<String> {
    use std::fmt::Write;
    let config = crate::config::load_cached()?;
    let libraries = crate::storage::list_libraries(&config).await?;
    if libraries.is_empty() {
        return Ok("No libraries. Create one with `mgimind create <name>`".to_string());
    }
    let mut out = String::from("Libraries:");
    for lib in libraries {
        let _ = write!(out, "\n  - {lib}");
    }
    Ok(out)
}

async fn cmd_delete(library: &str, id: &str) -> Result<()> {
    println!("{}", run_delete(library, id).await?);
    Ok(())
}

pub(crate) async fn run_delete(library: &str, id: &str) -> Result<String> {
    let config = crate::config::load_cached()?;
    crate::storage::delete_memory(&config, library, id).await?;
    Ok(format!("Deleted from '{library}' [id: {id}]"))
}

/// Build the compact session-start briefing as a string (last session, key
/// facts, libraries, vault status). Shared by the `context` CLI command and the
/// `mind_context` MCP tool so both render identically.
pub(crate) async fn build_context(config: &crate::config::MindConfig) -> Result<String> {
    use std::fmt::Write;

    // 1. Last session
    let session =
        crate::session::last(None)?.unwrap_or_else(|| "No previous sessions.".to_string());

    // 2. Key facts from KG
    let client = crate::storage::get_client(config).await?;
    let facts_exist = client
        .collection_exists(crate::storage::FACTS_COLLECTION)
        .await
        .unwrap_or(false);

    let mut facts_summary = String::new();
    if facts_exist {
        // Newest facts first, not an arbitrary 20 (same fix as history).
        let order = qdrant_client::qdrant::OrderBy {
            key: "created_at".to_string(),
            direction: Some(qdrant_client::qdrant::Direction::Desc as i32),
            start_from: None,
        };
        let scroll = client
            .scroll(
                qdrant_client::qdrant::ScrollPointsBuilder::new(crate::storage::FACTS_COLLECTION)
                    .limit(20)
                    .with_payload(true)
                    .order_by(order),
            )
            .await;

        if let Ok(response) = scroll {
            for point in &response.result {
                let p = &point.payload;
                let subj = crate::storage::extract_string_pub(p, "subject").unwrap_or_default();
                let pred = crate::storage::extract_string_pub(p, "predicate").unwrap_or_default();
                let obj = crate::storage::extract_string_pub(p, "object").unwrap_or_default();
                let valid = crate::storage::extract_string_pub(p, "valid").unwrap_or_default();
                let status = crate::storage::extract_string_pub(p, "status").unwrap_or_default();
                // Bug fix (issue #25, PR #26): exclude dampened losers and
                // superseded history from the doctor summary so the user
                // sees the post-duel canonical state, not entombed tombstones.
                if valid == "true" && status != "stale" && status != "superseded" {
                    let _ = writeln!(facts_summary, "  {subj} -> {pred} -> {obj}");
                }
            }
        }
    }

    // 3. Libraries overview
    let (libraries, facts_count) = crate::storage::stats(config).await?;

    // 4. Vault status (no plaintext count on disk - audit #26)
    let vault_summary = crate::vault::summary();

    let mut out = String::new();
    let _ = writeln!(out, "=== MGI-Mind Context ===");
    let _ = writeln!(out);
    let _ = writeln!(out, "[Last Session]");
    // Only include the first 10 lines of the session.
    for line in session.lines().take(10) {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "[Knowledge Graph - {facts_count} facts]");
    if facts_summary.is_empty() {
        let _ = writeln!(out, "  (none)");
    } else {
        out.push_str(&facts_summary);
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "[Libraries]");
    for (name, count) in &libraries {
        let _ = writeln!(out, "  {name}: {count} memories");
    }

    // Best-effort retrieval prompt (v0.11). User-facing libraries (those not
    // prefixed with `_` — system/bench/internal) are the namespaces the agent
    // should consider searching before answering. Keep this short: it competes
    // for tokens with the actual conversation, and the MCP `instructions`
    // already carries the general policy.
    let user_libs: Vec<&str> = libraries
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| !n.starts_with('_'))
        .collect();
    if !user_libs.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "[Before answering, consider mind_search in:]");
        let _ = writeln!(out, "  {}", user_libs.join(", "));
    }

    if crate::vault::is_vault_initialized() {
        let _ = writeln!(out);
        let _ = writeln!(out, "[Vault: {vault_summary}]");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "=== End Context ===");

    Ok(out)
}

async fn cmd_context() -> Result<()> {
    let config = crate::config::load_cached()?;
    print!("{}", build_context(&config).await?);
    Ok(())
}

/// Render recent-memories list as text (shared by CLI `history` and MCP `mind_history`).
pub(crate) fn render_history(results: &[crate::storage::SearchResult]) -> String {
    use std::fmt::Write;
    if results.is_empty() {
        return "No memories yet.".to_string();
    }
    let mut out = String::from("Recent memories:\n");
    for (i, r) in results.iter().enumerate() {
        let _ = writeln!(out, "{}. [{}] {}", i + 1, r.library, r.content);
        if let Some(src) = &r.source {
            let _ = writeln!(out, "   source: {src}");
        }
    }
    out.trim_end().to_string()
}

async fn cmd_history(limit: usize) -> Result<()> {
    let config = crate::config::load_cached()?;
    let results = crate::storage::history(&config, limit).await?;
    println!("{}", render_history(&results));
    Ok(())
}

async fn cmd_web(url: &str, save_to: Option<&str>) -> Result<()> {
    println!("{}", run_web(url, save_to).await?);
    Ok(())
}

pub(crate) async fn run_web(url: &str, save_to: Option<&str>) -> Result<String> {
    // Use CRW to fetch page
    let output = std::process::Command::new("crw").arg(url).output();

    let markdown = match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("CRW failed: {err}. Is crw installed? Run: cargo install crw-cli");
        }
        Err(_) => {
            anyhow::bail!("CRW not found. Install with: cargo install crw-cli");
        }
    };

    if markdown.trim().is_empty() {
        anyhow::bail!("CRW returned empty content for {url}");
    }

    if let Some(library) = save_to {
        let config = crate::config::load_cached()?;
        // add_memory chunks long content itself (audit #3).
        let n = crate::storage::add_memory(&config, library, markdown.trim(), Some(url)).await?;
        Ok(format!("Saved {n} chunk(s) from {url} to '{library}'"))
    } else {
        Ok(markdown)
    }
}

async fn cmd_add(library: &str, content: &str, source: Option<&str>) -> Result<()> {
    let config = crate::config::load_cached()?;
    let n = crate::storage::add_memory(&config, library, content, source).await?;
    println!("Added {n} chunk(s) to '{library}'");
    Ok(())
}

/// Render search results as text. Shared by the `search` CLI command and the
/// `mind_search` MCP tool, so both produce identical output.
pub(crate) fn render_search(results: &[crate::storage::SearchResult]) -> String {
    use std::fmt::Write;
    if results.is_empty() {
        return "No results.".to_string();
    }
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        let _ = writeln!(
            out,
            "{}. [{}] (score: {:.3}) id: {}",
            i + 1,
            r.library,
            r.score,
            r.id
        );
        let _ = writeln!(out, "   {}", r.content);
        if let Some(src) = &r.source {
            let _ = writeln!(out, "   source: {src}");
        }
        let _ = writeln!(out);
    }
    out.trim_end().to_string()
}

async fn cmd_search(query: &str, library: Option<&str>, limit: usize, tier: u8) -> Result<()> {
    let config = crate::config::load_cached()?;
    let results = crate::storage::search(&config, query, library, limit, tier).await?;
    println!("{}", render_search(&results));
    Ok(())
}

async fn cmd_fact_add(subject: &str, predicate: &str, object: &str) -> Result<()> {
    let config = crate::config::load_cached()?;
    let id = crate::knowledge::add_fact(&config, subject, predicate, object).await?;
    println!("Fact added: {subject} -> {predicate} -> {object} [id: {id}]");
    Ok(())
}

/// Render fact-query results as text (shared by CLI `fact query` and MCP `mind_fact_query`).
pub(crate) fn render_facts(subject: &str, facts: &[crate::knowledge::Fact]) -> String {
    use std::fmt::Write;
    if facts.is_empty() {
        return format!("No facts about '{subject}'.");
    }
    let mut out = String::new();
    for f in facts {
        let _ = writeln!(out, "  {} -> {} -> {}", f.subject, f.predicate, f.object);
        if let Some(ts) = &f.created_at {
            let _ = writeln!(out, "    added: {ts}");
        }
    }
    out.trim_end().to_string()
}

async fn cmd_fact_query(subject: &str) -> Result<()> {
    let config = crate::config::load_cached()?;
    let facts = crate::knowledge::query_facts(&config, subject).await?;
    println!("{}", render_facts(subject, &facts));
    Ok(())
}

async fn cmd_fact_invalidate(id: &str) -> Result<()> {
    println!("{}", run_fact_invalidate(id).await?);
    Ok(())
}

pub(crate) async fn run_fact_invalidate(id: &str) -> Result<String> {
    let config = crate::config::load_cached()?;
    crate::knowledge::invalidate_fact(&config, id).await?;
    Ok(format!("Fact '{id}' invalidated."))
}

async fn cmd_session_start(agent: &str) -> Result<()> {
    println!("{}", run_session_start(agent).await?);
    Ok(())
}

pub(crate) async fn run_session_start(agent: &str) -> Result<String> {
    let report = crate::session::start(agent)?;
    let mut out = format!("Session started (agent: {agent})");
    if let Some(r) = report.recovered {
        let last = r
            .last_active_at
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "<unknown>".to_string());
        let started = r
            .started_at
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "<unknown>".to_string());
        out.push_str(&format!(
            "\n\n⚠ Recovered an interrupted session for agent '{agent}':\n  \
             started:     {started}\n  \
             last active: {last}\n  \
             It was auto-closed with a stub summary because it never received \
             mind_session_end (kill/Ctrl-C/crash). The session file is at:\n    \
             {}\n  \
             If you remember what it was, you can append a real summary to that file \
             manually — the new session is separate.",
            r.path.display()
        ));
    }
    Ok(out)
}

async fn cmd_session_last(agent: Option<&str>) -> Result<()> {
    println!("{}", run_session_last(agent).await?);
    Ok(())
}

pub(crate) async fn run_session_last(agent: Option<&str>) -> Result<String> {
    Ok(match crate::session::last(agent)? {
        Some(summary) => summary,
        None => "No previous sessions found.".to_string(),
    })
}

async fn cmd_session_end(agent: &str, summary: &str) -> Result<()> {
    println!("{}", run_session_end(agent, summary).await?);
    Ok(())
}

pub(crate) async fn run_session_end(agent: &str, summary: &str) -> Result<String> {
    crate::session::end(agent, summary)?;
    // v1.4 Phase 3 step 4: clear the inheritance flag registry at
    // session end. The flag tracks "this came in from memory in *this*
    // session"; leaking it into the next session that starts in the
    // same warm process would re-discount facts that were genuinely
    // confirmed in the new live conversation. Process restart already
    // clears the in-memory state; this clear handles the warm-process
    // case (mgimind mcp).
    crate::doubt::clear_all_inherited();
    Ok("Session ended.".to_string())
}

async fn cmd_backup(output: &str) -> Result<()> {
    println!("Backing up to {output}...");
    crate::storage::backup(output)?;
    println!("Backup complete.");
    Ok(())
}

async fn cmd_restore(input: &str) -> Result<()> {
    println!("Restoring from {input}...");
    crate::storage::restore(input)?;
    println!("Restore complete.");
    Ok(())
}

async fn cmd_export(format: &str, output: Option<&str>) -> Result<()> {
    println!("{}", run_export(format, output).await?);
    Ok(())
}

pub(crate) async fn run_export(format: &str, output: Option<&str>) -> Result<String> {
    let config = crate::config::load_cached()?;
    let out = output.unwrap_or("./mgimind-export");
    let count = crate::storage::export_all(&config, format, out).await?;
    Ok(format!(
        "Exporting as {format} to {out}...\nExported {count} entries to {out}/"
    ))
}

// --- Import ---

/// md import is the escape hatch for hand-edits. Runs as reconcile + "md
/// wins" — see `md_reconcile.rs` for the rationale. Default is dry-run that
/// prints the plan; `--apply` mutates.
async fn cmd_import(source: &str, path: &str, library: &str, apply: bool) -> Result<()> {
    println!("{}", run_import(source, path, library, apply).await?);
    Ok(())
}

/// Shared by CLI `import` and MCP `mind_import`. MCP defaults to `apply=false`
/// (dry-run is the safe default across surfaces).
pub(crate) async fn run_import(
    source: &str,
    path: &str,
    library: &str,
    apply: bool,
) -> Result<String> {
    use std::fmt::Write;
    match source.to_lowercase().as_str() {
        "obsidian" | "markdown" | "md" => {}
        other => anyhow::bail!("Unknown source: {other}. Supported: obsidian, markdown"),
    }

    let config = crate::config::MindConfig::load()
        .context("Failed to load config — run `mgimind init` first")?;

    // Ensure the library exists; ignore "already exists" since import is
    // typically rerun.
    let _ = crate::storage::create_library(&config, library).await;

    let root = std::path::Path::new(path);
    let plan = crate::md_reconcile::plan(&config, library, root).await?;
    let c = plan.counts();

    // Always lead with the rendered plan — "Qdrant now → will become (md)".
    // The asymmetric direction is the whole point of md-wins reconcile and
    // it's the thing the user must read before flipping --apply.
    let mut out = crate::md_reconcile::render_plan(&plan);

    if !apply {
        let _ = writeln!(
            out,
            "\nDry-run. Re-run with --apply to write {} new and replace {} existing.",
            c.new, c.replace
        );
        return Ok(out);
    }
    if c.new + c.replace == 0 {
        return Ok(out);
    }
    let report = crate::md_reconcile::apply(&config, &plan).await?;
    let _ = writeln!(
        out,
        "\nApplied: {} new file(s), {} replaced, {} chunks written.",
        report.added, report.replaced, report.chunks_written
    );
    Ok(out)
}

// --- Stats ---

/// Render the statistics block as text (shared by CLI `stats` and MCP `mind_stats`).
pub(crate) async fn build_stats(config: &crate::config::MindConfig) -> Result<String> {
    use std::fmt::Write;
    let (libraries, facts_count) = crate::storage::stats(config).await?;
    let total_memories: u64 = libraries.iter().map(|(_, c)| c).sum();

    let session_count = std::fs::read_dir(crate::config::sessions_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".md"))
                .count()
        })
        .unwrap_or(0);

    // Vault status (no plaintext count on disk - audit #26)
    let vault_summary = crate::vault::summary();

    let mut out = String::new();
    let _ = writeln!(out, "MGI-Mind Statistics");
    let _ = writeln!(out, "-------------------");
    let _ = writeln!(out, "Libraries:  {}", libraries.len());
    for (name, count) in &libraries {
        let _ = writeln!(out, "  {name}: {count} memories");
    }
    let _ = writeln!(out, "Total memories: {total_memories}");
    let _ = writeln!(out, "KG facts:       {facts_count}");

    // v1.6.1: distribution of dependants_count + confidence_score
    // across facts. Reads the same payload fields v1.5 retest_fact_step82
    // consumes. O(facts) scan — fine at 12k, may need bounding past 100k.
    if facts_count > 0 {
        match crate::knowledge::list_top_dependants_facts(config, facts_count as usize).await {
            Ok(pairs) if !pairs.is_empty() => {
                let dep_counts: Vec<u32> = pairs.iter().map(|(_, c)| *c).collect();
                let stats = percentiles_u32(&dep_counts);
                let _ = writeln!(
                    out,
                    "  dependants:   min={} p50={} p90={} p99={} max={} mean={:.2}",
                    stats.min, stats.p50, stats.p90, stats.p99, stats.max, stats.mean
                );
                let with_deps = dep_counts.iter().filter(|&&n| n > 0).count();
                let _ = writeln!(
                    out,
                    "                {with_deps}/{} facts have ≥1 dependant ({:.1}%)",
                    pairs.len(),
                    100.0 * with_deps as f64 / pairs.len() as f64,
                );
            }
            _ => {}
        }
    }

    // v1.5 Phase 8 in-process registry counts.
    let inherited = crate::doubt::inherited_count();
    let flagged = crate::doubt::doubt_window_flag_count();
    let _ = writeln!(out, "  in-doubt:     {flagged} flagged for retest");
    let _ = writeln!(out, "  inherited:    {inherited} (cleared on session end)");

    let _ = writeln!(out, "Sessions:       {session_count}");
    // v0.13: surface zombie-session count alongside other stats. The number
    // is the same one `mind_doctor` shows in detail.
    let zombies = crate::session::list_zombies(crate::session::DEFAULT_IDLE_THRESHOLD_MINUTES);
    if !zombies.is_empty() {
        let _ = writeln!(
            out,
            "  zombies:      {} (idle >30min, see `mgimind doctor`)",
            zombies.len()
        );
    }
    let _ = write!(out, "Vault:          {vault_summary}");
    Ok(out)
}

struct DistU32 {
    min: u32,
    p50: u32,
    p90: u32,
    p99: u32,
    max: u32,
    mean: f64,
}

fn percentiles_u32(values: &[u32]) -> DistU32 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let mean = sorted.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
    let p = |q: f64| -> u32 {
        if n == 0 {
            return 0;
        }
        let idx = ((q * (n as f64 - 1.0)).round() as usize).min(n - 1);
        sorted[idx]
    };
    DistU32 {
        min: sorted[0],
        p50: p(0.5),
        p90: p(0.9),
        p99: p(0.99),
        max: sorted[n - 1],
        mean,
    }
}

async fn cmd_stats(json: bool) -> Result<()> {
    let config = crate::config::load_cached()?;
    if json {
        let stats = build_stats_json(&config).await?;
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        println!("{}", build_stats(&config).await?);
    }
    Ok(())
}

async fn build_stats_json(config: &crate::config::MindConfig) -> Result<serde_json::Value> {
    let (libraries, facts_count) = crate::storage::stats(config).await?;
    let total_memories: u64 = libraries.iter().map(|(_, c)| c).sum();

    // Library breakdown
    let libs_json: serde_json::Map<String, serde_json::Value> = libraries
        .iter()
        .map(|(name, count)| (name.clone(), serde_json::Value::from(*count)))
        .collect();

    // Dependants distribution — same data the human view emits.
    let dependants_json = if facts_count > 0 {
        match crate::knowledge::list_top_dependants_facts(config, facts_count as usize).await {
            Ok(pairs) if !pairs.is_empty() => {
                let dep_counts: Vec<u32> = pairs.iter().map(|(_, c)| *c).collect();
                let s = percentiles_u32(&dep_counts);
                let with_deps = dep_counts.iter().filter(|&&n| n > 0).count();
                serde_json::json!({
                    "min": s.min,
                    "p50": s.p50,
                    "p90": s.p90,
                    "p99": s.p99,
                    "max": s.max,
                    "mean": s.mean,
                    "with_deps_count": with_deps,
                    "with_deps_pct": 100.0 * with_deps as f64 / pairs.len() as f64,
                })
            }
            _ => serde_json::Value::Null,
        }
    } else {
        serde_json::Value::Null
    };

    let session_count = std::fs::read_dir(crate::config::sessions_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".md"))
                .count()
        })
        .unwrap_or(0);

    let zombies = crate::session::list_zombies(crate::session::DEFAULT_IDLE_THRESHOLD_MINUTES);

    Ok(serde_json::json!({
        "libraries": libs_json,
        "total_memories": total_memories,
        "kg_facts": facts_count,
        "dependants_distribution": dependants_json,
        "in_doubt_count": crate::doubt::doubt_window_flag_count(),
        "inherited_count": crate::doubt::inherited_count(),
        "sessions": session_count,
        "zombies": zombies.len(),
        "vault": crate::vault::summary(),
    }))
}

// --- Vault commands ---

async fn cmd_vault_store(key: &str, value: &str, category: &str, desc: &str) -> Result<()> {
    crate::vault::store(key, value, category, desc)?;
    println!("Secret stored: {key} [{category}]");
    Ok(())
}

async fn cmd_vault_get(key: &str, skip_confirm: bool) -> Result<()> {
    match crate::vault::retrieve(key, skip_confirm)? {
        Some(value) => println!("{value}"),
        None => println!("Secret '{key}' not found or access denied."),
    }
    Ok(())
}

async fn cmd_vault_list() -> Result<()> {
    let keys = crate::vault::list_keys()?;
    if keys.is_empty() {
        println!("Vault is empty.");
    } else {
        println!("Vault secrets:");
        for (key, category, desc) in &keys {
            let desc_str = if desc.is_empty() { "" } else { desc.as_str() };
            println!(
                "  [{category}] {key}{}",
                if desc_str.is_empty() {
                    String::new()
                } else {
                    format!(" - {desc_str}")
                }
            );
        }
    }
    Ok(())
}

async fn cmd_vault_delete(key: &str) -> Result<()> {
    if crate::vault::delete(key)? {
        println!("Secret '{key}' deleted.");
    } else {
        println!("Secret '{key}' not found.");
    }
    Ok(())
}

// --- Qdrant management ---

fn qdrant_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "qdrant.exe"
    } else {
        "qdrant"
    }
}

fn qdrant_binary_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .unwrap_or(std::path::Path::new("."))
        .join(qdrant_binary_name())
}

fn qdrant_pid_path() -> std::path::PathBuf {
    crate::config::mind_home().join(".qdrant.pid")
}

pub fn is_qdrant_available() -> bool {
    qdrant_binary_path().exists()
}

pub fn is_qdrant_running() -> bool {
    // Check if we can connect
    std::net::TcpStream::connect("127.0.0.1:6334").is_ok()
}

const QDRANT_VERSION: &str = "1.18.1";

pub async fn download_qdrant() -> Result<()> {
    let dest = qdrant_binary_path();
    if dest.exists() {
        eprintln!("  Qdrant binary already exists at {}", dest.display());
        return Ok(());
    }

    let is_x64 = cfg!(target_arch = "x86_64");
    let (archive_name, archive_ext, expected): (&str, &str, Option<&str>) =
        if cfg!(target_os = "windows") {
            ("qdrant-x86_64-pc-windows-msvc", "zip", None)
        } else if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                ("qdrant-aarch64-apple-darwin", "tar.gz", None)
            } else {
                ("qdrant-x86_64-apple-darwin", "tar.gz", None)
            }
        } else if is_x64 {
            // musl static build runs on any glibc. The gnu build is linked
            // against glibc 2.38 and silently fails on Ubuntu LTS 22.04 (2.35)
            // and 20.04 (2.31), which is most servers in the wild.
            (
                "qdrant-x86_64-unknown-linux-musl",
                "tar.gz",
                crate::integrity::pin(crate::integrity::QDRANT_LINUX_X64_1_18_1_MUSL),
            )
        } else {
            ("qdrant-aarch64-unknown-linux-musl", "tar.gz", None)
        };

    let url = format!(
        "https://github.com/qdrant/qdrant/releases/download/v{QDRANT_VERSION}/{archive_name}.{archive_ext}"
    );

    let tmp_dir = std::env::temp_dir().join("mgimind_qdrant_download");
    std::fs::create_dir_all(&tmp_dir)?;
    let archive_path = tmp_dir.join(format!("qdrant.{archive_ext}"));

    if expected.is_none() {
        eprintln!(
            "  [warn] no pinned checksum for this platform's Qdrant - integrity not verified"
        );
    }
    eprintln!("  Downloading Qdrant v{QDRANT_VERSION}...");
    crate::util::download_file(&url, &archive_path, expected).await?;

    eprintln!("  Extracting...");
    let member = qdrant_binary_name();
    if archive_ext == "zip" {
        crate::embedder::extract_member_zip(&archive_path, member, &dest)?;
    } else {
        crate::embedder::extract_member_tar_gz(&archive_path, member, &dest)?;
    }

    if !dest.exists() {
        anyhow::bail!("Could not find qdrant binary after extraction");
    }

    // Make executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    }

    eprintln!("  Qdrant installed to {}", dest.display());

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(())
}

/// Spawn the bundled Qdrant as a **detached** background process and return its
/// PID. Detached so it survives the parent exiting - data lives inside Qdrant,
/// so it must outlive the MCP session (or the foreground `serve` shell) instead
/// of dying with it. Platform-specific: on Unix the child gets its own process
/// group so a terminal Ctrl-C (SIGINT to the foreground group) doesn't reach it;
/// on Windows we use DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP.
fn spawn_qdrant_detached() -> Result<u32> {
    let qdrant_path = qdrant_binary_path();
    if !qdrant_path.exists() {
        anyhow::bail!(
            "Qdrant binary not found at {}. Run `mgimind doctor --fix` to download it.",
            qdrant_path.display()
        );
    }

    let data_dir = crate::config::mind_home().join("qdrant");
    std::fs::create_dir_all(&data_dir)?;

    let mut command = std::process::Command::new(&qdrant_path);
    command
        .env(
            "QDRANT__STORAGE__STORAGE_PATH",
            data_dir.join("storage").to_string_lossy().to_string(),
        )
        .env("QDRANT__LOG_LEVEL", "WARN")
        // Bind to loopback only - never expose Qdrant on all interfaces (audit #7).
        .env("QDRANT__SERVICE__HOST", "127.0.0.1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Optional API-key authentication (audit #7).
    if let Ok(cfg) = crate::config::load_cached()
        && let Some(key) = cfg.qdrant_api_key
    {
        command.env("QDRANT__SERVICE__API_KEY", key);
    }

    // Detach from the parent's process group / console.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    let child = command.spawn().context("Failed to start Qdrant")?;
    let pid = child.id();

    // Save the PID for `mgimind stop`. We deliberately drop the Child handle
    // without waiting: std never kills children on drop, so the detached Qdrant
    // keeps running after we exit.
    std::fs::write(qdrant_pid_path(), pid.to_string())?;
    Ok(pid)
}

/// Poll the Qdrant gRPC port until it answers or the timeout elapses. Returns
/// whether it is running - true also covers the race where another session
/// brought Qdrant up first (the port is busy for our child, but it IS running).
fn wait_for_qdrant_ready(max_attempts: u32) -> bool {
    for _ in 0..max_attempts {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if is_qdrant_running() {
            return true;
        }
    }
    is_qdrant_running()
}

/// Ensure Qdrant is up, starting it detached if needed. Used by `mgimind mcp` so
/// a minimal user never has to run `serve` by hand. Soft on the "two sessions
/// start at once" race: if the port is already (or becomes) live, that's success
/// regardless of whose process won. Errors only when Qdrant truly can't be
/// started (e.g. binary missing), so the caller can surface a `doctor` hint.
pub(crate) async fn ensure_qdrant_running() -> Result<()> {
    if is_qdrant_running() {
        return Ok(());
    }
    let pid = spawn_qdrant_detached()?;
    if wait_for_qdrant_ready(30) {
        warn_on_dimension_mismatch().await;
        Ok(())
    } else {
        anyhow::bail!("Qdrant was started (PID {pid}) but did not become ready within 15 seconds")
    }
}

async fn cmd_serve() -> Result<()> {
    if is_qdrant_running() {
        println!("Qdrant is already running on port 6334.");
        return Ok(());
    }

    println!("Starting Qdrant...");
    let pid = spawn_qdrant_detached()?;

    if wait_for_qdrant_ready(30) {
        println!("Qdrant started on port 6333/6334 (PID: {pid})");
        warn_on_dimension_mismatch().await;
        Ok(())
    } else {
        anyhow::bail!("Qdrant started but not responding after 15 seconds");
    }
}

/// On startup, surface any collection whose vector dimension disagrees with the
/// configured `vector_size` (model changed without a reindex - audit #11). This
/// is the cheap once-per-serve check that complements the per-embedding guard,
/// so a mismatch is reported up front instead of as a raw Qdrant error on the
/// first add. Never fails serve - memory must still come up.
async fn warn_on_dimension_mismatch() {
    let Ok(cfg) = crate::config::load_cached() else {
        return;
    };
    if let Ok(mismatches) = crate::storage::dimension_mismatches(&cfg).await
        && !mismatches.is_empty()
    {
        eprintln!("[WARN] vector dimension mismatch - embedding model changed without a reindex?");
        for (name, dim) in &mismatches {
            eprintln!(
                "       collection '{name}' is dim {dim}, but config vector_size = {}",
                cfg.vector_size
            );
        }
        eprintln!("       Adds/searches on these collections will fail until you reindex.");
    }
}

async fn cmd_stop() -> Result<()> {
    let pid_path = qdrant_pid_path();
    if !pid_path.exists() {
        println!("No Qdrant PID file found. Is it running?");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_path)?;
    let pid = pid_str.trim();

    println!("Stopping Qdrant (PID: {pid})...");

    if cfg!(target_os = "windows") {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", pid, "/F"])
            .status();
    } else {
        let _ = std::process::Command::new("kill").arg(pid).status();
    }

    std::fs::remove_file(&pid_path)?;
    println!("Qdrant stopped.");
    Ok(())
}

// ===== v1.5 Phase 6: config command handler =====

async fn cmd_config(what: ConfigCmd) -> Result<()> {
    match what {
        ConfigCmd::InstallMode => cmd_config_install_mode_show().await,
        ConfigCmd::SetInstallMode { mode } => cmd_config_install_mode_set(&mode).await,
    }
}

async fn cmd_config_install_mode_show() -> Result<()> {
    let config = crate::config::MindConfig::load()
        .with_context(|| "config not initialised — run `mgimind init` first".to_string())?;
    let weights = config.install_mode.weights();
    println!(
        "install-mode: {} [dependants={:.2} confirmations={:.2} external={:.2}]",
        config.install_mode.as_str(),
        weights.dependants,
        weights.confirmations,
        weights.external,
    );

    let inputs = crate::install_detect::collect(&config).await;
    let recommendation = crate::install_mode::recommend(inputs);
    println!(
        "auto-detect recommendation: {} (external_signals_7d={}, distinct_agents_30d={})",
        recommendation.as_str(),
        inputs.external_signal_count_last_7d,
        inputs.distinct_session_agents_last_30d,
    );
    if recommendation != config.install_mode {
        println!(
            "\nthe configured mode differs from the recommendation; \
             run `mgimind config set-install-mode {}` to apply.",
            recommendation.as_str()
        );
    }
    Ok(())
}

async fn cmd_config_install_mode_set(mode_str: &str) -> Result<()> {
    let new_mode = crate::install_mode::InstallMode::parse(mode_str).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown install-mode '{mode_str}' — expected one of: chat-only, dev-with-ci, multi-tenant"
        )
    })?;
    let mut config = crate::config::MindConfig::load()
        .with_context(|| "config not initialised — run `mgimind init` first".to_string())?;
    let old = config.install_mode;
    if old == new_mode {
        println!("install-mode already {} — no change", new_mode.as_str());
        return Ok(());
    }
    config.install_mode = new_mode;
    config.save()?;
    println!(
        "install-mode: {} → {} (saved to config.json). Restart `mgimind serve` for long-lived MCP sessions to pick up the change.",
        old.as_str(),
        new_mode.as_str()
    );
    Ok(())
}

// ===== v1.5 Phase 7 + v1.6.1: outcome command handler =====

async fn cmd_outcome(
    memory_id: &str,
    signal_type_str: &str,
    success: bool,
    source: &str,
) -> Result<()> {
    let signal_type = crate::outcome::OutcomeSignal::parse(signal_type_str).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown signal_type '{signal_type_str}' — expected one of: \
                 test_passed, code_compiled, user_confirmed, cited_by"
        )
    })?;
    let cfg = crate::config::MindConfig::load()
        .with_context(|| "config not initialised — run `mgimind init` first".to_string())?;
    let signal = crate::outcome::ExternalSignal {
        signal_type,
        success,
        source: source.to_string(),
        ts: chrono::Utc::now().to_rfc3339(),
    };
    let summary = crate::outcome::record(&cfg, memory_id, signal).await?;
    println!("{summary}");
    Ok(())
}

// ===== v1.6.2: facts inspection command handlers =====

async fn cmd_facts_list(
    limit: usize,
    predicate: Option<&str>,
    sort: &str,
    with_id: bool,
) -> Result<()> {
    let cfg = crate::config::load_cached()?;
    // Pull every fact (capped). list_top_dependants_facts gives ids
    // + counts; we need subject/predicate/object too, so we use the
    // broader list_all_facts and join with dependants_count from a
    // batched payload read.
    let mut facts = crate::knowledge::list_all_facts(&cfg).await?;
    if let Some(p) = predicate {
        facts.retain(|f| f.predicate == p);
    }
    let total = facts.len();

    // Decorate with dependants_count for the sort + display.
    // O(facts) reads — fine at <100k; cap to prevent runaway over
    // a huge base.
    let read_cap = facts.len().min(10_000);
    let mut decorated: Vec<(crate::knowledge::Fact, u32)> = Vec::with_capacity(read_cap);
    let client = crate::storage::get_client(&cfg).await?;
    for f in facts.into_iter().take(read_cap) {
        let dep = crate::storage::existing_payload_string(
            &client,
            crate::storage::FACTS_COLLECTION,
            &f.id,
            "dependants_count",
        )
        .await
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
        decorated.push((f, dep));
    }

    match sort {
        "dependants" => {
            decorated.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
        }
        "created" => {
            decorated.sort_by(|a, b| {
                b.0.created_at
                    .as_deref()
                    .unwrap_or("")
                    .cmp(a.0.created_at.as_deref().unwrap_or(""))
            });
        }
        other => anyhow::bail!("unknown sort '{other}' — expected: dependants, created"),
    }

    decorated.truncate(limit);

    if decorated.is_empty() {
        println!("No facts matching filters.");
        return Ok(());
    }

    if with_id {
        println!(
            "{:>3} {:>4} {:<36} {:<28} {:<22} {:<30}",
            "#", "dep", "id", "subject", "predicate", "object"
        );
    } else {
        println!(
            "{:>3} {:>4} {:<35} {:<25} {:<35}",
            "#", "dep", "subject", "predicate", "object"
        );
    }
    for (i, (f, dep)) in decorated.iter().enumerate() {
        let s = truncate_for_table(&f.subject, if with_id { 28 } else { 35 });
        let p = truncate_for_table(&f.predicate, if with_id { 22 } else { 25 });
        let o = truncate_for_table(&f.object, if with_id { 30 } else { 35 });
        if with_id {
            println!(
                "{:>3} {:>4} {:<36} {:<28} {:<22} {:<30}",
                i + 1,
                dep,
                &f.id,
                s,
                p,
                o
            );
        } else {
            println!("{:>3} {:>4} {:<35} {:<25} {:<35}", i + 1, dep, s, p, o);
        }
    }
    println!("\nShowing {} of {} facts.", decorated.len(), total);
    Ok(())
}

async fn cmd_facts_show(id: &str) -> Result<()> {
    let cfg = crate::config::load_cached()?;
    let client = crate::storage::get_client(&cfg).await?;

    // Pull the full payload via the existing batched helper.
    const PAYLOAD_KEYS: &[&str] = &[
        "subject",
        "predicate",
        "object",
        "valid",
        "created_at",
        "dependants_count",
        "confirmations_count",
        "external_signals",
        "confidence_score",
        "doubt_drift_count",
        "status",
    ];
    let payload = crate::storage::read_point_payload_strings(
        &client,
        crate::storage::FACTS_COLLECTION,
        id,
        PAYLOAD_KEYS,
    )
    .await
    .ok_or_else(|| anyhow::anyhow!("fact id '{id}' not found"))?;

    println!("Fact: {id}");
    for &key in PAYLOAD_KEYS {
        if let Some(value) = payload.get(key) {
            println!("  {key:<22} = {value}");
        }
    }
    Ok(())
}

fn truncate_for_table(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('…');
    t
}

// ===== v1.4 Phase 5: extractor command handler =====

#[cfg(feature = "extractor")]
async fn cmd_extractor(what: ExtractorCmd) -> Result<()> {
    match what {
        ExtractorCmd::Install { variant } => {
            let v = crate::extractor::ExtractorVariant::parse(&variant).ok_or_else(|| {
                anyhow::anyhow!("unknown variant '{variant}' (expected lite or default)")
            })?;
            println!("Installing extractor: {}", v.describe());
            let warn = v.multilingual_warning();
            if !warn.is_empty() {
                println!("{warn}");
            }
            crate::extractor::install(v).await?;
            Ok(())
        }
        ExtractorCmd::Info => {
            print!("{}", crate::extractor::info());
            Ok(())
        }
        ExtractorCmd::Unload => {
            crate::extractor::shutdown_server();
            println!("llama-server shut down.");
            Ok(())
        }
        ExtractorCmd::Uninstall => {
            crate::extractor::shutdown_server();
            crate::extractor::uninstall_all()?;
            println!("Extractor uninstalled.");
            Ok(())
        }
        ExtractorCmd::Test { text, variant } => {
            let v = crate::extractor::ExtractorVariant::parse(&variant).ok_or_else(|| {
                anyhow::anyhow!("unknown variant '{variant}' (expected lite or default)")
            })?;
            let cfg = crate::extractor::ExtractConfig {
                variant: v,
                ..crate::extractor::ExtractConfig::default()
            };
            println!("Extracting from: {text}\n");
            let triples = crate::extractor::extract_facts(&cfg, &text).await?;
            if triples.is_empty() {
                println!("No triples extracted.");
            } else {
                println!("Extracted {} triple(s):", triples.len());
                for t in &triples {
                    println!("  ({}, {}, {})", t.subject, t.predicate, t.object);
                }
            }
            Ok(())
        }
        ExtractorCmd::BatchFromLibrary {
            library,
            variant,
            limit,
            dry_run,
        } => cmd_extractor_batch_from_library(&library, &variant, limit, dry_run).await,
    }
}

#[cfg(feature = "extractor")]
async fn cmd_extractor_batch_from_library(
    library: &str,
    variant: &str,
    limit: usize,
    dry_run: bool,
) -> Result<()> {
    let v = crate::extractor::ExtractorVariant::parse(variant)
        .ok_or_else(|| anyhow::anyhow!("unknown variant '{variant}' (expected lite or default)"))?;
    let extract_cfg = crate::extractor::ExtractConfig {
        variant: v,
        ..crate::extractor::ExtractConfig::default()
    };
    let mind_cfg = crate::config::load_cached()?;

    eprintln!(
        "v1.5 batch extraction from library `{library}` (variant: {variant}, dry_run: {dry_run})"
    );
    // Pull every memory in the library. The current `list_memories`
    // API takes a hard limit; pass 100k so we cover Mad's ~12k base
    // without paging logic. For libraries above that, ship a paged
    // version in v1.6.
    let scan_cap = if limit == 0 { 100_000 } else { limit };
    let memories = crate::storage::list_memories(&mind_cfg, library, scan_cap).await?;
    eprintln!("loaded {} memories from library", memories.len());

    let mut processed = 0usize;
    let mut produced_triples = 0usize;
    let mut written = 0usize;
    let mut empty_outputs = 0usize;
    let mut errors = 0usize;
    let total = memories.len();

    for (i, mem) in memories.into_iter().enumerate() {
        if limit > 0 && i >= limit {
            break;
        }

        let triples = match crate::extractor::extract_facts(&extract_cfg, &mem.content).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("memory {}: extraction error: {e}", mem.id);
                errors += 1;
                processed += 1;
                continue;
            }
        };

        if triples.is_empty() {
            empty_outputs += 1;
        } else {
            produced_triples += triples.len();
            if !dry_run {
                for t in &triples {
                    match crate::knowledge::add_fact(&mind_cfg, &t.subject, &t.predicate, &t.object)
                        .await
                    {
                        Ok(_) => written += 1,
                        Err(e) => {
                            eprintln!(
                                "memory {}: add_fact (`{}` `{}` `{}`) failed: {e}",
                                mem.id, t.subject, t.predicate, t.object
                            );
                            errors += 1;
                        }
                    }
                }
            }
        }

        processed += 1;

        if processed.is_multiple_of(100) {
            eprintln!(
                "progress: {processed}/{total} memories, {produced_triples} triples produced, \
                 {written} facts written, {empty_outputs} empty, {errors} errors"
            );
        }
    }

    eprintln!("\n=== batch extraction summary ===");
    eprintln!("memories processed     : {processed}");
    eprintln!("triples produced       : {produced_triples}");
    eprintln!("facts written          : {written}");
    eprintln!("memories with no output: {empty_outputs}");
    eprintln!("errors                 : {errors}");
    eprintln!(
        "avg triples / memory   : {:.2}",
        if processed == 0 {
            0.0
        } else {
            produced_triples as f64 / processed as f64
        }
    );

    if dry_run {
        eprintln!("\n(dry_run: no facts written. Re-run without --dry-run to persist.)");
    }
    Ok(())
}
