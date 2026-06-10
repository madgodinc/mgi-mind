"""mgi-mind as memory tools for a Pydantic AI agent.

Run the brain first:
    docker run -p 8765:8765 -e MGIMIND_TOKEN=dev-token madgodinc/mgi-mind

Then:
    export MGIMIND_URL=http://127.0.0.1:8765 MGIMIND_TOKEN=dev-token
    export ANTHROPIC_API_KEY=...
    pip install mgi-mind pydantic-ai
    python pydantic_ai.py
"""

from pydantic_ai import Agent

from mgimind import Memory

mem = Memory(library="agent")  # MGIMIND_URL / MGIMIND_TOKEN from the env

agent = Agent(
    "anthropic:claude-opus-4-8",
    instructions=(
        "You have a persistent memory. Use `remember` to store durable facts "
        "and `recall` before answering anything that may depend on them."
    ),
)


@agent.tool_plain
def recall(query: str) -> str:
    """Search the agent's long-term memory for anything relevant to a query."""
    return str(mem.search(query, tier=2))


@agent.tool_plain
def remember(fact: str) -> str:
    """Save a durable fact to long-term memory so future runs can recall it."""
    return str(mem.add(fact))


if __name__ == "__main__":
    print(
        agent.run_sync(
            "Remember that our prod database is Postgres 16 on db-prod:5432."
        ).output
    )
    print(agent.run_sync("What port is the prod database on?").output)
