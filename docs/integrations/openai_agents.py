"""mgi-mind as memory tools for the OpenAI Agents SDK.

Run the brain first:
    docker run -p 8765:8765 -e MGIMIND_TOKEN=dev-token madgodinc/mgi-mind

Then:
    export MGIMIND_URL=http://127.0.0.1:8765 MGIMIND_TOKEN=dev-token
    export OPENAI_API_KEY=...
    pip install mgi-mind openai-agents
    python openai_agents.py
"""

from agents import Agent, Runner, function_tool

from mgimind import Memory

mem = Memory(library="agent")  # MGIMIND_URL / MGIMIND_TOKEN from the env


@function_tool
def recall(query: str) -> str:
    """Search the agent's long-term memory for anything relevant to a query."""
    return str(mem.search(query, tier=2))


@function_tool
def remember(fact: str) -> str:
    """Save a durable fact to long-term memory so future runs can recall it."""
    return str(mem.add(fact))


agent = Agent(
    name="Memory assistant",
    instructions=(
        "You have a persistent memory. Use `remember` to store durable facts "
        "and `recall` before answering anything that may depend on them."
    ),
    tools=[recall, remember],
)


if __name__ == "__main__":
    print(
        Runner.run_sync(
            agent, "Remember that our prod database is Postgres 16 on db-prod:5432."
        ).final_output
    )
    print(Runner.run_sync(agent, "What port is the prod database on?").final_output)
