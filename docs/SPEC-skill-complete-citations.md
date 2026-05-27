# news-lens-ab-merge skill — Complete Citation Coverage

> Self-contained implementation task for rb-lite. Modifies the
> `news-lens-ab-merge` skill so that every `[[wikilink]]` in the
> produced thesis resolves to a file in the live wiki by the time the
> skill exits.
>
> Driven by wiki repo issue
> https://github.com/galtland/galtland-wiki-index/issues/11 and the design decision
> recorded as a comment there.

Status: **ready for implementation**, revision 1.

---

## 0. Goal

After the skill's `[promote]` step finishes, every `[[target]]`
wikilink in the produced thesis body MUST resolve to an existing
markdown file under `$LIVE_WIKI/wiki/`. If a cited concept or
reference doesn't yet exist, the skill drafts a proper focused
article for it (full text, not a stub) using the same editorial
lens and writes it to the appropriate `wiki/concepts/` or
`wiki/references/` directory.

If any wikilink can't be filled after the drafting batch, the skill
fails the run with a clear error listing the unfilled targets.

---

## 1. Context

### What the skill does today

`skills/news-lens-ab-merge/scripts/ab_merge.sh`:

1. Loads the live wiki (`$LIVE_WIKI`, default
   `/home/user/wiki/topics/libertarian`).
2. Runs claude + codex backends in parallel against the news input.
3. If both succeed, codex merges them.
4. Promotes the chosen draft to
   `$LIVE_WIKI/wiki/theses/<slug>.md`.
5. **Lazily** copies ONE focused-concept stub from the trial wiki
   to live if the merged thesis introduced one (see "[promote] new
   focused article" log line).
6. Writes a manifest to
   `$LIVE_WIKI/.news-lens/<label>.json`.

The problem: step 5 only handles a single new focused article, and
even then ships a stub. Theses routinely cite 4–8 wikilinks, and any
that don't already exist in the wiki break the downstream pre-merge
lint (which requires every wikilink to resolve).

Observed on wiki repo PR #10 (now closed): thesis cited
`[[mises-on-sound-money]]` and `[[hoppe-on-fiat-money-devolution]]`,
neither was created, lint produced 7 unresolved-wikilink errors.

### What needs to change

Replace the "lazy single-stub" logic in the promote step with a
batch that:

1. Parses every `[[wikilink]]` target from the merged thesis body.
2. Filters to targets that don't already resolve in
   `$LIVE_WIKI/wiki/**/*.md`.
3. For each remaining target, drafts a full focused article via the
   same backend (claude/codex) using a focused prompt + the lens
   file + a snapshot of the existing wiki's article inventory.
4. Writes each new article under
   `$LIVE_WIKI/wiki/concepts/<target>.md` or
   `$LIVE_WIKI/wiki/references/<target>.md` based on
   classification.
5. Re-verifies all wikilinks resolve. If not, fails loudly.

---

## 2. Scope

### In scope (modify)

- `skills/news-lens-ab-merge/scripts/ab_merge.sh` — specifically
  the promote step (around the existing "[promote] new focused
  article" handling). Replace the lazy single-stub logic with the
  batch described in §3.

### Out of scope (do NOT modify)

- `prompts/process-post.md`, any other script in the news-lens
  repo, any Rust code.
- The matching skill copy at
  `$HOME/.claude/skills/news-lens-ab-merge/` (operator will sync
  after merge — out of scope here).
- The wiki repo and its `newsroom/`, `.github/`, or
  `topics/libertarian/` content.
- Schema of the existing manifest at
  `$LIVE_WIKI/.news-lens/<label>.json` — keep it backwards
  compatible. Adding optional fields (e.g., `new_articles`) is OK;
  removing or renaming existing fields is not.

---

## 3. Hard constraints

1. **Do not run `rb-lite` itself, send signals to your process
   tree, or otherwise interfere with the surrounding
   orchestration.**
2. **Bash only.** No Python, no Ruby. Use `jq`, `sed`, `awk`,
   `grep`, `find`, `claude`, `codex`. These are already on the
   runner's PATH (or in the news-lens dev shell).
3. **`set -euo pipefail` stays at the top of the script** — don't
   loosen error handling.
4. **No new external dependencies.**
5. **Fail loud on incomplete coverage.** If any wikilink target
   can't be drafted (drafting call exhausted retries, classifier
   couldn't decide, etc.), the skill must exit non-zero with a
   message naming exactly which targets couldn't be filled. Don't
   ship a partial thesis. Don't silently drop the unfilled
   wikilinks from the thesis body.
6. **Cap drafting at 10 new articles per thesis** (configurable
   via `NL_MAX_NEW_ARTICLES`, default 10). A thesis that needs
   more than 10 new articles is probably a sign the wiki coverage
   is too thin for this news item — fail with a clear message
   pointing at issue #11 for context.
7. **Don't re-draft articles that already exist.** Idempotency:
   running the skill twice on the same input must not double-write
   or rewrite existing focused articles.
8. **Per-article drafting failures roll up to skill failure**, not
   silent skip.
9. **Don't modify the thesis body content** during the batch.
   Drafting new articles must not require editing the thesis (its
   wikilinks were chosen by the editorial pass). If a thesis's
   wikilink slug is so malformed that even a draft can't slot in,
   that's a content quality problem to surface in the failure
   message, not paper over.

---

## 4. Detailed implementation

### 4.1 Detect missing wikilinks

After `[promote] wrote $PROMOTE_TARGET`, before writing the final
manifest:

```bash
# Extract every [[target]] from the thesis body. Strip display
# text after | and section after #. Deduplicate.
mapfile -t WIKILINK_TARGETS < <(
  grep -oE '\[\[[^]|#]+' "$PROMOTE_TARGET" \
    | sed 's/^\[\[//' \
    | sort -u
)
```

For each target, check whether a file exists in any of:

- `$LIVE_WIKI/wiki/concepts/<target>.md`
- `$LIVE_WIKI/wiki/references/<target>.md`
- `$LIVE_WIKI/wiki/topics/<target>.md`
- Any other `$LIVE_WIKI/wiki/*/<target>.md`

A `find $LIVE_WIKI/wiki -type f -name "<target>.md"` check is fine.

Targets where a file exists: drop from the list.
Remaining targets: the MISSING set.

### 4.2 Classify each missing target

For each missing target, decide whether it belongs in
`wiki/concepts/` or `wiki/references/`:

- **reference**: target slug is an author name
  (`hans-hermann-hoppe`, `ludwig-von-mises`, `friedrich-a-hayek`,
  `murray-rothbard`, `ron-paul`, `franz-oppenheimer`,
  `friedrich-bastiat`, etc.) OR a book title
  (`human-action`, `americas-great-depression`, `road-to-serfdom`,
  `man-economy-and-state`, `prices-and-production`, `socialism`,
  `the-theory-of-money-and-credit`, etc.). The canonical lists
  live in the existing `$LIVE_WIKI/wiki/references/` directory —
  enumerate filenames once at the start of the batch and treat any
  near-match (full-slug equality) as a hint.
- **concept**: everything else (default).

If classification is ambiguous (e.g., a new author whose slug
matches no existing reference), prefer `concept`. Surface the
choice in the log line so the operator can promote it to
`references/` later if needed.

### 4.3 Draft each missing article

For each missing target, invoke a backend to produce the article
content. Reuse the existing backend infrastructure
(`run_backend` or whatever helper writes the thesis). Backend
choice: same as the thesis (prefer the codex-merged path; fall back
to whichever backend succeeded for the thesis).

Prompt template (write this as a heredoc near the existing thesis
prompts, parameterized by `$TARGET`, `$KIND`,
`$LIVE_WIKI_INVENTORY`, `$LENS_FILE`):

```
You are writing a focused wiki article from the Austrian-libertarian
editorial lens.

Editorial lens (full text):
<contents of $LENS_FILE>

Existing wiki inventory (filenames only, for valid [[wikilink]]
targets in your body):
<list of every .md file in $LIVE_WIKI/wiki/, one per line>

Task: write the article for slug "<TARGET>" classified as a
<KIND> (concept or reference).

The output must be a single markdown file with this exact shape:

---
title: "<reader-facing title — convert the slug to title case>"
volatility: warm
category: <concept|reference>
sources:
  - raw/articles/<one or more sources from the inventory>
created: <today's date YYYY-MM-DD>
updated: <today's date YYYY-MM-DD>
tags: [<3-6 tags>]
aliases: [<1-3 reader-friendly alias names>]
short: "<one-sentence summary of the article's core claim>"
---

# <title>

<body — 150 to 300 words. State the core claim in 1-2 sentences.
Include at least one direct quote from a cited source, formatted
as a markdown blockquote. Link to related wiki articles using
[[wikilink|Display Text]] form, but ONLY to articles already in
the inventory above. End with a short "See Also" bullet list of
2-4 related articles (also from the inventory).>

Constraints:
- Stay under 300 words in the body (excluding frontmatter).
- Every [[wikilink]] in your body MUST be in the inventory list.
- Quote at least one source verbatim.
- Match the tone of the editorial lens.
- Output ONLY the markdown file. No commentary before or after.
```

Backend invocation: similar shape to the existing thesis-drafting
call. Capture stdout as the article content. Validate that:

- The output starts with `---` and contains a closing `---` for the
  frontmatter block.
- The frontmatter has `title:` and `category:` non-empty.
- The body is non-empty and ≤ ~500 words (slack above the 300-word
  target).

If validation fails, retry once. If the retry fails, mark the
target as drafting-failed for the final report.

### 4.4 Write each drafted article

Write to `$LIVE_WIKI/wiki/<concepts|references>/<target>.md`.
Use `mv` of a temp file for atomicity. Don't overwrite existing
files (defensive — should never happen after §4.1's filter).

Log: `[promote] wrote new <kind> article: <path>`

### 4.5 Re-verify coverage

After the batch, re-parse the thesis body for `[[wikilinks]]` and
check that EVERY target now resolves. Apply the same lookup logic
as §4.1.

If anything is still unresolved, fail with:

```
[promote] FATAL: <N> wikilink(s) in the thesis still unresolved
after the citation-completion pass:
  - <target1>
  - <target2>
  ...
Drafting attempts:
  - <target1>: <succeeded|failed: <reason>>
  - <target2>: <succeeded|failed: <reason>>
Cap (NL_MAX_NEW_ARTICLES=$NL_MAX_NEW_ARTICLES) hit: <yes|no>
See wiki repo issue #11 for the design decision.
Exit 1.
```

Skill exits non-zero. The downstream consumer (build-thesis.sh)
sees the failure, posts it to the originating issue, and the
operator knows exactly what to fix (or which slugs to remove from
the thesis manually if they're typos).

### 4.6 Manifest updates

The existing manifest already has `citations` (count) and other
fields. Add one new optional field for visibility:

```json
"new_articles": [
  { "slug": "mises-on-sound-money", "kind": "concept",
    "path": "topics/libertarian/wiki/concepts/mises-on-sound-money.md" },
  { "slug": "hoppe-on-fiat-money-devolution", "kind": "concept",
    "path": "topics/libertarian/wiki/concepts/hoppe-on-fiat-money-devolution.md" }
]
```

Empty list (`[]`) when no new articles were needed. Downstream
consumers may use this list for backlink propagation or PR-summary
generation; it should not be required reading.

---

## 5. Edge cases and clarifications

- **Wikilink in a wiki/concepts/* file we just wrote** — handled
  by §4.3's "inventory list" constraint: the new article's body can
  only link to articles already in the wiki at the moment we
  invoked the drafting call. So a fresh article from this batch
  cannot create more dangling wikilinks. (Order of writes in the
  batch matters less than this constraint.)
- **Author target matching existing reference** — if
  `wiki/references/hans-hermann-hoppe.md` already exists and the
  thesis cites `[[hans-hermann-hoppe]]`, §4.1 finds the file and
  drops it from the missing set. No duplicate creation.
- **Numeric or date-prefixed slugs** like
  `[[2026-05-23-fed-rate-cut]]` — these typically point at raw/news
  files, not wiki articles. The lint currently rejects these too;
  for now, treat them as "not in wiki" and let the cap-or-fail
  message guide the operator to remove that wikilink from the
  thesis manually. (A separate decision on
  raw/news wikilink resolution is in scope of issue #11 option 4 —
  not solved here.)
- **`$LIVE_WIKI/wiki/` glob ambiguity** — if two files match the
  same target slug in different subdirs (concepts/ and topics/),
  treat as resolved. Don't try to dedupe or warn.

---

## 6. Test plan

Add a self-test inside the news-lens repo if one is feasible.
Otherwise, document the manual test in this section.

**Smoke test (manual, document in the commit message):**

1. Run `ab_merge.sh --label test-cite --text "<news text that
   would produce a thesis citing at least one missing concept>"
   --no-publish` against the live wiki.
2. After completion, run `grep -oE '\[\[[^]|#]+' \
   <thesis-path> | sed 's/^\[\[//' | sort -u`.
3. For each target, verify the file exists at one of the
   `$LIVE_WIKI/wiki/*/<target>.md` paths.
4. Check the manifest at `$LIVE_WIKI/.news-lens/test-cite.json`
   for the new optional `new_articles` field.

**Cap behavior:**

1. Set `NL_MAX_NEW_ARTICLES=1` and run the skill against a thesis
   that needs at least 2 new articles.
2. Verify the skill exits non-zero with a message naming the cap.

**Idempotency:**

1. Run the skill once successfully.
2. Run it again on the same input.
3. Verify no second copies of new articles were written.

---

## 7. Acceptance criteria

A PR against `news-lens` master is acceptance-ready when:

- `shellcheck skills/news-lens-ab-merge/scripts/ab_merge.sh`
  passes with no new errors.
- The smoke test above passes when run against the maintainer's
  live wiki.
- The cap-behavior and idempotency tests pass.
- The change is contained in `ab_merge.sh` plus optionally a
  short inline test in the same file. No other files modified.
- The commit message documents:
  - The smoke test command(s) used to verify.
  - The chosen drafting prompt (or a link to where it lives in
    the script).
  - Any classification edge cases encountered.

---

## 8. Non-goals (explicit list)

Do not implement, do not "improve into existence":

- Relaxed lint that accepts unresolved wikilinks.
- Propagator-side stub creation.
- Bidirectional backlink fix-ups in the drafted articles
  (propagator handles those after merge).
- Concept-vs-reference classification using an LLM call (heuristic
  + existing-reference filename match is enough).
- Citation-quality scoring of the drafted articles.
- Caching of drafted articles across runs.
- Parallel drafting of the missing articles (serial is fine; the
  cap of 10 makes the total time predictable).
- Any change to the manifest's required fields.

---

## 9. rb-lite invocation hints (for the operator)

```bash
cd ~/news-lens
git switch -c skill/complete-citations
rb-lite run \
  --task-file docs/SPEC-skill-complete-citations.md \
  --base origin/master
```

After rb-lite finishes and the PR merges to master, the operator
must manually sync the skill to the runner's home:

```bash
cp ~/news-lens/skills/news-lens-ab-merge/scripts/ab_merge.sh \
   ~/.claude/skills/news-lens-ab-merge/scripts/ab_merge.sh
```

(The two copies are kept in sync by convention; there's no
automated mirror.)
