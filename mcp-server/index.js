import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { execFile } from "child_process";
import { promisify } from "util";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";
import { accessSync } from "fs";

const execFileAsync = promisify(execFile);

const __dirname = dirname(fileURLToPath(import.meta.url));

// Find mgimind binary - check next to mcp-server, then in parent's target
function findBinary() {
  const candidates = [
    resolve(__dirname, "..", "target", "release", "mgimind"),
    resolve(__dirname, "..", "target", "release", "mgimind.exe"),
    resolve(
      __dirname,
      "..",
      "target",
      "x86_64-pc-windows-msvc",
      "release",
      "mgimind.exe"
    ),
    "mgimind",
  ];
  return candidates.find((c) => {
    try {
      accessSync(c);
      return true;
    } catch {
      return false;
    }
  }) || "mgimind";
}

const MGIMIND = process.env.MGIMIND_BIN || findBinary();

async function run(args) {
  try {
    const { stdout, stderr } = await execFileAsync(MGIMIND, args, {
      timeout: 60000,
      env: { ...process.env },
    });
    return stdout.trim() || stderr.trim() || "(no output)";
  } catch (err) {
    return `Error: ${err.message}`;
  }
}

const server = new McpServer({
  name: "mgi-mind",
  version: "0.1.0",
});

// --- Tools ---

server.tool("mind_search", "Semantic search across memories", {
  query: z.string().describe("Search query"),
  library: z.string().optional().describe("Filter by library"),
  limit: z.number().default(5).describe("Max results"),
  tier: z.number().default(2).describe("Retrieval tier: 1=facts, 2=summaries, 3=full"),
}, async ({ query, library, limit, tier }) => {
  const args = ["search", query, "--limit", String(limit), "--tier", String(tier)];
  if (library) args.push("--library", library);
  const result = await run(args);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_add", "Add a memory entry", {
  library: z.string().describe("Library name"),
  content: z.string().describe("Content to store"),
  source: z.string().optional().describe("Source tag"),
}, async ({ library, content, source }) => {
  const args = ["add", library, content];
  if (source) args.push("--source", source);
  const result = await run(args);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_fact_add", "Add a knowledge graph fact", {
  subject: z.string(),
  predicate: z.string(),
  object: z.string(),
}, async ({ subject, predicate, object }) => {
  const result = await run(["fact", "add", subject, predicate, object]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_fact_query", "Query facts about a subject", {
  subject: z.string(),
}, async ({ subject }) => {
  const result = await run(["fact", "query", subject]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_session_start", "Start a new session", {
  agent: z.string().default("unknown").describe("Agent name"),
}, async ({ agent }) => {
  const result = await run(["session", "start", "--agent", agent]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_session_last", "Get last session summary", {}, async () => {
  const result = await run(["session", "last"]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_session_end", "End current session", {
  summary: z.string().describe("Session summary"),
}, async ({ summary }) => {
  const result = await run(["session", "end", "--summary", summary]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_create", "Create a new library", {
  name: z.string(),
}, async ({ name }) => {
  const result = await run(["create", name]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_list", "List all libraries", {}, async () => {
  const result = await run(["list"]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_doctor", "Check system health", {
  fix: z.boolean().default(false),
}, async ({ fix }) => {
  const args = ["doctor"];
  if (fix) args.push("--fix");
  const result = await run(args);
  return { content: [{ type: "text", text: result }] };
});

// --- New core tools ---

server.tool("mind_delete", "Delete a specific memory by ID", {
  library: z.string().describe("Library name"),
  id: z.string().describe("Memory UUID (from search results)"),
}, async ({ library, id }) => {
  const result = await run(["delete", library, id]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_context", "Generate compact context briefing for session start", {}, async () => {
  const result = await run(["context"]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_history", "Show recent additions chronologically", {
  limit: z.number().default(10),
}, async ({ limit }) => {
  const result = await run(["history", "--limit", String(limit)]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_web", "Read a webpage as Markdown, optionally save to library", {
  url: z.string().describe("URL to read"),
  save: z.string().optional().describe("Library to save into (omit to just read)"),
}, async ({ url, save }) => {
  const args = ["web", url];
  if (save) args.push("--save", save);
  const result = await run(args);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_export", "Export all data to JSON or Markdown files", {
  format: z.string().default("json").describe("json or md"),
  output: z.string().default("./mgimind-export").describe("Output directory"),
}, async ({ format, output }) => {
  const result = await run(["export", "--format", format, "--output", output]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_import", "Import markdown files from a directory (Obsidian, etc.)", {
  source: z.string().default("obsidian").describe("Source type: obsidian, markdown"),
  path: z.string().describe("Path to vault/directory"),
  library: z.string().default("imported").describe("Target library name"),
}, async ({ source, path, library }) => {
  const result = await run(["import", source, path, "--library", library]);
  return { content: [{ type: "text", text: result }] };
});

// --- Vault (secrets with user confirmation) ---

server.tool("mind_vault_store", "Store a secret (password, SSH key, API token)", {
  key: z.string().describe("Unique key name"),
  value: z.string().describe("Secret value"),
  category: z.string().default("other").describe("Category: password, ssh, api-key, token, other"),
  desc: z.string().default("").describe("What this secret is for"),
}, async ({ key, value, category, desc }) => {
  const args = ["vault", "store", key, value, "--category", category, "--desc", desc];
  const result = await run(args);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_vault_get", "Retrieve a secret (REQUIRES user confirmation in terminal)", {
  key: z.string().describe("Key name"),
}, async ({ key }) => {
  // NOTE: This uses --yes because MCP can't do interactive prompts.
  // The AI should warn the user before calling this.
  const result = await run(["vault", "get", key, "--yes"]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_vault_list", "List stored secret keys (values hidden)", {}, async () => {
  const result = await run(["vault", "list"]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_stats", "Show memory statistics", {}, async () => {
  const result = await run(["stats"]);
  return { content: [{ type: "text", text: result }] };
});

// --- Start ---

const transport = new StdioServerTransport();
await server.connect(transport);
