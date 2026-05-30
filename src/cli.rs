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
    },

    /// Show memory statistics
    Stats,

    /// Start bundled Qdrant server
    Serve,

    /// Stop bundled Qdrant server
    Stop,

    /// Run as an MCP server over stdio. One process is the whole server and
    /// stays warm for the session - no daemon, no Unix socket, no Node wrapper.
    Mcp,

    /// Migrate legacy per-library collections into the single `memories`
    /// collection (audit #18). Idempotent; re-embeds from stored content.
    Migrate {
        /// Delete the old per-library collections after a successful copy
        #[arg(long)]
        purge: bool,
    },
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
        } => cmd_import(&source, &path, &library).await,
        Commands::Stats => cmd_stats().await,
        Commands::Backup { output } => cmd_backup(&output).await,
        Commands::Restore { input } => cmd_restore(&input).await,
        Commands::Export { format, output } => cmd_export(&format, output.as_deref()).await,
        Commands::Serve => cmd_serve().await,
        Commands::Stop => cmd_stop().await,
        Commands::Mcp => crate::mcp::serve().await,
        Commands::Migrate { purge } => cmd_migrate(purge).await,
    }
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
        if crate::embedder::is_model_downloaded(&cfg) {
            let _ = writeln!(out, "[OK]   Embedding model");
        } else {
            let _ = writeln!(out, "[FAIL] Embedding model not downloaded");
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
                if valid == "true" {
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
    crate::session::start(agent)?;
    Ok(format!("Session started (agent: {agent})"))
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

async fn cmd_import(source: &str, path: &str, library: &str) -> Result<()> {
    println!("{}", run_import(source, path, library).await?);
    Ok(())
}

pub(crate) async fn run_import(source: &str, path: &str, library: &str) -> Result<String> {
    use std::fmt::Write;
    let config = crate::config::load_cached()?;
    let dir = std::path::Path::new(path);

    if !dir.exists() || !dir.is_dir() {
        anyhow::bail!("Directory not found: {path}");
    }

    match source.to_lowercase().as_str() {
        "obsidian" | "markdown" | "md" => {}
        other => anyhow::bail!("Unknown source: {other}. Supported: obsidian, markdown"),
    }

    let mut out = String::new();

    // Ensure the target library is registered (single-collection layout, #18).
    // create_library is idempotent-friendly here: a LibraryExists error is fine.
    if crate::storage::create_library(&config, library)
        .await
        .is_ok()
    {
        let _ = writeln!(out, "Created library '{library}'");
    }

    // Scan for .md files
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    scan_md_files(dir, &mut files)?;

    let _ = writeln!(out, "Found {} markdown files in {path}", files.len());

    let mut imported = 0;
    let mut skipped = 0;

    for file in &files {
        let content = match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        // Skip empty files and very short ones
        let trimmed = content.trim();
        if trimmed.len() < 10 {
            skipped += 1;
            continue;
        }

        // Use filename as source tag
        let filename = file
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();

        // add_memory chunks the file itself (audit #3).
        match crate::storage::add_memory(&config, library, trimmed, Some(&filename)).await {
            Ok(n) => imported += n,
            Err(e) => {
                // Per-file progress/errors go to stderr (never the stdout/MCP channel).
                eprintln!("  Error importing {filename}: {e}");
                skipped += 1;
            }
        }

        if imported % 10 == 0 && imported > 0 {
            eprint!("\r  Imported: {imported}, skipped: {skipped}");
        }
    }

    let _ = write!(
        out,
        "Import complete: {imported} chunks imported, {skipped} skipped"
    );
    Ok(out)
}

fn scan_md_files(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden dirs like .obsidian, .trash
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if !name.starts_with('.') {
                scan_md_files(&path, files)?;
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    Ok(())
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
    let _ = writeln!(out, "Sessions:       {session_count}");
    let _ = write!(out, "Vault:          {vault_summary}");
    Ok(out)
}

async fn cmd_stats() -> Result<()> {
    let config = crate::config::load_cached()?;
    println!("{}", build_stats(&config).await?);
    Ok(())
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
            (
                "qdrant-x86_64-unknown-linux-gnu",
                "tar.gz",
                crate::integrity::pin(crate::integrity::QDRANT_LINUX_X64_1_18_1),
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
