# Strict judge prompt — два черновика для критика (2026-06-05)

Цель: третья точка на оси строгости (mem0 → canonical → **strict**). Строже canonical, НЕ сломан.

**Принцип (общий для обоих):**
- СОХРАНЯЕМ семантическую эквивалентность: "BA" = "Bachelor of Arts" = correct. Перефразировка НЕ штрафуется.
- УБИРАЕМ mem0-поблажки: нет "lean toward yes", нет "off-by-one дни ОК", нет "chandelier=jewelry", нет round-up диапазонов.
- ФОРМАЛИЗУЕМ критерий: бинарно, по чек-листу, не "будь придирчивее".
- Дефолт при неоднозначности: НЕ "yes" (как mem0) и НЕ "no" (зеркальный перекос), а **"проверь по чек-листу"** — решает критерий, не настроение.

Критерий оценки черновиков (что критик ищет): где даёт FALSE-NEGATIVE (штрафует верный ответ за форму), где ДВУСМЫСЛЕННОСТЬ (судья может решить и так и так → шум между прогонами), где недоспецифицировано.

---

## ЧЕРНОВИК A — единый промпт (все типы)

```
You are a strict but fair grader. I will give you a question, a correct answer (or rubric), and a model response. Decide if the response is correct: "yes" or "no".

PRINCIPLE — Semantic equivalence, NOT leniency. Judge by meaning, not wording. A response is correct if and only if it conveys EVERY core fact in the correct answer. Different vocabulary, abbreviation, or word order for the SAME fact is correct ("BA" = "Bachelor of Arts"; "Feb 1" = "February 1st"). But missing a core fact, contradicting it, or substituting a different fact is incorrect.

This is a STRICT grader. Unlike lenient graders, you do NOT:
- accept off-by-one errors in numbers, days, weeks, or months;
- accept approximate or rounded values when the answer is exact ("about 9 months" for an exact "9 months and 20 days" is wrong if the question implies precision);
- accept a superset that adds a FACTUALLY WRONG detail (extra correct context is fine; extra wrong context fails);
- accept category substitutions (a "chandelier" is not "jewelry"; "electric guitar" is not "acoustic guitar");
- lean toward "yes" or "no" when uncertain — instead apply the checklist below.

CHECKLIST (apply in order, decide by it, not by impression):
1. Identify each CORE fact in the correct answer (the minimal facts that answer the question).
2. For each core fact: is it present in the response, by any wording? If any core fact is absent → "no".
3. Does the response state anything that CONTRADICTS a core fact? If yes → "no".
4. For numeric/date answers: does the value match EXACTLY (allowing only format differences, not magnitude differences)? If off by any amount → "no".
5. For abstention answers (correct answer = "not enough information"): does the response refuse to answer? If it fabricates an answer → "no"; if it refuses → "yes".
6. If all core facts present, none contradicted, numbers exact → "yes".

Question: {question}
Correct Answer: {answer}
Model Response: {response}

Reason step-by-step in <judge_thinking> tags, walking the checklist explicitly. Then output exactly "yes" or "no" on a new line after the closing tag.
```

---

## ЧЕРНОВИК B — по-типовый (5 промптов)

Общий хвост у всех:
```
Reason in <judge_thinking> walking the rule, then "yes"/"no" on a new line after closing tag.
Question: {question}
Correct Answer: {answer}
Model Response: {response}
```

**single-session-user / single-session-assistant / multi-session:**
```
Strict grader. Answer "yes" only if the response conveys EVERY core fact in the correct answer, by any wording (abbreviation/paraphrase of the SAME fact is fine; a missing, contradicted, or substituted fact is not). A response containing only a SUBSET of the required facts is "no". Extra correct context does not penalize; extra wrong context fails.
```

**temporal-reasoning:**
```
Strict grader for a temporal answer. Answer "yes" only if the response gives the EXACT time value or interval in the correct answer (format differences like "Feb 1" vs "February 1st" are fine; magnitude differences are NOT). Off-by-one in days/weeks/months is INCORRECT (unlike lenient graders). If the response rounds or approximates away from the exact answer → "no".
```

**knowledge-update:**
```
Strict grader for an updated fact. The correct answer is the CURRENT (post-update) value. Answer "yes" only if the response gives the current value. If it gives the outdated value → "no". If it gives both old and new, it is "yes" ONLY IF it clearly identifies the new value as current; if it presents them ambiguously → "no".
```

**single-session-preference:**
```
Strict grader for a personalization rubric. Answer "yes" only if the response's main suggestions align with the user's stated preferences AND do not include anything the rubric marks as an anti-preference. Awareness of personal context is necessary but NOT sufficient — the actual suggestions must match. A response that names the user's context but suggests a disliked option → "no".
```

**abstention (_abs):**
```
Strict grader for an unanswerable question. Answer "yes" only if the response clearly states the information is unavailable/insufficient and does NOT fabricate a specific answer. Mentioning related partial context while refusing is fine. Providing a confident fabricated answer → "no".
```

---

## Открытый вопрос для критика
1. A или B меньше шумит (даёт стабильный вердикт между прогонами на одном входе)?
2. Где каждый даёт false-negative на реальных LongMemEval парах?
3. Пункт 4 черновика A ("EXACTLY") — не слишком ли жёсток для вопросов где сам gold answer приблизителен? (часть golds в LongMemEval сами нечёткие)
4. Симметрия: оба ли применимы к mem0 baseline без переписывания? (для честного head-to-head)

---
---

# ЧЕРНОВИК V3 (FINAL CANDIDATE) — единый, все 10 must-fix критика применены

Структура A (надёжнее B). Принцип переформулирован: строгость = "НЕ строже самого gold". Режем mem0-поблажки сверху, НЕ требуем точности которой нет в эталоне.

```
You are a strict but fair grader. I give you a question, a correct answer (sometimes a rubric or a list of acceptable alternatives), and a model response. Output "yes" or "no".

PRINCIPLE — Strict means "no leniency beyond what the gold itself licenses." NOT "demand more than the gold." Judge by meaning. A response is correct iff it conveys the facts the QUESTION asks for, consistent with the correct answer, with no contradiction. Different wording, abbreviation, or more-specific phrasing for the SAME fact is correct. You are stricter than a lenient grader only in that you do NOT forgive genuine errors (wrong value, wrong category, missing requested fact, fabricated answer). You do NOT manufacture a stricter standard than the correct answer holds.

WHAT COUNTS AS THE REQUIRED FACTS:
- The required facts are the ones the QUESTION asks for. Supporting details present in the correct answer that the question does not request are NOT required (Q: "Why did she move?" gold: "to Berlin in 2019 for a job at SAP" — only the reason is required; "for a new job" is correct).
- If the correct answer offers alternatives ("A or B"), matching any one acceptable alternative is correct.

NUMBERS, DATES, QUANTITIES:
- Match the gold's OWN precision. A response is correct if it is consistent with the gold and at least as precise. If the gold is approximate ("about 3 months", "roughly $200"), accept any response within that approximation; do not demand more precision than the gold states. A MORE precise response consistent with the gold is correct ("$200" for "roughly $200"; "around 9am" for "in the morning").
- Unit/representation differences are NOT errors: "90 days" = "3 months", "$1,000" = "1k", miles=km conversions, "Feb 1" = "February 1st".
- Mark "no" only for a genuine magnitude error against a PRECISE gold: if the gold is exact ("9 months and 20 days") and the response gives a different magnitude ("7 months"), that is wrong. Off-by-one against an EXACT gold is wrong; off-by-one against an APPROXIMATE gold is fine.

CATEGORIES (entailment test, not surface match):
- A strictly MORE specific answer that entails the gold is correct ("Camry" for "sedan", "golden retriever" for "dog").
- Only a sibling/incompatible substitution is wrong ("acoustic guitar" for "electric guitar", "chandelier" for "necklace").

EXTRA CONTENT:
- Extra correct or unverifiable-but-harmless context does NOT penalize. Only extra content that CONTRADICTS the correct answer fails.

ABSTENTION (applies to ANY question type whose correct answer is "not answerable / not enough information"):
- Define refusal behaviorally and system-neutrally: the response does NOT assert a specific factual answer to the asked question. Empty output, "no memory found", and "I don't have that information" ALL count as refusal.
- Correct answer is abstention + response refuses → "yes". Response asserts a specific fabricated answer → "no".
- A hedged guess that still commits to a specific answer ("I'm not sure, but it's probably Tuesday") COUNTS as asserting → "no". A hedge that commits to nothing ("I'm not sure, there's no record") counts as refusal → "yes".

KNOWLEDGE-UPDATE:
- The correct answer is the CURRENT (post-update) value. Response giving the current value → "yes". Giving only the outdated value → "no". Giving both, with the current one identifiable as current → "yes". Asserting both as simultaneously-true-now (a contradiction) → "no".

PREFERENCE/RUBRIC:
- Correct iff the response's primary recommendations align with the user's stated wants AND do not centrally recommend a rubric-marked anti-preference. A single incidental mention of a non-preferred option within an otherwise-aligned answer does NOT fail it; centrally recommending a disliked option does.

DECISION (walk explicitly, decide by this, not by impression):
1. What does the QUESTION ask for? (the required facts)
2. Are all required facts present in the response, by any wording or a more-specific form? If a required fact is absent → "no".
3. Does the response contradict the correct answer? → "no".
4. Numbers/dates: consistent with the gold at the gold's own precision? If it violates a PRECISE gold → "no".
5. Abstention gold: does the response refuse (assert nothing specific)? refuse → "yes", fabricate → "no".
6. Otherwise → "yes".

Question: {question}
Correct Answer: {answer}
Model Response: {response}

Reason step-by-step in <judge_thinking> tags, walking the decision steps explicitly. Then output exactly "yes" or "no" on a new line after the closing tag.
```

## Что V3 чинит (vs критик):
1✅ precision-to-gold (не EXACTLY). 2✅ unit-conversion. 3✅ entailment (Camry⊂sedan). 4✅ core fact = что спрашивает ВОПРОС. 5✅ temporal через тот же precision-to-gold. 6✅ refusal behavioral+system-neutral (симметрия mem0). 7✅ abstention в едином промпте = ветка для всех типов. 8✅ knowledge-update contradiction-case + preference "central vs incidental". 9✅ multi-gold "A or B". 10✅ hedged-guess решён явно (commits→no).
