"""HTTP client for a running mgi-mind server (`mgimind serve-http`).

This is a *client*. The brain itself — the Rust binary, Qdrant, and the local
models — is installed separately (see the README). This package talks to it over
the loopback HTTP surface, so any Python code (a plain script, or an agent in
LangChain / CrewAI / AutoGen / Band) can read and write memory by calling a few
functions.

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


def _envelope_text(data: dict[str, Any]) -> str:
    # serve-http wraps every tool result as {"ok": true, "result": "<text>"}
    # or {"ok": false, "error": "..."} with a 4xx/5xx status.
    if data.get("ok"):
        return str(data.get("result", ""))
    raise MgiMindError(str(data.get("error", "unknown server error")))


class Memory:
    """A connection to a running mgi-mind server.

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
        self.url = (url or os.environ.get("MGIMIND_URL") or DEFAULT_URL).rstrip("/")
        self.token = token or os.environ.get("MGIMIND_TOKEN")
        self.library = library
        self.agent = agent
        self._warn_if_token_over_plaintext()
        self._client = httpx.Client(timeout=timeout)

    def _warn_if_token_over_plaintext(self) -> None:
        # The server is loopback-only by design. If a user repoints it at a
        # remote host over plain http with a token set, the bearer crosses the
        # wire in the clear — warn rather than silently leak it.
        if not self.token:
            return
        parsed = urlparse(self.url)
        loopback = {"127.0.0.1", "::1", "localhost"}
        if parsed.scheme == "http" and (parsed.hostname or "") not in loopback:
            warnings.warn(
                f"sending a bearer token over plaintext HTTP to non-loopback host "
                f"'{parsed.hostname}'; use https for a remote mgimind server.",
                stacklevel=3,
            )

    # -- lifecycle ---------------------------------------------------------

    def close(self) -> None:
        self._client.close()

    def __enter__(self) -> "Memory":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def __del__(self) -> None:
        # Defensive: a long-lived agent that constructs Memory() and never closes
        # it shouldn't leak the connection pool. Guard a partially-built instance.
        client = getattr(self, "_client", None)
        if client is not None:
            client.close()

    # -- internals ---------------------------------------------------------

    def _headers(self) -> dict[str, str]:
        h = {"Content-Type": "application/json"}
        if self.token:
            h["Authorization"] = f"Bearer {self.token}"
        return h

    def _post(self, path: str, body: dict[str, Any]) -> MemoryResult:
        # Drop None values so the server sees only what was set.
        payload = {k: v for k, v in body.items() if v is not None}
        try:
            resp = self._client.post(
                f"{self.url}{path}", json=payload, headers=self._headers()
            )
        except httpx.HTTPError as e:
            raise MgiMindError(
                f"could not reach mgimind at {self.url}: {e}. "
                "Start one with `mgimind serve-http` and set MGIMIND_URL/MGIMIND_TOKEN."
            ) from e
        if resp.status_code in (401, 403):
            raise MgiMindError(
                f"{resp.status_code} auth failed: set MGIMIND_TOKEN to the server's "
                "bearer token (printed when you run `mgimind serve-http`)."
            )
        try:
            data = resp.json()
        except ValueError:
            raise MgiMindError(f"{resp.status_code}: non-JSON response: {resp.text[:200]}")
        return MemoryResult(text=_envelope_text(data), raw=data)

    # -- the five core verbs ----------------------------------------------

    def add(
        self, content: str, *, library: str | None = None, agent: str | None = None
    ) -> MemoryResult:
        """Store a memory. Returns the server's confirmation."""
        return self._post(
            "/memory/add",
            {
                "content": content,
                "library": library or self.library,
                "agent": agent or self.agent,
            },
        )

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
        return self._post(
            "/memory/search",
            {
                "query": query,
                "library": library or self.library,
                "tier": tier,
                "limit": limit,
            },
        )

    def recall(self, query: str, *, library: str | None = None) -> MemoryResult:
        """Unified recall: memories + facts + procedures in one call. The best
        single 'give the agent everything it knows on this topic' verb."""
        return self._post(
            "/memory/recall", {"query": query, "library": library or self.library}
        )

    def add_fact(
        self, subject: str, predicate: str, object: str, *, agent: str | None = None
    ) -> MemoryResult:
        """Store a structured fact triple (subject -> predicate -> object)."""
        return self._post(
            "/fact/add",
            {
                "subject": subject,
                "predicate": predicate,
                "object": object,
                "agent": agent or self.agent,
            },
        )

    def health(self) -> bool:
        """True if the server answers /health."""
        try:
            resp = self._client.get(f"{self.url}/health", headers=self._headers())
        except httpx.HTTPError:
            return False
        return resp.status_code == 200
