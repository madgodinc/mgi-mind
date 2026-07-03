# OpenClaude (CC Mirror) + mgi-mind

OpenClaude runs a Conductor that decomposes a task and spawns background Claude
agents to work it in parallel. Those agents share a task graph but not memory, so
what one agent learns is gone when its context ends. Point them all at one
mgi-mind server and the whole swarm reads and writes a single brain.

OpenClaude is Claude Code underneath, so it connects the same two ways Claude
Code does.

## One Conductor, stdio MCP

For a single-agent or sequential run, register the brain as an MCP server. Add it
to the project `.mcp.json` the Conductor reads:

```json
{
  "mcpServers": {
    "mgimind": { "command": "mgimind", "args": ["mcp"] }
  }
}
```

The agent now has `mind_search`, `mind_add`, `mind_recall`, and the rest as
tools. Over MCP the author on a write is self-asserted through an optional
`agent` argument, so a single driver is fine, but the brain cannot prove which
agent wrote what. When several agents write at once, use the HTTP door below.

## A swarm sharing one pool, over HTTP

Run one server, create the library once, and pin a port (`serve-http` binds a
random free port otherwise):

```bash
mgimind create shared
mgimind serve-http --port 8765 \
  --agent-token "conductor:tok-c" \
  --agent-token "worker:tok-w"
```

Give each spawned agent a memory tool that posts to that server. The four-line
shape is in [`raw_http.py`](./raw_http.py); wrapped as a Claude Code tool it is
`recall` (POST `/memory/search`) and `remember` (POST `/memory/add`). Both take a
`library` field, and that library must already exist (the `create` step above).

Two things make the single server the right topology for a swarm. The author is
derived from the token, not self-asserted, so a write through `tok-w` is provably
`author=worker` and the Conductor can pull one worker's contributions from
`/memory/by-agent`. And one process owns the writes to the plain JSON files
(libraries, kv), so a dozen agents writing at once do not race each other.

The underlying Qdrant is a shared local service, so stdio and HTTP callers reach
the same store either way; the difference is trustworthy attribution and clean
concurrent writes, not which door sees the data.

## Which to use

| Run shape | Wiring |
|-----------|--------|
| Single Conductor, sequential | stdio MCP (`mgimind mcp`) |
| Many background agents, shared memory | one `serve-http`, HTTP tools, per-agent tokens |

Reads are safe from either door. The single-process writer is the one to trust
when many agents write the same pool at once.
