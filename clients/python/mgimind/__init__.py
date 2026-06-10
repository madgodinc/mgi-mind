"""mgi-mind Python client.

A thin HTTP client for a running mgi-mind server (`mgimind serve-http`). Lets any
Python code or agent framework read and write long-term memory.

    from mgimind import Memory

    mem = Memory()  # connects to a server set via MGIMIND_URL / MGIMIND_TOKEN
    mem.add("The staging DB is Postgres 16 on db-staging:5432")
    print(mem.search("staging database"))

The server (the Rust binary + Qdrant + local models) is installed separately; see
the README. This package is the client.
"""

from ._client import AsyncMemory, Memory, MemoryResult, MgiMindError

__all__ = ["Memory", "AsyncMemory", "MemoryResult", "MgiMindError"]
__version__ = "0.5.0"
