---
name: Bug report
about: Something crashes, returns wrong data, or wedges
labels: bug
---

## What happened

<!-- One paragraph. What you did, what you expected, what you got. -->

## Reproduction

<!-- The smallest sequence of commands that triggers the bug. If a fixture
     file is needed, attach it or paste a few lines that show the shape. -->

```sh
# example
mgimind init
mgimind add projects "..."
mgimind search "..."
```

## Environment

- mgi-mind version: <!-- `mgimind --version` -->
- OS: <!-- Linux distribution, kernel, etc. -->
- Install mode: <!-- `mgimind config install-mode` output -->
- Extractor feature: <!-- on/off; if on, version from `mgimind extractor info` -->

## Logs / output

<!-- Output from the failing command, or stderr from `mgimind serve` if the
     bug surfaces over MCP. Long logs go behind a <details> tag. -->

```
<paste here>
```

## Anything else

<!-- Optional context: when did this start happening, did a recent release
     change behaviour, is there a workaround. -->
