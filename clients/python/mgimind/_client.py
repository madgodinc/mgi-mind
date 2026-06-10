"""HTTP client for a running mgi-mind server (`mgimind serve-http`).

This is a *client*. The brain itself — the Rust binary, Qdrant, and the local
models — is installed separately (see the README). This package talks to it over
the loopback HTTP surface, so any Python code (a plain script, or an agent in
LangChain / CrewAI / AutoGen / Band) can read and write memory by calling a few
functions.

Two clients with the same five verbs: `Memory` (synchronous) and `AsyncMemory`
(`await`-able, for async agent frameworks like LangGraph, Pydantic AI, the OpenAI
Agents SDK, or any FastAPI handler, where a blocking call would stall the event
loop).

The server's responses are rendered text, not structured rows: `search` returns
the same recall blob the assistant would see, which is exactly what you feed back
into a model's context. `MemoryResult` wraps that text plus the raw response
envelope; it stringifies to the text, so `str(mem.search(q))` is the common path.
"""

from __future__ import annotations

import os
import warnings
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlparse

import httpx

DEFAULT_URL = "http://127.0.0.1:8765"
_TIMEOUT = 30.0
_LOOPBACK = {"127.0.0.1", "::1", "localhost"}


@dataclass
class MemoryResult:
    """One server response. `.text` is the rendered result the model should see;
    `.raw` is the parsed JSON envelope for callers that want the metadata."""

    text: str
    raw: dict[str, Any]

    def __str__(self) -> str:  # the common case: drop it into a prompt
        return self.text

    def __bool__(self) -> bool:
        return bool(self.text.strip())


class MgiMindError(RuntimeError):
    """A server error (4xx/5xx) or a transport failure."""


# -- shared request/response helpers (used by both sync and async clients) -----


def _resolve(url: str | None, token: str | None) -> tuple[str, str | None]:
    resolved_url = (url or os.environ.get("MGIMIND_URL") or DEFAULT_URL).rstrip("/")
    resolved_token = token or os.environ.get("MGIMIND_TOKEN")
    _warn_if_token_over_plaintext(resolved_url, resolved_token)
    return resolved_url, resolved_token


def _warn_if_token_over_plaintext(url: str, token: str | None) -> None:
    # The server is loopback-only by design. If a user repoints it at a remote
    # host over plain http with a token set, the bearer crosses the wire in the
    # clear — warn rather than silently leak it.
    if not token:
        return
    parsed = urlparse(url)
    if parsed.scheme == "http" and (parsed.hostname or "") not in _LOOPBACK:
        warnings.warn(
            f"sending a bearer token over plaintext HTTP to non-loopback host "
            f"'{parsed.hostname}'; use https for a remote mgimind server.",
            # 4 = user's Memory(...) call -> __init__ -> _resolve -> here.
            stacklevel=4,
        )


def _headers(token: str | None) -> dict[str, str]:
    h = {"Content-Type": "application/json"}
    if token:
        h["Authorization"] = f"Bearer {token}"
    return h


def _prune(body: dict[str, Any]) -> dict[str, Any]:
    # Drop None values so the server sees only what was set.
    return {k: v for k, v in body.items() if v is not None}


def _parse(resp: httpx.Response) -> MemoryResult:
    if resp.status_code in (401, 403):
        raise MgiMindError(
            f"{resp.status_code} auth failed: set MGIMIND_TOKEN to the server's "
            "bearer token (printed when you run `mgimind serve-http`)."
        )
    try:
        data = resp.json()
    except ValueError:
        raise MgiMindError(f"{resp.status_code}: non-JSON response: {resp.text[:200]}")
    # serve-http wraps every tool result as {"ok": true, "result": "<text>"} or
    # {"ok": false, "error": "..."} (with a 4xx/5xx status).
    if data.get("ok"):
        return MemoryResult(text=str(data.get("result", "")), raw=data)
    raise MgiMindError(str(data.get("error", "unknown server error")))


def _unreachable(url: str, e: Exception) -> MgiMindError:
    return MgiMindError(
        f"could not reach mgimind at {url}: {e}. "
        "Start one with `mgimind serve-http` and set MGIMIND_URL/MGIMIND_TOKEN."
    )


# Body builders for the five verbs. Shared so the sync and async clients can't
# drift on argument names.


def _add_body(content: str, library: str | None, agent: str | None) -> dict[str, Any]:
    return {"content": content, "library": library, "agent": agent}


def _search_body(
    query: str, library: str | None, tier: int, limit: int | None
) -> dict[str, Any]:
    return {"query": query, "library": library, "tier": tier, "limit": limit}


def _recall_body(query: str, library: str | None) -> dict[str, Any]:
    return {"query": query, "library": library}


def _fact_body(
    subject: str, predicate: str, object: str, agent: str | None
) -> dict[str, Any]:
    return {"subject": subject, "predicate": predicate, "object": object, "agent": agent}


class Memory:
    """A connection to a running mgi-mind server (synchronous).

    Connect-only: this does NOT start the server. Run `mgimind serve-http`
    yourself (it prints a bearer token), then point the client at it via the
    constructor args or the `MGIMIND_URL` / `MGIMIND_TOKEN` environment
    variables. The constructor creates an httpx client but opens no socket until
    the first call; use `with Memory() as mem:` or call `.close()` to release it.

        mem = Memory()                         # reads env, or localhost defaults
        mem.add("The deploy host is 10.0.0.5") # write a memory
        ctx = mem.search("deploy host")        # read it back as recall text
    """

    def __init__(
        self,
        url: str | None = None,
        token: str | None = None,
        *,
        library: str | None = None,
        agent: str | None = None,
        timeout: float = _TIMEOUT,
    ) -> None:
        self.url, self.token = _resolve(url, token)
        self.library = library
        self.agent = agent
        self._client = httpx.Client(timeout=timeout)

    def close(self) -> None:
        self._client.close()

    def __enter__(self) -> "Memory":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def __del__(self) -> None:
        # Defensive: a long-lived agent that never closes shouldn't leak the
        # connection pool. Guard a partially-built instance.
        client = getattr(self, "_client", None)
        if client is not None:
            client.close()

    def _post(self, path: str, body: dict[str, Any]) -> MemoryResult:
        try:
            resp = self._client.post(
                f"{self.url}{path}", json=_prune(body), headers=_headers(self.token)
            )
        except httpx.HTTPError as e:
            raise _unreachable(self.url, e) from e
        return _parse(resp)

    def add(
        self, content: str, *, library: str | None = None, agent: str | None = None
    ) -> MemoryResult:
        """Store a memory. Returns the server's confirmation."""
        return self._post("/memory/add", _add_body(content, library or self.library, agent or self.agent))

    def search(
        self,
        query: str,
        *,
        library: str | None = None,
        tier: int = 2,
        limit: int | None = None,
    ) -> MemoryResult:
        """Hybrid search over memories. Returns rendered recall text (tier 1≈100,
        2≈500, 3=full chars). Feed `str(result)` straight into a model prompt."""
        return self._post("/memory/search", _search_body(query, library or self.library, tier, limit))

    def recall(self, query: str, *, library: str | None = None) -> MemoryResult:
        """Unified recall: memories + facts + procedures in one call. The best
        single 'give the agent everything it knows on this topic' verb."""
        return self._post("/memory/recall", _recall_body(query, library or self.library))

    def add_fact(
        self, subject: str, predicate: str, object: str, *, agent: str | None = None
    ) -> MemoryResult:
        """Store a structured fact triple (subject -> predicate -> object)."""
        return self._post("/fact/add", _fact_body(subject, predicate, object, agent or self.agent))

    def health(self) -> bool:
        """True if the server answers /health."""
        try:
            resp = self._client.get(f"{self.url}/health", headers=_headers(self.token))
        except httpx.HTTPError:
            return False
        return resp.status_code == 200


class AsyncMemory:
    """Async twin of `Memory` for asyncio agent frameworks. Same five verbs,
    `await`-able, backed by `httpx.AsyncClient`. Use `async with AsyncMemory()`
    or `await mem.aclose()`.

        mem = AsyncMemory()
        await mem.add("...")
        ctx = await mem.search("deploy host")
    """

    def __init__(
        self,
        url: str | None = None,
        token: str | None = None,
        *,
        library: str | None = None,
        agent: str | None = None,
        timeout: float = _TIMEOUT,
    ) -> None:
        self.url, self.token = _resolve(url, token)
        self.library = library
        self.agent = agent
        self._client = httpx.AsyncClient(timeout=timeout)

    async def aclose(self) -> None:
        await self._client.aclose()

    async def __aenter__(self) -> "AsyncMemory":
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.aclose()

    def __del__(self) -> None:
        # An AsyncClient can only be closed with `await aclose()`, which is
        # impossible from a finalizer (can't await, the loop may be gone). So we
        # can't free the pool here — but a long-lived agent that forgot to close
        # should at least be told, the idiomatic way, via ResourceWarning.
        client = getattr(self, "_client", None)
        if client is not None and not client.is_closed:
            warnings.warn(
                "AsyncMemory was not closed; use `async with AsyncMemory()` or "
                "`await mem.aclose()`. The connection pool may leak.",
                ResourceWarning,
                stacklevel=2,
            )

    async def _post(self, path: str, body: dict[str, Any]) -> MemoryResult:
        try:
            resp = await self._client.post(
                f"{self.url}{path}", json=_prune(body), headers=_headers(self.token)
            )
        except httpx.HTTPError as e:
            raise _unreachable(self.url, e) from e
        return _parse(resp)

    async def add(
        self, content: str, *, library: str | None = None, agent: str | None = None
    ) -> MemoryResult:
        """Store a memory."""
        return await self._post("/memory/add", _add_body(content, library or self.library, agent or self.agent))

    async def search(
        self,
        query: str,
        *,
        library: str | None = None,
        tier: int = 2,
        limit: int | None = None,
    ) -> MemoryResult:
        """Hybrid search. Returns rendered recall text."""
        return await self._post("/memory/search", _search_body(query, library or self.library, tier, limit))

    async def recall(self, query: str, *, library: str | None = None) -> MemoryResult:
        """Unified recall: memories + facts + procedures in one call."""
        return await self._post("/memory/recall", _recall_body(query, library or self.library))

    async def add_fact(
        self, subject: str, predicate: str, object: str, *, agent: str | None = None
    ) -> MemoryResult:
        """Store a fact triple (subject -> predicate -> object)."""
        return await self._post("/fact/add", _fact_body(subject, predicate, object, agent or self.agent))

    async def health(self) -> bool:
        """True if the server answers /health."""
        try:
            resp = await self._client.get(f"{self.url}/health", headers=_headers(self.token))
        except httpx.HTTPError:
            return False
        return resp.status_code == 200
