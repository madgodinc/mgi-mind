import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { execFile } from "child_process";
import { promisify } from "util";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";
import { accessSync } from "fs";
import { createConnection } from "net";
import { homedir } from "os";

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

// Long-lived daemon socket (audit #16). The daemon keeps the embedding model
// warm, so routing embed-heavy calls to it avoids the ~2-5s per-call model
// reload that spawning the CLI incurs.
const SOCKET_PATH =
  process.env.MGIMIND_SOCKET || resolve(homedir(), "mgimind", "daemon.sock");

// Send one newline-delimited JSON request to the daemon and resolve its rendered
// text. Returns null on any connection/parse failure so the caller can fall back
// to spawning the CLI — i.e. the daemon is a pure optimization, never required.
function daemonRequest(req, timeoutMs = 60000) {
  return new Promise((resolvePromise) => {
    let settled = false;
    const done = (val) => {
      if (!settled) {
        settled = true;
        resolvePromise(val);
      }
    };
    let sock;
    try {
      sock = createConnection(SOCKET_PATH);
    } catch {
      return done(null);
    }
    let buf = "";
    const timer = setTimeout(() => {
      try {
        sock.destroy();
      } catch {}
      done(null);
    }, timeoutMs);
    sock.on("connect", () => sock.write(JSON.stringify(req) + "\n"));
    sock.on("data", (chunk) => {
      buf += chunk.toString();
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      clearTimeout(timer);
      const line = buf.slice(0, nl);
      try {
        sock.destroy();
      } catch {}
      try {
        const resp = JSON.parse(line);
        if (resp.ok) done(resp.data?.text ?? "(no output)");
        else done(`Error: ${resp.error}`);
      } catch {
        done(null);
      }
    });
    sock.on("error", () => {
      clearTimeout(timer);
      done(null);
    });
  });
}

// Try the daemon first; fall back to spawning the CLI if it isn't there.
async function runVia(req, args) {
  const viaDaemon = await daemonRequest(req);
  return viaDaemon !== null ? viaDaemon : run(args);
}

const server = new McpServer({
  name: "mgi-mind",
  version: "0.4.0",
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
  const req = { cmd: "search", query, limit, tier };
  if (library) req.library = library;
  const result = await runVia(req, args);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_add", "Add a memory entry", {
  library: z.string().describe("Library name"),
  content: z.string().describe("Content to store"),
  source: z.string().optional().describe("Source tag"),
}, async ({ library, content, source }) => {
  const args = ["add", library, content];
  if (source) args.push("--source", source);
  const req = { cmd: "add", library, content };
  if (source) req.source = source;
  const result = await runVia(req, args);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_fact_add", "Add a knowledge graph fact", {
  subject: z.string(),
  predicate: z.string(),
  object: z.string(),
}, async ({ subject, predicate, object }) => {
  const result = await runVia(
    { cmd: "fact_add", subject, predicate, object },
    ["fact", "add", subject, predicate, object],
  );
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_fact_query", "Query facts about a subject", {
  subject: z.string(),
}, async ({ subject }) => {
  const result = await runVia({ cmd: "fact_query", subject }, ["fact", "query", subject]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_session_start", "Start a new session", {
  agent: z.string().default("unknown").describe("Agent name"),
}, async ({ agent }) => {
  const result = await run(["session", "start", "--agent", agent]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_session_last", "Get last session summary", {
  agent: z.string().optional().describe("Only consider this agent's sessions"),
}, async ({ agent }) => {
  const args = ["session", "last"];
  if (agent) args.push("--agent", agent);
  const result = await run(args);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_session_end", "End the active session for an agent", {
  agent: z.string().default("unknown").describe("Same agent name used in mind_session_start"),
  summary: z.string().describe("Session summary"),
}, async ({ agent, summary }) => {
  // Pass the agent so concurrent agents don't end each other's session (audit #14).
  const result = await run(["session", "end", "--agent", agent, "--summary", summary]);
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
  const result = await runVia({ cmd: "context" }, ["context"]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_history", "Show recent additions chronologically", {
  limit: z.number().default(10),
}, async ({ limit }) => {
  const result = await runVia({ cmd: "history", limit }, ["history", "--limit", String(limit)]);
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

server.tool("mind_vault_get", "Explain how to retrieve a secret (never returns plaintext over MCP)", {
  key: z.string().describe("Key name"),
}, async ({ key }) => {
  // Audit #2: the master password and the decrypted secret must NOT flow through
  // the MCP/LLM channel. The vault is terminal-only — instruct the user instead
  // of returning plaintext (and never with an empty/blank master password).
  return {
    content: [{
      type: "text",
      text:
        `For security, secrets are never returned over this channel.\n` +
        `Retrieve "${key}" yourself in a terminal:\n\n` +
        `    mgimind vault get ${key}\n\n` +
        `You'll be prompted for the master password (hidden) and a confirmation.`,
    }],
  };
});

server.tool("mind_vault_list", "List stored secret keys (values hidden)", {}, async () => {
  const result = await run(["vault", "list"]);
  return { content: [{ type: "text", text: result }] };
});

server.tool("mind_stats", "Show memory statistics", {}, async () => {
  const result = await runVia({ cmd: "stats" }, ["stats"]);
  return { content: [{ type: "text", text: result }] };
});

// --- Start ---

const transport = new StdioServerTransport();
await server.connect(transport);
