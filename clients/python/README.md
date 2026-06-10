# mgi-mind: Python client

A thin Python client for [mgi-mind](https://github.com/madgodinc/mgi-mind), a
local long-term memory server for AI assistants and agents. Lets any Python code
(a plain script, or an agent built on LangChain / CrewAI / AutoGen / Band) read
and write memory with a few function calls.

## Two pieces

This is the **client**. The brain itself (a Rust binary, a local Qdrant, and ONNX
models) runs as a separate server. The client talks to it over loopback HTTP.

```
  your Python code  ──HTTP──▶  mgimind serve-http  ──▶  Qdrant + models
  (pip install mgi-mind)        (installed separately)
```

So setup is two steps, not one:

1. **Install the server once** and start it:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.sh | sh
   mgimind serve-http        # prints a bearer token; keep it
   ```
2. **Install the client** and connect:
   ```bash
   pip install mgi-mind
   ```

This is the same shape as `psycopg` needing Postgres or `redis-py` needing Redis:
the pip package is a client, the data store is its own process.

## Use it

```python
from mgimind import Memory

# Point at the server. Reads MGIMIND_URL / MGIMIND_TOKEN from the environment,
# or pass them in. Connect-only: it does not start the server.
mem = Memory(url="http://127.0.0.1:8765", token="<the printed token>")

mem.add("The staging DB is Postgres 16 on db-staging:5432")

ctx = mem.search("staging database")
print(ctx)            # rendered recall text, ready to drop into a prompt
print(ctx.raw)        # the parsed response envelope, if you want metadata
```

Set the connection once via the environment and `Memory()` takes no args:

```bash
export MGIMIND_URL=http://127.0.0.1:8765
export MGIMIND_TOKEN=<token>
```

```python
mem = Memory()
```

## The five verbs

| Method | What it does |
|---|---|
| `add(content, library=None, agent=None)` | Store a memory. |
| `search(query, library=None, tier=2, limit=None)` | Hybrid search. Returns recall text (tier 1≈100, 2≈500, 3=full chars). |
| `recall(query, library=None)` | Unified recall: memories + facts + procedures in one call. |
| `add_fact(subject, predicate, object, agent=None)` | Store a structured fact triple. |
| `health()` | `True` if the server is up. |

`search` / `recall` return a `MemoryResult`. It stringifies to the rendered text
(`str(result)` is the common path for feeding a model), and `result.raw` holds the
JSON envelope. The text is what the assistant would see, a recall blob rather than
a list of rows; feed it into context, don't parse it.

## Use it as an agent tool

The frameworks turn a function into a tool, so the wrapping is trivial. For
example, with LangChain:

```python
from langchain_core.tools import tool
from mgimind import Memory

mem = Memory()

@tool
def recall_memory(query: str) -> str:
    """Search the team's long-term memory for relevant context."""
    return str(mem.search(query))
```

CrewAI, AutoGen, and Band wrap the same way: a function the agent can call.

## Status

v0.1: connect-only, synchronous, five verbs. Async methods, a managed-server
mode, and structured (non-text) results are planned. The server speaks the same
surface over MCP, so AI assistants (Claude Code, Cursor, Cline) can use the brain
without this client at all.

Apache-2.0.
