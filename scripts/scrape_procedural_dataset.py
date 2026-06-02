#!/usr/bin/env python3
"""Procedural dataset scraper (phase Д6, v0.10.0).

Mines "failing test → fix commit" pairs from local git repositories by walking
commit history, picking fix-pattern commits, and pairing the modified test
output / error message with the fix description. Output is JSONL ready for
`mgimind bench-procedural`.

Heuristic (intentionally conservative, false-positives are worse than misses):
  1. Pick commits whose subject matches `^(fix|fixes|bug)[\(\:]` — these are
     the explicit fix commits.
  2. From each fix commit, parse the body for an error signature (lines that
     look like a panic / traceback / compiler error / failing assert).
  3. The commit subject + body becomes the `fix`.
  4. The error signature (if any) becomes the `error`. Skip the commit if no
     error signature is found.
  5. Language and stratum are inferred from the changed file extensions and
     the error pattern (compile / test / runtime).

Usage:
  scrape_procedural_dataset.py REPO [REPO ...] --output dataset.jsonl [--max-per-repo 30]

Не делает: HTTP, GitHub API, clone. Только git log над уже клонированными
репозиториями. Pure local.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

FIX_PATTERN = re.compile(r"^(fix|fixes|bug|patch|hotfix)\b[\(\:\s]", re.IGNORECASE)

# Heuristic error-line patterns. The first capture group is the error sig.
ERROR_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("rust-compile", re.compile(r"(error\[E\d+\]:[^\n]+)", re.MULTILINE)),
    ("rust-runtime", re.compile(r"(thread ['\"][^'\"]*['\"] panicked[^\n]+)", re.MULTILINE)),
    ("python-traceback", re.compile(r"^(\w+(?:Error|Exception): [^\n]+)", re.MULTILINE)),
    ("ts-runtime", re.compile(r"((?:Type|Reference|Range|Syntax)Error: [^\n]+)", re.MULTILINE)),
    ("test-assert", re.compile(r"((?:assert|expect)[\w\.]*\([^\)]+\)[^\n]*)", re.MULTILINE)),
    ("test-expected", re.compile(r"(expected [^\n]{5,80} (?:received|got|but was|actual)[^\n]+)", re.MULTILINE | re.IGNORECASE)),
    ("go-error", re.compile(r"(panic: [^\n]+)", re.MULTILINE)),
    # Code-quoted error messages in commit body — `'foo' is undefined` style.
    ("backtick-error", re.compile(r"`([^`\n]{8,200}(?:error|failed|not found|already exists|expected|missing|invalid|unable)[^`\n]*)`", re.MULTILINE | re.IGNORECASE)),
    ("quoted-error", re.compile(r"'([^'\n]{8,200}(?:error|failed|not found|already exists|expected|missing|invalid|unable)[^'\n]*)'", re.MULTILINE | re.IGNORECASE)),
    # Last-resort: a "symptom sentence" — body line describing what went wrong.
    ("symptom-sentence", re.compile(r"^([^\n]*(?:Root cause|was failing|broke|regression|hangs|crash(?:es|ed)?|silently fails|leak)[^\n]{5,200})$", re.MULTILINE | re.IGNORECASE)),
]

EXT_LANG = {
    ".rs": "rust",
    ".ts": "ts",
    ".tsx": "ts",
    ".js": "ts",
    ".jsx": "ts",
    ".py": "py",
    ".go": "go",
    ".java": "java",
}

STRATUM_HINT = {
    "rust-compile": ("rust", "compile"),
    "rust-runtime": ("rust", "runtime"),
    "python-traceback": ("py", "runtime"),
    "ts-runtime": ("ts", "runtime"),
    "test-assert": (None, "test"),
    "test-expected": (None, "test"),
    "go-error": ("go", "runtime"),
    "backtick-error": (None, "runtime"),
    "quoted-error": (None, "runtime"),
    "symptom-sentence": (None, "runtime"),
}


def run(cmd: list[str], cwd: Path) -> str:
    out = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=False)
    return out.stdout


def commit_subjects(repo: Path, max_commits: int = 5000) -> list[str]:
    """Return SHA list, newest first, capped."""
    raw = run(["git", "log", f"--max-count={max_commits}", "--pretty=%H %s"], repo)
    return [line for line in raw.splitlines() if line.strip()]


def commit_body(repo: Path, sha: str) -> str:
    return run(["git", "log", "-1", "--pretty=%B", sha], repo)


def commit_files(repo: Path, sha: str) -> list[str]:
    raw = run(["git", "show", "--name-only", "--pretty=", sha], repo)
    return [line for line in raw.splitlines() if line.strip()]


def infer_language(files: list[str]) -> str | None:
    """Pick the most common file extension's language."""
    counts: dict[str, int] = {}
    for f in files:
        ext = Path(f).suffix
        lang = EXT_LANG.get(ext)
        if lang:
            counts[lang] = counts.get(lang, 0) + 1
    if not counts:
        return None
    return max(counts.items(), key=lambda kv: kv[1])[0]


# File-based stratum inference: which kind of fix is this commit?
TEST_PATH_HINTS = ("/test", "/tests/", "_test.", ".test.", "/spec", "_spec.", ".spec.")
BUILD_PATH_HINTS = (
    "Cargo.toml", "Cargo.lock", "package.json", "package-lock.json",
    "pyproject.toml", "setup.py", "requirements", "tsconfig",
    "build.rs", "Makefile", "CMakeLists",
)
CI_PATH_HINTS = (".github/workflows", "ci/", "docker", ".gitlab")


def infer_stratum_from_files(files: list[str], pattern_hint: str | None) -> str | None:
    """Stratum priority: pattern-derived first, then file-based.

    File-based heuristic looks at *what kind of files the fix changed*:
      - any test-suite path  -> "test"
      - build manifests       -> "compile"
      - .github/.gitlab/ci    -> "ci"
      - otherwise             -> "runtime"

    If `pattern_hint` already tagged a stratum (e.g. compile via error[E0599])
    we trust it. The file heuristic only fills in the blanks the pattern
    couldn't decide.
    """
    if pattern_hint and pattern_hint != "runtime":
        return pattern_hint

    has_test = any(any(h in f for h in TEST_PATH_HINTS) for f in files)
    has_build = any(any(h in f for h in BUILD_PATH_HINTS) for f in files)
    has_ci = any(any(h in f for h in CI_PATH_HINTS) for f in files)

    # Prefer the most specific bucket. A test-edit commit is a test fix even
    # if it also touched build files (the actual fix lives in the test
    # change, the manifest tweak is collateral).
    if has_test:
        return "test"
    if has_build:
        return "compile"
    if has_ci:
        return "ci"
    return pattern_hint or "runtime"


def extract_error(body: str) -> tuple[str | None, str | None]:
    """Find the first matching error signature. Returns (error, pattern_id)."""
    for name, pat in ERROR_PATTERNS:
        m = pat.search(body)
        if m:
            return m.group(1).strip(), name
    return None, None


def mine_repo(repo: Path, max_pairs: int) -> list[dict]:
    """Walk a repo and produce up to max_pairs (error, fix) records."""
    out: list[dict] = []
    subjects = commit_subjects(repo)
    for line in subjects:
        if not line:
            continue
        sha, subject = line.split(" ", 1)
        if not FIX_PATTERN.match(subject):
            continue

        body = commit_body(repo, sha)
        error, pattern_id = extract_error(body)
        if not error:
            continue

        files = commit_files(repo, sha)
        lang_from_files = infer_language(files)
        if pattern_id and pattern_id in STRATUM_HINT:
            lang_hint, stratum_hint = STRATUM_HINT[pattern_id]
            lang = lang_hint or lang_from_files
        else:
            lang = lang_from_files
            stratum_hint = None

        # Promote stratum off the file-touch signal so we don't dump every fix
        # into "runtime" just because the body said "broke".
        stratum = infer_stratum_from_files(files, stratum_hint)

        if not lang:
            continue

        out.append({
            "error": error,
            "fix": subject.strip(),
            "language": lang,
            "stratum": stratum,
            "id": f"{repo.name}@{sha[:10]}",
            "context": "",
        })
        if len(out) >= max_pairs:
            break
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("repos", nargs="+", help="Local git repo paths to mine.")
    ap.add_argument("--output", default="procedural-dataset.jsonl",
                    help="Output JSONL path.")
    ap.add_argument("--max-per-repo", type=int, default=30,
                    help="Maximum pairs to pull from each repo.")
    args = ap.parse_args()

    all_records: list[dict] = []
    for repo_str in args.repos:
        repo = Path(repo_str).expanduser().resolve()
        if not (repo / ".git").exists():
            print(f"  skip (not a git repo): {repo}", file=sys.stderr)
            continue
        records = mine_repo(repo, args.max_per_repo)
        print(f"  {repo.name}: {len(records)} pairs", file=sys.stderr)
        all_records.extend(records)

    out_path = Path(args.output)
    out_path.write_text("\n".join(json.dumps(r) for r in all_records) + "\n")
    print(f"  wrote {len(all_records)} records → {out_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
