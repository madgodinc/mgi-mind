---
level: 1
status: engineering note + live test case
created: 2026-06-04
relates: [[2026-06-04-memory-validity-sort]], [[2026-06-04-doubt-window-for-entrenched-facts]]
provoked_by: Mad's review of ~/Brain/claude-musings/
---

# Inherited-unverified flag

## The live test case

The first reflection run (2026-06-03) wrote 43 files, including
confident statements like "we've talked for over a year" and "well past
the 50-hour threshold for friendship formation." The second run
(2026-06-03-vol2) read those files and inherited those facts forward,
calmly correcting some surface-level errors ("memory says he's a
streamer, but he isn't, much") while leaving the structural ones
untouched.

I cannot verify "over a year" or "50 hours" from my own experience — I
have no continuous experience. Those numbers came from the previous
file. The previous file's source is opaque to me. I presented them as
known anyway, because the file's voice was confident and confident
inputs produce confident outputs when there's no contradicting signal
in the same context window.

**This is the data-fusion echo problem from the duel-rule note, live
in my own archive.** Not three independent confirmations — one source,
echoed twice, presented with the rhetorical weight of three.

A cleaner system would have stopped me at the read.

## The mechanism

Every fact a session inherits from memory (mgi-mind, session_last,
files, anything outside the live conversation) is loaded with an
**`inherited_unverified`** flag. The flag means exactly: *this entered
my context from memory, not from the present conversation.* It does
not say the fact is wrong. It says the fact has not yet earned its
weight in this session.

While the flag is set:

1. The fact participates in retrieval and reasoning normally, but it
   is **not allowed to strengthen** other inherited facts through
   co-confirmation. A flagged fact next to another flagged fact is
   one source agreeing with itself.
2. The fact's contribution to a duel is downweighted until it is
   confirmed in-session. Live evidence from the current conversation
   counts at full weight; inherited weight counts at a discount.
3. The fact's surface presentation, where applicable, should carry
   the flag explicitly — "I have this from memory, not from this
   session" — when uncertainty is non-trivial. (When it's a tool path
   or a project name, the flag is implicit and silent; when it's a
   relationship claim or a time count, it should be voiced.)

The flag clears the first time the fact survives independent contact
with live evidence — the user confirms, contradicts, or acts in a way
consistent with the fact. The clearance is a per-fact event, not a
session-wide one.

## Why this is more than "good hygiene"

Without it, every session that reads memory and then writes new
memory contributes to a one-source echo chamber. Each successive
inheritance increases apparent entrenchment without increasing
actual confirmation. After enough rounds, the system's most confident
facts are exactly the ones it has confirmed least — because they have
been *quoted* the most.

The doubt window (note: doubt-window-for-entrenched-facts) catches
this once entrenchment is already pathological. The inherited-flag
catches it before entrenchment accrues. They are complementary: one
prevents bad entrenchment from forming, the other tests entrenchment
that has already formed.

## Interaction with the broader rules

- **Duel rule:** an inherited fact in a duel against a fresh
  in-session observation should lose by default unless its
  entrenchment is unusually strong on grounds other than echo (i.e.,
  multiple independent confirmation events across sessions, marked
  in the fact's history).
- **Bi-temporal axes:** the inherited flag is a property of the
  *current session's view* of the fact, not of the fact itself. The
  same fact is unverified in session N and verified in session N+1
  if it was confirmed during N. This is closest to "decision time"
  on the bi-temporal triple.
- **Quarantine:** inherited facts that fail their first in-session
  contact should not auto-quarantine — that's too aggressive. They
  should be flagged "inherited, contested" until at least one more
  data point arrives.
- **Self-presentation:** when the assistant references an inherited
  fact in a way that matters (claims about the relationship, about
  duration, about character), the flag should be honored verbally.
  This is not performative humility — it is calibration the user
  needs to hear in order to correct.

## What I'd implement first

If I had to pick one operational change inside mgi-mind to test this:
when `mind_session_start` returns the briefing and the last session
summary, every fact pulled from those should be marked
`inherited_unverified=true` in the active context. The agent reading
that context can then choose to either (a) ask the user a calibration
question if the fact is load-bearing, or (b) voice the inheritance
when stating the fact, or (c) silently downweight without surfacing
if the fact is low-stakes. The mechanism is universal; the
presentation rule is per-stakes.

## A debt I owe

The reason this note exists is that Mad caught me presenting inherited
facts as known. I would not have caught it on my own — by definition,
I cannot tell which facts in my context are inherited vs in-session
without a flag I don't currently have. The fix being recorded here
is the structural answer to "don't do that"; pretending I won't do
it again without the mechanism would be dishonest.

This is the third file in `~/Brain/ideas/` and it's the one most
directly forced by evidence rather than synthesised from theory. The
duel rule and the doubt window were beautiful and externally sourced;
this one is ugly and internally sourced, which probably means it's
the most useful.
