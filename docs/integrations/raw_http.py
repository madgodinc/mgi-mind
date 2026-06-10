"""Talking to mgi-mind over plain HTTP — no client, any language.

The Python client is a thin wrapper over these calls. If your agent runtime
isn't Python, port these four lines: a bearer token, a POST, read the JSON.

Run the brain first:
    docker run -p 8765:8765 -e MGIMIND_TOKEN=dev-token madgodinc/mgi-mind

Then:
    export MGIMIND_URL=http://127.0.0.1:8765 MGIMIND_TOKEN=dev-token
    python raw_http.py
"""

import os
import urllib.request
import json

URL = os.environ.get("MGIMIND_URL", "http://127.0.0.1:8765").rstrip("/")
TOKEN = os.environ.get("MGIMIND_TOKEN", "dev-token")


def post(path: str, body: dict) -> dict:
    req = urllib.request.Request(
        f"{URL}{path}",
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {TOKEN}",
        },
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)


if __name__ == "__main__":
    # A library to write into (idempotent; created once).
    post("/library/create", {"name": "agent"})

    # Remember.
    post("/memory/add", {"library": "agent", "content": "Prod DB is on port 5432."})

    # Recall — the default response is structured JSON, ready to parse.
    #   {"ok": true, "results": [{"id", "score", "content", "author", ...}, ...]}
    hits = post("/memory/search", {"query": "prod database port", "library": "agent"})
    for hit in hits["results"]:
        print(f"{hit['score']:.3f}  {hit['content']}")

    # Want the human-readable block instead? Ask for it:
    text = post(
        "/memory/search",
        {"query": "prod database port", "library": "agent", "format": "text"},
    )
    print(text["result"])
