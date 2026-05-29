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

    /// Run the long-lived daemon (keeps the embedding model warm; serves the
    /// MCP client over a Unix socket to avoid per-call model reloads — audit #16)
    Daemon,

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
        Commands::Daemon => cmd_daemon().await,
        Commands::Migrate { purge } => cmd_migrate(purge).await,
    }
}

async fn cmd_daemon() -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    crate::daemon::run(config).await
}

async fn cmd_migrate(purge: bool) -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    println!(
        "Migrating legacy per-library collections into '{}'...",
        crate::storage::MEMORIES_COLLECTION
    );
    let (moved, libs) = crate::storage::migrate(&config, purge).await?;
    if libs.is_empty() {
        println!("No legacy collections found — nothing to migrate.");
    } else {
        println!(
            "Migrated {moved} entries from libraries: {}",
            libs.join(", ")
        );
        if purge {
            println!("Old per-library collections were purged.");
        } else {
            println!("Old collections kept. Re-run with --purge to delete them once verified.");
        }
    }
    Ok(())
}

async fn cmd_init() -> Result<()> {
    use crate::config::{self, MindConfig};
    use crate::storage;

    if config::is_initialized() {
        println!(
            "MGI-Mind is already initialized at {}",
            config::mind_home().display()
        );
        return Ok(());
    }

    let config = MindConfig::default();

    // Create directories
    std::fs::create_dir_all(config::sessions_dir())?;
    std::fs::create_dir_all(config::models_dir())?;

    // Save config
    config.save()?;

    // Try to initialize storage (Qdrant may not be running yet)
    if is_qdrant_running()
        && let Err(e) = storage::init(&config).await
    {
        println!("  Note: Could not initialize Qdrant collections: {e}");
        println!("  Collections will be created on first use.");
    }

    println!("MGI-Mind initialized at {}", config::mind_home().display());
    println!("  Data:     {}", config.data_dir.display());
    println!("  Sessions: {}", config::sessions_dir().display());
    println!("  Models:   {}", config::models_dir().display());
    println!("\nReady. Connect your AI assistant via MCP or use CLI directly.");

    Ok(())
}

async fn cmd_doctor(fix: bool) -> Result<()> {
    use crate::config;

    let mut issues = 0;
    let mut fixed = 0;

    // Check initialization
    if !config::is_initialized() {
        println!("[FAIL] MGI-Mind not initialized");
        if fix {
            cmd_init().await?;
            fixed += 1;
        } else {
            issues += 1;
        }
    } else {
        println!("[OK]   Config exists");
    }

    // Check directories
    for (name, path) in [
        ("Sessions dir", config::sessions_dir()),
        ("Models dir", config::models_dir()),
    ] {
        if path.exists() {
            println!("[OK]   {name}");
        } else {
            println!("[FAIL] {name} missing: {}", path.display());
            if fix {
                std::fs::create_dir_all(&path)?;
                println!("       Fixed: created {}", path.display());
                fixed += 1;
            } else {
                issues += 1;
            }
        }
    }

    // Check Qdrant data
    let qdrant_dir = config::mind_home().join("qdrant");
    if qdrant_dir.exists() {
        println!("[OK]   Qdrant data directory");
    } else {
        println!("[FAIL] Qdrant data directory missing");
        if fix {
            std::fs::create_dir_all(&qdrant_dir)?;
            println!("       Fixed: created {}", qdrant_dir.display());
            fixed += 1;
        } else {
            issues += 1;
        }
    }

    // Check Qdrant binary
    if is_qdrant_available() {
        println!("[OK]   Qdrant binary");
    } else {
        println!("[FAIL] Qdrant binary not found");
        if fix {
            println!("       Downloading Qdrant...");
            download_qdrant().await?;
            fixed += 1;
        } else {
            issues += 1;
        }
    }

    // Check Qdrant running
    if is_qdrant_running() {
        println!("[OK]   Qdrant server (running)");
    } else {
        println!("[WARN] Qdrant server not running. Start with `mgimind serve`");
    }

    // Check ONNX Runtime
    if crate::embedder::is_ort_available() {
        println!("[OK]   ONNX Runtime");
    } else {
        println!("[FAIL] ONNX Runtime not found");
        if fix {
            println!("       Installing ONNX Runtime...");
            crate::embedder::download_ort_runtime().await?;
            fixed += 1;
        } else {
            issues += 1;
        }
    }

    // Check embedding model
    if config::is_initialized() {
        let cfg = crate::config::MindConfig::load()?;
        if crate::embedder::is_model_downloaded(&cfg) {
            println!("[OK]   Embedding model");
        } else {
            println!("[FAIL] Embedding model not downloaded");
            if fix {
                println!("       Downloading model...");
                crate::embedder::download_model(&cfg).await?;
                fixed += 1;
            } else {
                issues += 1;
            }
        }

        // Reranker model (audit #22) — only when reranking is enabled.
        if cfg.rerank_enabled {
            if crate::reranker::is_model_downloaded(&cfg) {
                println!("[OK]   Reranker model");
            } else {
                println!("[FAIL] Reranker model not downloaded");
                if fix {
                    println!("       Downloading reranker...");
                    crate::reranker::download_model(&cfg).await?;
                    fixed += 1;
                } else {
                    issues += 1;
                }
            }
        }
    }

    if issues == 0 && fixed == 0 {
        println!("\nAll checks passed.");
    } else if fix {
        println!("\nFixed {fixed} issue(s).");
    } else {
        println!("\n{issues} issue(s) found. Run `mgimind doctor --fix` to fix.");
    }

    Ok(())
}

async fn cmd_create(name: &str) -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    crate::storage::create_library(&config, name).await?;
    println!("Library '{name}' created.");
    Ok(())
}

async fn cmd_drop(name: &str) -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    crate::storage::drop_library(&config, name).await?;
    println!("Library '{name}' dropped.");
    Ok(())
}

async fn cmd_list() -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    let libraries = crate::storage::list_libraries(&config).await?;
    if libraries.is_empty() {
        println!("No libraries. Create one with `mgimind create <name>`");
    } else {
        println!("Libraries:");
        for lib in libraries {
            println!("  - {lib}");
        }
    }
    Ok(())
}

async fn cmd_delete(library: &str, id: &str) -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    crate::storage::delete_memory(&config, library, id).await?;
    println!("Deleted from '{library}' [id: {id}]");
    Ok(())
}

/// Build the compact session-start briefing as a string (last session, key
/// facts, libraries, vault status). Shared by the `context` CLI command and the
/// daemon's `context` request so both render identically.
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
        let scroll = client
            .scroll(
                qdrant_client::qdrant::ScrollPointsBuilder::new(crate::storage::FACTS_COLLECTION)
                    .limit(20)
                    .with_payload(true),
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

    // 4. Vault status (no plaintext count on disk — audit #26)
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
    let config = crate::config::MindConfig::load()?;
    print!("{}", build_context(&config).await?);
    Ok(())
}

/// Render recent-memories list as text (shared by CLI `history` and the daemon).
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
    let config = crate::config::MindConfig::load()?;
    let results = crate::storage::history(&config, limit).await?;
    println!("{}", render_history(&results));
    Ok(())
}

async fn cmd_web(url: &str, save_to: Option<&str>) -> Result<()> {
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
        let config = crate::config::MindConfig::load()?;

        // Chunk and save
        let chunks = chunk_text(markdown.trim(), 500);
        let mut saved = 0;
        for chunk in &chunks {
            if chunk.trim().len() < 10 {
                continue;
            }
            match crate::storage::add_memory(&config, library, chunk, Some(url)).await {
                Ok(_) => saved += 1,
                Err(e) => {
                    if !e.to_string().contains("Duplicate") {
                        eprintln!("Error: {e}");
                    }
                }
            }
        }
        println!("Saved {saved} chunks from {url} to '{library}'");
    } else {
        // Just print
        println!("{markdown}");
    }

    Ok(())
}

async fn cmd_add(library: &str, content: &str, source: Option<&str>) -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    let id = crate::storage::add_memory(&config, library, content, source).await?;
    println!("Added to '{library}' [id: {id}]");
    Ok(())
}

/// Render search results as text. Shared by the `search` CLI command and the
/// daemon, so both produce identical output (audit #16).
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
    let config = crate::config::MindConfig::load()?;
    let results = crate::storage::search(&config, query, library, limit, tier).await?;
    println!("{}", render_search(&results));
    Ok(())
}

async fn cmd_fact_add(subject: &str, predicate: &str, object: &str) -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    let id = crate::knowledge::add_fact(&config, subject, predicate, object).await?;
    println!("Fact added: {subject} -> {predicate} -> {object} [id: {id}]");
    Ok(())
}

/// Render fact-query results as text (shared by CLI `fact query` and the daemon).
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
    let config = crate::config::MindConfig::load()?;
    let facts = crate::knowledge::query_facts(&config, subject).await?;
    println!("{}", render_facts(subject, &facts));
    Ok(())
}

async fn cmd_fact_invalidate(id: &str) -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    crate::knowledge::invalidate_fact(&config, id).await?;
    println!("Fact '{id}' invalidated.");
    Ok(())
}

async fn cmd_session_start(agent: &str) -> Result<()> {
    crate::session::start(agent)?;
    println!("Session started (agent: {agent})");
    Ok(())
}

async fn cmd_session_last(agent: Option<&str>) -> Result<()> {
    match crate::session::last(agent)? {
        Some(summary) => println!("{summary}"),
        None => println!("No previous sessions found."),
    }
    Ok(())
}

async fn cmd_session_end(agent: &str, summary: &str) -> Result<()> {
    crate::session::end(agent, summary)?;
    println!("Session ended.");
    Ok(())
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
    let config = crate::config::MindConfig::load()?;
    let out = output.unwrap_or("./mgimind-export");
    println!("Exporting as {format} to {out}...");
    let count = crate::storage::export_all(&config, format, out).await?;
    println!("Exported {count} entries to {out}/");
    Ok(())
}

// --- Import ---

async fn cmd_import(source: &str, path: &str, library: &str) -> Result<()> {
    let config = crate::config::MindConfig::load()?;
    let dir = std::path::Path::new(path);

    if !dir.exists() || !dir.is_dir() {
        anyhow::bail!("Directory not found: {path}");
    }

    match source.to_lowercase().as_str() {
        "obsidian" | "markdown" | "md" => {}
        other => anyhow::bail!("Unknown source: {other}. Supported: obsidian, markdown"),
    }

    // Ensure the target library is registered (single-collection layout, #18).
    // create_library is idempotent-friendly here: a LibraryExists error is fine.
    if crate::storage::create_library(&config, library)
        .await
        .is_ok()
    {
        println!("Created library '{library}'");
    }

    // Scan for .md files
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    scan_md_files(dir, &mut files)?;

    println!("Found {} markdown files in {path}", files.len());

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

        // Split long files into chunks (~500 chars per chunk)
        let chunks = chunk_text(trimmed, 500);

        for chunk in &chunks {
            if chunk.trim().len() < 10 {
                continue;
            }
            match crate::storage::add_memory(&config, library, chunk, Some(&filename)).await {
                Ok(_) => imported += 1,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("Duplicate") {
                        skipped += 1;
                    } else {
                        eprintln!("  Error importing {filename}: {e}");
                        skipped += 1;
                    }
                }
            }
        }

        if imported % 10 == 0 && imported > 0 {
            eprint!("\r  Imported: {imported}, skipped: {skipped}");
        }
    }

    println!("\nImport complete: {imported} chunks imported, {skipped} skipped");
    Ok(())
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

/// Split text into ~`max_chars` chunks with a small overlap between consecutive
/// chunks, and hard-split any single line longer than `max_chars` so it never
/// becomes a giant chunk that the embedder would silently truncate (audit #20).
/// (Token-aware / AST-aware chunking is planned for v0.3.)
fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }

    let overlap = (max_chars / 8).max(32);

    // Break into line units, hard-splitting overlong lines first.
    let mut units: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.chars().count() <= max_chars {
            units.push(line.to_string());
        } else {
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(max_chars) {
                units.push(piece.iter().collect());
            }
        }
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for unit in &units {
        if !current.is_empty() && current.chars().count() + unit.chars().count() + 1 > max_chars {
            chunks.push(current.clone());
            // Seed the next chunk with the tail of this one for context continuity.
            let count = current.chars().count();
            current = current
                .chars()
                .skip(count.saturating_sub(overlap))
                .collect();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(unit);
    }

    if !current.trim().is_empty() {
        chunks.push(current);
    }

    chunks
}

// --- Stats ---

/// Render the statistics block as text (shared by CLI `stats` and the daemon).
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

    // Vault status (no plaintext count on disk — audit #26)
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
    let config = crate::config::MindConfig::load()?;
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
        println!("  Qdrant binary already exists at {}", dest.display());
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
        println!("  [warn] no pinned checksum for this platform's Qdrant — integrity not verified");
    }
    println!("  Downloading Qdrant v{QDRANT_VERSION}...");
    crate::util::download_file(&url, &archive_path, expected).await?;

    println!("  Extracting...");
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

    println!("  Qdrant installed to {}", dest.display());

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(())
}

async fn cmd_serve() -> Result<()> {
    if is_qdrant_running() {
        println!("Qdrant is already running on port 6334.");
        return Ok(());
    }

    let qdrant_path = qdrant_binary_path();
    if !qdrant_path.exists() {
        anyhow::bail!(
            "Qdrant binary not found at {}. Run `mgimind doctor --fix` to download it.",
            qdrant_path.display()
        );
    }

    let data_dir = crate::config::mind_home().join("qdrant");
    std::fs::create_dir_all(&data_dir)?;

    println!("Starting Qdrant...");

    let mut command = std::process::Command::new(&qdrant_path);
    command
        .env(
            "QDRANT__STORAGE__STORAGE_PATH",
            data_dir.join("storage").to_string_lossy().to_string(),
        )
        .env("QDRANT__LOG_LEVEL", "WARN")
        // Bind to loopback only — never expose Qdrant on all interfaces (audit #7).
        .env("QDRANT__SERVICE__HOST", "127.0.0.1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Optional API-key authentication (audit #7).
    if let Ok(cfg) = crate::config::MindConfig::load()
        && let Some(key) = cfg.qdrant_api_key
    {
        command.env("QDRANT__SERVICE__API_KEY", key);
    }

    let child = command.spawn().context("Failed to start Qdrant")?;

    // Save PID
    std::fs::write(qdrant_pid_path(), child.id().to_string())?;

    // Wait for Qdrant to be ready
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if is_qdrant_running() {
            println!("Qdrant started on port 6333/6334 (PID: {})", child.id());
            warn_on_dimension_mismatch().await;
            return Ok(());
        }
    }

    anyhow::bail!("Qdrant started but not responding after 15 seconds");
}

/// On startup, surface any collection whose vector dimension disagrees with the
/// configured `vector_size` (model changed without a reindex — audit #11). This
/// is the cheap once-per-serve check that complements the per-embedding guard,
/// so a mismatch is reported up front instead of as a raw Qdrant error on the
/// first add. Never fails serve — memory must still come up.
async fn warn_on_dimension_mismatch() {
    let Ok(cfg) = crate::config::MindConfig::load() else {
        return;
    };
    if let Ok(mismatches) = crate::storage::dimension_mismatches(&cfg).await
        && !mismatches.is_empty()
    {
        eprintln!("[WARN] vector dimension mismatch — embedding model changed without a reindex?");
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

#[cfg(test)]
mod tests {
    use super::chunk_text;

    #[test]
    fn short_text_is_single_chunk() {
        assert_eq!(chunk_text("hello", 100), vec!["hello".to_string()]);
    }

    #[test]
    fn long_text_splits_with_bounded_chunks() {
        let line = "word ".repeat(400); // ~2000 chars, multiple lines worth
        let chunks = chunk_text(&line, 200);
        assert!(chunks.len() > 1, "should split into multiple chunks");
        // No chunk should be wildly over the limit (overlap adds a little).
        for c in &chunks {
            assert!(
                c.chars().count() <= 200 + 64,
                "chunk too large: {}",
                c.chars().count()
            );
        }
    }

    #[test]
    fn overlong_single_line_is_hard_split() {
        let giant = "x".repeat(1000); // one line, no whitespace
        let chunks = chunk_text(&giant, 200);
        assert!(
            chunks.len() >= 5,
            "overlong line must be hard-split, got {}",
            chunks.len()
        );
        for c in &chunks {
            assert!(c.chars().count() <= 200 + 64);
        }
    }
}
