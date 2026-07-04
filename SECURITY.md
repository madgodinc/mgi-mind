# Security policy

mgi-mind is a local-first memory layer. By design nothing leaves the
box — no cloud account, no API key, no telemetry. That's also the
threat model: a local attacker reading memory contents from disk,
intercepting MCP traffic on localhost, or feeding malicious input
through the extractor.

## Supported versions

Security fixes go to the latest `1.x` minor and to `main`. Older
versions are not patched.

| Version | Supported |
|---------|-----------|
| 2.1.x | ✅ |
| 2.0.x | ✅ (one cycle) |
| ≤ 1.x | ❌ |

The version drops out of support when the next minor ships and the
prior minor has had at least one cycle of grace. If you have a 1.x-or-earlier vault
that needs migrating, open an Issue — there's a path even though
the build itself is unsupported.

## Reporting

Send vulnerability reports to **the email in the
[@madgodinc GitHub profile](https://github.com/madgodinc)**.
That's the only private channel — DMs on social networks are not
monitored for security.

Please include:

- mgi-mind version (`mgimind --version`).
- A reproduction. The smallest sequence of CLI commands or MCP calls
  that surfaces the issue.
- What you observed and what you expected.
- Whether you've reported it elsewhere or intend to.

I'll acknowledge within 5 business days. Fixes go through a private
branch; a public Security Advisory + patched release lands when the
fix is ready.

If you want to coordinate disclosure (CVE assignment, embargo dates),
say so in the initial email.

## Scope

In scope:

- **Vault** — secrets stored via `mgimind vault store`. Encryption,
  key derivation, on-disk format, key reuse, terminal-echo prevention.
- **Extractor subprocess** — llama-server binary, prompt injection
  via memory content, command injection via filenames or model
  variant strings.
- **MCP stdio surface** — JSON-RPC parsing, prompt-injection escape
  via tool arguments, audit-log evasion.
- **Qdrant binary** — the bundled `qdrant` is downloaded with a
  pinned SHA-256 checksum (audit #6). If a download path bypasses
  the integrity check, that is in scope.
- **Audit log** — append-only contract, ability to tamper with
  past entries, ability to write false entries.

Out of scope:

- **DoS via large inputs.** `mgimind ingest <100MB-file>` will be
  slow; that's a quality issue, not a vulnerability.
- **Reading memory contents with shell access.** mgi-mind data lives
  in `~/mgimind/` with default user file modes. A local attacker
  with your shell is past the security boundary.
- **Browser-based attacks against the viewer.** The viewer is
  `mgimind viewer` — a localhost-only HTTP server with no auth by
  design (local-first). Cross-site scripting via memory content
  injected into the viewer is interesting only if it escalates;
  please report if so.
- **MCP client misbehaviour.** If Claude Code (or any MCP client)
  passes malicious arguments to a tool, that's a client-side issue.
  mgi-mind validates schema at the boundary; report failures of
  that validation.

## Known limitations the project does not consider vulnerabilities

- The extractor receives memory content as model input. Prompt
  injection is **expected** and handled by the triple-backtick fence
  + sanitisation in `src/extractor.rs::build_prompt`. If you can
  bypass the fence to make the extractor write to the knowledge
  graph through a path the user did not consent to, that IS a
  vulnerability. If you make the extractor emit garbage triples
  the user has to delete, that's a quality issue.
- The audit log is append-only on the filesystem, not
  cryptographically chained. A local attacker with file system
  access can tamper. A hash-chain (`mgimind audit verify`) is scoped for
  v2.4 — see ROADMAP.md.
- No rate-limiting on MCP tool calls. A malicious client can
  exhaust the background loop's BACKGROUND_PER_TICK_CAP. The §10
  q5 guarantees mean foreground MCP latency stays bounded; the
  background pass falls behind. Not a security issue per se.

## Past disclosures

None yet. As of v2.1.1 no security advisories have been published.
This file exists so future researchers have a path.
