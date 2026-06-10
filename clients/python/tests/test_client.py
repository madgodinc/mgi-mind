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
    for verb in ("add", "search", "recall", "add_fact", "health"):
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
