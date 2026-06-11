# mgi-mind terminal demo — regenerate with:  vhs docs/demo.tape
# (needs the bundled Qdrant up: `mgimind serve` in another shell.)
#
# Records a real "store a fact -> recall it by MEANING -> it's all local"
# session. The query shares no words with the stored fact on purpose:
# semantic search matches intent, not keywords. An assistant does the same
# thing on its own over MCP (mind_search); the CLI makes it visible in one
# frame. Scoped to a throwaway `demo` library so it never touches real memory.
#
# Output: docs/demo.gif (committed; small enough to inline in the README).

Output docs/demo.gif

Set Shell "bash"
Set FontSize 19
Set Width 1200
Set Height 660
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms
Set PlaybackSpeed 1.0

# --- prep (hidden): release binary on PATH, fresh demo library ---
Hide
Type "export PATH=$PWD/target/release:$PATH" Enter
Type "mgimind create demo >/dev/null 2>&1; clear" Enter
Show

# 1. store a fact, in plain words
Type "mgimind add demo 'Deploy server is 10.0.0.5:8080, SSH as deploy@, key in the vault'"
Sleep 600ms
Enter
Sleep 1.8s

# 2. ask for it later, worded nothing like what we stored
Type "mgimind search 'where do I push releases to again?' --library demo"
Sleep 600ms
Enter
Sleep 3s

# 3. it never left the machine
Type "# embeddings, search, rerank, vault — all local. no cloud, no API key."
Sleep 600ms
Enter
Sleep 2.2s

# cleanup (hidden)
Hide
Type "mgimind drop demo >/dev/null 2>&1; clear" Enter
Show
Sleep 400ms
