# Plugging mgi-mind into an agent framework

mgi-mind is one brain with several doors. An agent framework reaches it through
the HTTP surface (`mgimind serve-http`) and the Python client (`pip install
mgi-mind`). You give the agent two tools, one to recall and one to remember, so
its memory survives the process.

There is no framework-specific adapter to install. The pattern is the same
everywhere: wrap `mem.search(...)` and `mem.add(...)` in whatever the framework
calls a "tool". These examples show that wrapping for four runtimes.

## The one shared idea

```python
from mgimind import Memory

mem = Memory()  # reads MGIMIND_URL / MGIMIND_TOKEN, or localhost defaults

def recall(query: str) -> str:
    """Search the agent's long-term memory."""
    return str(mem.search(query, tier=2))

def remember(fact: str) -> str:
    """Save a durable fact to long-term memory."""
    return str(mem.add(fact))
```

Those two functions are the whole integration. Each example below registers
them the way its framework expects, then runs an agent that recalls a fact a
previous run saved.

## Start the brain

```bash
# One command, no local install:
docker run -p 8765:8765 -e MGIMIND_TOKEN=dev-token madgodinc/mgi-mind

# ...or run a locally built binary:
mgimind serve-http --agent-token "agent:dev-token"
```

Then point the client at it:

```bash
export MGIMIND_URL=http://127.0.0.1:8765
export MGIMIND_TOKEN=dev-token
pip install mgi-mind
```

## The examples

| File | Framework | Tool primitive |
|------|-----------|----------------|
| [`langgraph.py`](./langgraph.py) | LangGraph + LangChain | `@tool` |
| [`pydantic_ai.py`](./pydantic_ai.py) | Pydantic AI | `@agent.tool_plain` |
| [`openai_agents.py`](./openai_agents.py) | OpenAI Agents SDK | `@function_tool` |
| [`raw_http.py`](./raw_http.py) | Any language (no client) | plain HTTP POST |

Every example targets the live HTTP server above; it assumes `MGIMIND_URL` and
`MGIMIND_TOKEN` are set and the agent's own model key (`ANTHROPIC_API_KEY` or
`OPENAI_API_KEY`) is present. They are illustrations, not a maintained package.
Copy the dozen lines you need.

## The HTTP surface

An HTTP-only agent reaches the same memory an MCP agent does. Every route takes
a bearer token; reads and non-destructive writes are exposed, destructive/bulk
tools (delete, export, import-apply, vault, `consolidate --apply`) stay CLI-only.

| Route | Does |
|-------|------|
| `POST /memory/search` | semantic search, with metadata filters |
| `POST /memory/browse` | list by metadata, no query (inventory) |
| `POST /memory/recall` | facts + memories + procedures in one call |
| `POST /memory/add` `/memory/ingest` | write |
| `POST /memory/by-agent` | what one agent wrote |
| `POST /fact/add` `/fact/query` `/fact/invalidate` | knowledge-graph triples |
| `POST /procedure/learn` `/procedure/recall` | error→fix playbooks |
| `POST /library/create` `/library/list` | library namespaces |
| `POST /quarantine/list` `/quarantine/promote` | relevance-gate recovery |
| `POST /consolidate` | dedup/decay preview (dry-run) |
| `POST /session/start` `/end` `/last` `/context` | continuity |

Search and browse return structured JSON by default; pass `format: "text"` for a
rendered block. See [`raw_http.py`](./raw_http.py) for the request shape.

Outbound fetch (`mind_web`) and destructive ops (delete, export, vault,
`consolidate --apply`) stay CLI/MCP-only by design: a token-gated loopback fetch
would make the server an SSRF deputy, and an HTTP agent has its own network.

## Why no per-framework package

The brain is universal on purpose. A framework adapter would be a thin shim over
two function calls and another thing to version against every framework release.
The HTTP surface plus a typed Python client already covers every Python runtime,
and the raw-HTTP example covers everything else. See
[`docs/design`](../design) for the surface contract.

## Multi-agent: who wrote what

When several agents share one brain, give each its own token so the brain can
attribute writes:

```bash
mgimind serve-http \
  --agent-token "researcher:tok-r" \
  --agent-token "writer:tok-w"
```

A memory written through `tok-r` is tagged `author=researcher`. The identity is
derived from the token, not self-asserted, so one agent can't write under
another's name. Each result from `mem.search(...)` carries its `author`; query
one agent's writes with `mem` pointed at `/memory/by-agent`.
