"""End-to-end test of the Python client against a real `mgimind serve-http`.

Skipped unless MGIMIND_BIN points at a built mgimind binary and the model env is
set (the server needs ONNX models to embed). Mirrors the Rust http_integration
test: spawn the server on a known port + token, then drive the client.
"""

from __future__ import annotations

import os
import subprocess
import time

import asyncio
import inspect

import pytest

from mgimind import AsyncMemory, Memory, MgiMindError


def test_sync_async_signatures_match():
    # Mechanically enforce that the two clients can't drift on the five verbs.
    # Needs no server.
    for verb in ("add", "search", "browse", "recall", "add_fact", "health"):
        assert inspect.signature(getattr(Memory, verb)) == inspect.signature(
            getattr(AsyncMemory, verb)
        ), f"signature drift on {verb}"

BIN = os.environ.get("MGIMIND_BIN")
PORT = 47291
TOKEN = "PYTEST_TOKEN_alice"


@pytest.fixture(scope="module")
def server():
    if not BIN:
        pytest.skip("set MGIMIND_BIN to a built mgimind binary to run e2e tests")
    env = dict(os.environ)
    proc = subprocess.Popen(
        [BIN, "serve-http", "--port", str(PORT), "--agent-token", f"alice:{TOKEN}"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    url = f"http://127.0.0.1:{PORT}"
    try:
        mem = Memory(url=url, token=TOKEN)
        deadline = time.time() + 30
        while time.time() < deadline:
            if proc.poll() is not None:
                pytest.skip(f"serve-http exited early (port {PORT} busy?)")
            if mem.health():
                break
            time.sleep(0.2)
        else:
            pytest.fail("server /health never came up")
        yield url
    finally:
        proc.terminate()
        proc.wait()


def test_health_and_auth(server):
    assert Memory(url=server, token=TOKEN).health()
    # A bad token must fail on a protected call, not on /health.
    bad = Memory(url=server, token="WRONG")
    with pytest.raises(MgiMindError):
        bad.search("anything")


def test_add_search_roundtrip(server):
    mem = Memory(url=server, token=TOKEN, library="pytest")
    # library must exist; create via the binary the way an operator would.
    subprocess.run([BIN, "create", "pytest"], env=dict(os.environ), check=False)
    mem.add("The launch retrospective is the second Tuesday of every month.")
    res = mem.search("when is the launch retrospective")
    assert "launch retrospective" in str(res)
    assert res.raw.get("ok") is True

    # The default format is structured JSON: hits are addressable, not just text.
    assert res.results, "search should return structured hits by default"
    assert len(res) == len(res.results)
    top = res.results[0]
    for key in ("id", "score", "content", "library"):
        assert key in top, f"hit missing '{key}'"
    # The author is the token-derived identity ('alice'), since alice wrote it.
    assert top.get("author") == "alice"
    # Iterating the result yields hits.
    assert list(res)[0] == top


def test_search_metadata_filters(server):
    subprocess.run([BIN, "create", "pytest"], env=dict(os.environ), check=False)
    mem = Memory(url=server, token=TOKEN, library="pytest")
    mem.add("The metadata-filter canary lives here.")
    # author=alice is the token-derived identity, so it must include our write.
    hit = mem.search("metadata filter canary", author="alice")
    assert hit.results and all(h["author"] == "alice" for h in hit.results)
    # A future created_since window must exclude everything.
    empty = mem.search("metadata filter canary", created_since="2099-01-01")
    assert len(empty) == 0
    # A bad date is a server-side 400 → MgiMindError, not a silent empty.
    with pytest.raises(MgiMindError):
        mem.search("x", created_since="not-a-date")


def test_browse_lists_by_metadata_without_query(server):
    subprocess.run([BIN, "create", "pytest"], env=dict(os.environ), check=False)
    mem = Memory(url=server, token=TOKEN, library="pytest")
    mem.add("Browse canary one.")
    mem.add("Browse canary two.")
    # No query — pure inventory. author=alice is the token identity.
    listing = mem.browse(author="alice", limit=50)
    assert len(listing) >= 2
    # Records carry inventory metadata (created_at, library), not a score.
    rec = listing.results[0]
    assert "created_at" in rec and "library" in rec
    assert rec.get("author") == "alice"
    # A future window lists nothing.
    assert len(mem.browse(created_since="2099-01-01")) == 0


def test_recall_splits_silos(server):
    subprocess.run([BIN, "create", "pytest"], env=dict(os.environ), check=False)
    mem = Memory(url=server, token=TOKEN, library="pytest")
    mem.add("The staging database lives at db-staging.internal:5432.")
    res = mem.recall("staging database address")
    # Recall returns the three silos as separate, addressable fields.
    assert isinstance(res.facts, list)
    assert isinstance(res.memories, list)
    assert isinstance(res.procedures, str)
    assert res.memories, "recall should surface the memory we just wrote"
    assert "staging database" in str(res)


def test_bad_args_raise(server):
    # search with no query is a dispatch error → 4xx → MgiMindError, not a crash.
    mem = Memory(url=server, token=TOKEN)
    with pytest.raises(MgiMindError):
        mem._post("/memory/search", {})


def test_async_add_search_roundtrip(server):
    # Reuse the `pytest` library created by the sync roundtrip test (which runs
    # first); avoids racing the library registry on a fresh name.
    subprocess.run([BIN, "create", "pytest"], env=dict(os.environ), check=False)

    async def run():
        async with AsyncMemory(url=server, token=TOKEN, library="pytest") as mem:
            assert await mem.health()
            await mem.add("The async standup is at 09:30 on weekdays.")
            res = await mem.search("when is the async standup")
            assert "async standup" in str(res)
            with pytest.raises(MgiMindError):
                await AsyncMemory(url=server, token="WRONG")._post(
                    "/memory/search", {"query": "x"}
                )

    asyncio.run(run())
