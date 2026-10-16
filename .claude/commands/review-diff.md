---
description: Run the code-reviewer agent against the current uncommitted diff (or against a specified range) and print findings.
---

Invoked as `/review-diff` (reviews uncommitted changes in the working tree) or `/review-diff <range>` where `<range>` is any `git diff` argument, e.g. `/review-diff main..HEAD` or `/review-diff HEAD~3`.

## What to do

1. **Determine the diff range.** If the user passed an argument, use it as the range. Otherwise default to `HEAD` (uncommitted staged + unstaged changes). If the working tree is clean and no range was given, tell the user there's nothing to review and stop — do not spawn the reviewer.

2. **Capture the diff and a file list:**
   - Run `git diff --stat <range>` to show the user a one-line summary of what's being reviewed.
   - Run `git diff <range>` and capture the full diff. If it's over ~2000 lines, warn the user and ask whether to continue (a huge diff will flood the agent's context and produce shallow review).
   - Run `git diff --name-only <range>` so the agent has a precise file list.

3. **Spawn the `code-reviewer` agent** via the Agent tool with `subagent_type: "code-reviewer"`. Pass a self-contained prompt:
   - **Context sentence** — what the user asked you to review and why (if you know from the current conversation).
   - **File list** — the output of `git diff --name-only`.
   - **The full diff** — pasted inline.
   - **One sentence of guidance** if the change has a specific focus the reviewer should weight higher (e.g. "this touches the plane SQL layer, watch for the column-alias contract").

4. **Relay the agent's verdict.** The code-reviewer returns a structured findings list and a SHIP/REVISE/BLOCK verdict. Surface it verbatim to the user — don't paraphrase. If the verdict is SHIP, also say "proceeding — no blockers"; if it's REVISE or BLOCK, stop and wait for the user to decide.

5. **Do NOT apply fixes yourself.** The reviewer's job is to identify issues; applying them is the user's decision. If the user asks you to fix something the reviewer flagged, that's a separate instruction on a new turn.

## When to use this

- Before any commit over ~100 lines.
- Before any commit touching more than one file in `crates/`.
- Before any commit touching `tyto/src/plane/**` (the highest-risk area — column-alias contract).
- Before any commit with significant CSS changes (shorthand-vs-longhand bugs are easy to miss).

## When NOT to use this

- Single-line fixes. Overhead exceeds the expected value.
- Commits where the user has explicitly said "don't bother reviewing, I checked it".
- Documentation-only changes inside `vault/` or `memory/` — no code to break.
- Style-only CSS tweaks under ~20 lines.

## Output format (what the user sees)

```
Reviewing <range> — N files, +M / -K lines

[agent output verbatim]

Verdict: SHIP | REVISE | BLOCK
```
