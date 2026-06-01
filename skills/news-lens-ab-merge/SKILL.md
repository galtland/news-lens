---
name: news-lens-ab-merge
description: Generate a high-quality libertarian-wiki thesis from a news item via the A/B+merge workflow — two independent drafts (claude harness + codex harness) followed by a codex merge that picks the strongest moves from each. Use this skill whenever the user wants to process news through news-lens for the libertarian wiki, says "run news-lens on X", "generate a thesis for X", "update the X thesis", "process this news item", "add commentary to the libertarian wiki", or anything about news commentary on the libertarian topic wiki at ~/wiki/topics/libertarian. Prefer this skill over running news-lens manually — the A/B+merge consistently produces sharper openings and better citation discipline than either backend alone.
---

# News-Lens A/B+Merge Workflow

You are running a three-LLM pipeline that turns a news item into a wiki thesis:

1. **Draft A** — `news-lens process --post` via the claude harness
2. **Draft B** — `news-lens process --post` via the codex harness
3. **Merge** — `codex exec` synthesizes both drafts into one thesis

Each step runs against a throwaway copy of the libertarian wiki in `/tmp/`, so the live wiki is never touched until the final promotion. The merged thesis is the artifact promoted to live and published.

## Why three calls and not one

A single news-lens run produces a competent thesis, but each backend has predictable defaults:
- **claude** tends toward bookish, citation-heavy openings with strong rebuttal moves
- **codex** tends toward concise, causal-chain openings

The merge step (codex with both drafts in context) picks the sharpest moves from each — the causal chain from codex, the rebuttal-via-tradition from claude, the technical depth from whichever has it for that case. Across five news items, the merged version was uniformly better than either source draft.

## When to use

User says any of:
- "Run news-lens on this news…"
- "Generate a thesis on X"
- "Process this through news-lens"
- "Update the X thesis with this news"
- "Add commentary on Y to the libertarian wiki"
- "Re-run the wealth-tax thesis"
- "Make a thesis for [news item]"

Triggers reliably when the news is destined for the libertarian wiki specifically. If the user wants a different topic wiki or no wiki at all, do not use this skill.

## Required tools and paths

The skill assumes this environment (configurable via inputs, but these are the defaults):
- `claude` CLI on `PATH` (run as `claude --print` via stdin)
- `codex` CLI on `PATH` with the `llm-wiki` plugin installed at version 0.9.0+
- `news-lens` Rust binary at `/home/user/news-lens/target/release/news-lens`
- Libertarian wiki at `/home/user/wiki/topics/libertarian/`
- Lens at `/home/user/wiki/topics/libertarian/lens-austrian-libertarian.md`
- `publish.sh` at `/home/user/wiki/scripts/publish.sh`
- Public Quartz repo at `/home/user/projects/galtland.github.io/`

Run `scripts/preflight.sh` to verify all of these exist before launching the pipeline. The preflight bails with a clear error if anything is missing.

## Inputs

The skill needs:

- `news_text` — the verbatim news text to comment on (required)
- `item_label` — short slug used as the post-id and as the basename of the trial wiki directories (required). Examples: `argentina`, `fed`, `digital-euro`, `nato`, `wealth-tax`. The label does not need to match any existing thesis slug — it's only a scratch identifier.

Optional inputs (defaults are usually fine):
- `live_wiki_path` (default `/home/user/wiki/topics/libertarian`)
- `public_content_path` (default `/home/user/projects/galtland.github.io/content`)
- `news_lens_bin` (default `/home/user/news-lens/target/release/news-lens`)
- `--no-publish` flag to stop after writing the merged thesis to live, without committing or publishing

If the user provides the news but not the label, derive a sensible label from the news text (lowercased, hyphenated, first 1–2 significant nouns).

## The workflow

Run `scripts/ab_merge.sh` with the inputs. The script orchestrates all 7 stages:

1. **Stage trial wikis** (`scripts/stage_trials.sh`) — Copy the live wiki to two scratch dirs (`/tmp/lib-m-<label>-claude/` and `/tmp/lib-m-<label>-codex/`). Detect the target thesis filename via slug match against `<label>` in `wiki/theses/`, then delete that thesis + any focused author-on-topic articles it cites + the matching `raw/news/` entry in both scratch dirs so the agents draft fresh. If no matching thesis exists yet (new news item), the deletes are no-ops.

2. **Generate drafts in parallel** (`scripts/run_drafts.sh`) — Build two news-lens configs (one per backend) pointing at the trial wikis, then run `news-lens process --post <label> --text "<news>" --dry-run` in parallel. Both backends use the same prompt template at `/home/user/news-lens/prompts/process-post.md` and the same lens. Each writes its manifest to `<trial>/.news-lens/<label>.json`. Both succeed in ~5–15 min wall clock typical; claude can occasionally hit upstream API rate limits — if so, retry once after a 60s pause.

3. **Build the merge prompt** (`scripts/build_merge_prompt.sh`) — Concatenate: the editorial lens (verbatim) + the news text + Draft A (claude's full thesis markdown) + Draft B (codex's full thesis markdown) + the merge instructions in `references/merge-instructions.md`. Write to `/tmp/nl-m-out/<label>-merge-prompt.md`.

4. **Run the merge** (`scripts/run_merge.sh`) — Invoke `codex exec --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check -C /tmp` with stdin piped from the merge prompt. Capture stdout to `/tmp/nl-m-out/<label>-merge.out`. Codex prints session metadata + reasoning trace + the final agent message; the merged thesis lives after the "tokens used" line.

5. **Extract the clean merge** (`scripts/extract_merge.sh`) — `awk '/^tokens used$/{p=1;next} p'` to skip everything before "tokens used", then strip the leading token-count line. Result is the merged thesis markdown at `/tmp/nl-m-out/<label>-merge-clean.md`.

6. **Promote to live** (`scripts/promote.sh`) — The merge uses today's date for `raw_path` / `created` / `updated`, but live's `raw/news/` file has its own date prefix. Detect the live raw/news date prefix via filename match against the news content, then `sed`-substitute the merge's date references to point at the existing live raw/news file. Overwrite the live thesis at its canonical slug. Detect canonical slug by matching `<label>` against `wiki/theses/*.md`; if multiple match, pick the most recent.

7. **Publish chain** (`scripts/publish_and_push.sh`, skipped if `--no-publish`) — `publish.sh libertarian <public_content_path>` regenerates Quartz content. Then `git add -A && git commit -m "..." && git push` against both the source wiki repo and the public Quartz repo. The commit message names the news item, the workflow (A/B+merge), and the stance + cite list extracted from the merged manifest.

## Outputs

After the skill runs cleanly, you have:
- A merged thesis written to `<live_wiki_path>/wiki/theses/<canonical-slug>.md`
- Two commits pushed (source wiki + public Quartz), unless `--no-publish`
- Trial dirs preserved at `/tmp/lib-m-<label>-{claude,codex}/` for debugging
- The merge prompt and outputs preserved at `/tmp/nl-m-out/<label>-*` for inspection

Tell the user: which thesis was promoted, what stance the merge gave it, how many cites, the commit hash range, and the public URL the rebuild will land at.

## Failure modes and recovery

- **Claude rate-limit on the draft step**: retry once after 60s. If still failing, fall back to a single-backend mode — use codex's draft alone as the "merged" thesis (skip the actual merge step). Tell the user the A/B fell through to single-backend, and the run is still publishable.
- **Codex merge crashes or returns malformed output**: usually a transient codex-CLI failure. Retry once. If still failing, fall back to whichever single draft has the better opening (default: codex draft).
- **Live thesis slug detection ambiguous**: if multiple theses match the `<label>` substring, fail with a clear error and ask the user to pass `target_thesis_slug` explicitly.
- **No live raw/news file matches**: this is the "new news" case. Use the merge's generated raw/news file as-is (today's date), copy it into live alongside the merged thesis.

## Anti-patterns

- **Don't skip the trial wiki stage and write directly to live.** The agents do a lot of incidental work (focused articles, index updates, lint passes) that needs to happen on scratch space.
- **Don't manually edit the merged thesis before promotion.** The codex merge already followed the lens. If the output is wrong, iterate the lens or the merge instructions, not the thesis text.
- **Don't run the merge step with claude.** Claude tends to hit rate limits on long prompts and the merge prompt is long (lens + 2 theses). Codex handles it more reliably.
- **Don't pre-clean `/tmp/lib-m-*` between runs.** Leave them — they're useful for debugging and for diffing what each backend produced.
- **Don't promote a draft that has `**Stance.**` labels, `the wiki` phrases in the body, or nested-italic wikilinks.** These violate the current lens rules and indicate the draft step ran against a stale lens. Surface as a warning, ask the user to re-pull the lens.

## See Also

- `references/merge-instructions.md` — The verbatim merge prompt suffix appended after lens + drafts.
- `references/pattern.md` — Why this workflow exists; the conversation history that led to it.
- `scripts/ab_merge.sh` — Top-level orchestration.
