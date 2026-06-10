"""mgi-mind as memory tools for a LangGraph agent.

Two tools — recall and remember — backed by a running mgi-mind server. The
agent calls them like any other tool; its memory now outlives the process.

Run the brain first:
    docker run -p 8765:8765 -e MGIMIND_TOKEN=dev-token madgodinc/mgi-mind

Then:
    export MGIMIND_URL=http://127.0.0.1:8765 MGIMIND_TOKEN=dev-token
    export ANTHROPIC_API_KEY=...
    pip install mgi-mind langgraph langchain-anthropic
    python langgraph.py
"""

from langchain_anthropic import ChatAnthropic
from langchain_core.messages import HumanMessage
from langchain_core.tools import tool
from langgraph.prebuilt import create_react_agent

from mgimind import Memory

mem = Memory(library="agent")  # MGIMIND_URL / MGIMIND_TOKEN from the env


@tool
def recall(query: str) -> str:
    """Search the agent's long-term memory for anything relevant to a query."""
    return str(mem.search(query, tier=2))


@tool
def remember(fact: str) -> str:
    """Save a durable fact to long-term memory so future runs can recall it."""
    return str(mem.add(fact))


agent = create_react_agent(
    model=ChatAnthropic(model="claude-opus-4-8"),
    tools=[recall, remember],
    prompt="You have a persistent memory. Use `remember` to store durable "
    "facts and `recall` before answering anything that may depend on them.",
)


def ask(question: str) -> str:
    result = agent.invoke({"messages": [HumanMessage(question)]})
    return result["messages"][-1].content


if __name__ == "__main__":
    # First run teaches it something; a later, separate run can recall it.
    print(ask("Remember that our prod database is Postgres 16 on db-prod:5432."))
    print(ask("What port is the prod database on?"))
