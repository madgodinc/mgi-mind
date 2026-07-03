"""mgi-mind as a memory plugin for Hermes (NousResearch/hermes-agent).

Hermes ships conversation tools but no shared memory, so two Hermes runs (or a
Hermes agent next to Claude Code) forget each other. This plugin registers
`recall` and `remember` tools backed by one mgi-mind server, so every agent
pointed at that server reads and writes the same brain.

The mgi-mind half below is exact: `Memory`, `mem.search`, `mem.add`,
`mem.browse`, the token-derived author. The Hermes half (the plugin.yaml keys and
the `register(ctx)` / `ctx.register_tool` signatures) is illustrative. It has NOT
been verified against a specific hermes-agent release, so check it against your
installed version's plugin docs and adjust the registration calls to match.

Drop this file into a plugin directory Hermes loads (default `~/.hermes/plugins/
mgimind/`) next to a `plugin.yaml`:

    name: mgimind
    entrypoint: hermes.py

Run the brain first, one server that owns the data. `serve-http` binds a random
free port unless you pin one, so pin it and create the library once:
    mgimind create agent
    mgimind serve-http --port 8765 --agent-token "hermes:tok-h"

Then point the plugin at it:
    export MGIMIND_URL=http://127.0.0.1:8765 MGIMIND_TOKEN=tok-h
    pip install mgi-mind

The token sets the author on every write, so `mem.browse(author="hermes")` later
shows exactly what this agent contributed to the shared pool.
"""

from mgimind import Memory

mem = Memory(library="agent")  # MGIMIND_URL / MGIMIND_TOKEN from the env


def recall(query: str) -> str:
    """Search long-term memory for anything relevant to a query."""
    return str(mem.search(query, tier=2))


def remember(fact: str) -> str:
    """Save a durable fact so future runs and other agents can recall it."""
    return str(mem.add(fact))


def register(ctx):
    """Hermes calls this at plugin load. Expose the two tools to the agent."""
    ctx.register_tool(
        name="recall",
        handler=recall,
        description="Search the shared long-term memory before answering.",
    )
    ctx.register_tool(
        name="remember",
        handler=remember,
        description="Store a durable fact in the shared long-term memory.",
    )
