# news-lens — Newsroom Automation (spec)

> Draft design for turning the `news-lens` repository into a GitHub-native
> newsroom: cron fetcher → issues → thesis-builder CI → PR → merge → publish.

Status: **design draft, pre-implementation**, revision 1.

Builds on `SPEC-libertarian.md` (the editorial pipeline itself — claude+codex
A/B+merge wrapped in the `news-lens-ab-merge` skill). This spec covers only
the *operational shell* around that pipeline: how news arrives, how a thesis
becomes a reviewable artifact, how merge propagates to the public site.

---

## 0. Proposed decisions

Numbered so we can lock them in/out one by one. **Italicized** items are open
questions where I've picked a default but want explicit confirmation.

1. **GitHub is the substrate.** Issues are the ingest queue. PRs are the
   review surface. Actions are the runtime. No external daemon, no VM, no
   queue server.
2. *Thesis PRs target the wiki repo, not the news-lens repo (Model A).*
   News-lens stays a code/automation repo; the wiki stays a content repo.
   Alternative (Model B): vendor the wiki content into `news-lens/wiki/`.
   See §6.
3. **Thesis PRs are atomic — exactly two new files.** `raw/news/<slug>.md`
   and `wiki/theses/<slug>.md`. No edits to indexes, no edits to `log.md`,
   no edits to existing articles. Everything else is post-merge.
4. **Post-merge propagation is single-threaded and serialized.** Run as one
   workflow with `concurrency.group` so merges queue, never race.
5. **Indexes are derived, not hand-edited.** `_index.md` files are rebuilt
   from frontmatter on every propagation pass. PRs that touch indexes are
   rejected by CI.
6. *Self-hosted runner on the user's machine for v1.* Reuses the existing
   `claude` + `codex` installs and llm-wiki plugin without secret management.
   Move to a hosted runner with API-key secrets only if the laptop being
   off becomes a problem.
7. **Dedup happens at fetch time, not at issue creation time.** The fetcher
   keeps a `state/seen-urls.txt` in the news-lens repo, committed on every
   fetch cycle. No issue is opened for a URL already in the file.
8. *Human merges the PR.* No auto-merge in v1. The whole point of the PR
   is to review the thesis before it goes public.
9. *Twitter is not a v1 source.* API access is paywalled to uselessness.
   v1 fetches from a small set of RSS feeds + HN front page. Twitter/X
   moves to v2 if/when a workable access path appears.
10. **Failure modes don't auto-retry.** A thesis build that fails records
    `stance=failed` in the PR description and stops. Human reruns the
    workflow manually. (Mirrors SPEC-libertarian §0.8 KISS rule.)

---

## 1. Goals and non-goals

### Goals

1. Eliminate the manual loop currently needed to run `news-lens-ab-merge`
   against incoming news. Make the laptop's role optional, not mandatory.
2. Give every thesis a reviewable preview before it's published. PRs are
   the natural fit — diff renders the thesis Markdown inline.
3. Keep the pipeline conflict-free even when many theses are in flight.
4. Preserve the existing wiki content layout. No content migration.

### Non-goals

1. **Multi-wiki orchestration.** v1 is single wiki (`topics/libertarian/`),
   mirroring SPEC-libertarian §0.2.
2. **High-volume throughput.** Target rate is a handful of theses per day,
   not a firehose. The 5-min cron is for latency, not volume.
3. **Editorial autonomy.** A human still reads each thesis before merge.
   We are not building a publish-without-review pipeline.
4. **Cross-wiki backlinks.** Propagation operates inside one topic wiki.

---

## 2. Pipeline shape

```
┌─────────────────────────────────────────────────────────┐
│ news-lens repo                                          │
│                                                         │
│  cron */5 * * * *                                       │
│      │                                                  │
│      ▼                                                  │
│  fetcher workflow ───── reads feeds, dedups via         │
│      │                  state/seen-urls.txt             │
│      │                                                  │
│      ▼                                                  │
│  gh issue create --label news                           │
│      │                                                  │
│      ▼                                                  │
│  on: issues opened ── thesis-builder workflow           │
│      │                  runs ab_merge.sh                │
│      │                  pushes branch + opens PR        │
└──────┼──────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────┐
│ wiki repo (topics/libertarian/)                         │
│                                                         │
│  PR opened with 2 new files                             │
│      │                                                  │
│      ▼                                                  │
│  pre-merge CI: structural lint (frontmatter, links)     │
│      │                                                  │
│      ▼                                                  │
│  human review + merge                                   │
│      │                                                  │
│      ▼                                                  │
│  on: push main (concurrency: propagate)                 │
│      │                                                  │
│      ▼                                                  │
│  propagation workflow ── rebuild indexes,               │
│      │                   /wiki:lint --fix backlinks,    │
│      │                   append log.md,                 │
│      │                   commit directly to main        │
│      │                                                  │
│      ▼                                                  │
│  publish workflow ── publish.sh → Quartz repo           │
│                                  → GitHub Pages         │
└─────────────────────────────────────────────────────────┘
```

---

## 3. Components

### 3.1 Fetcher

- **Trigger**: `on: schedule: cron: "*/5 * * * *"` + `workflow_dispatch` for
  manual runs.
- **Inputs**: `state/feeds.yaml` (list of RSS URLs + HN tag filters),
  `state/seen-urls.txt` (committed dedup set).
- **Behavior**:
  1. Read `feeds.yaml`, fetch each feed (RSS XML or HN API).
  2. For each item, compute canonical URL (strip tracking params). Skip if
     present in `seen-urls.txt`.
  3. For each new item, `gh issue create` with title = headline, body = URL
     + first paragraph of fetched content + source feed name, labels =
     `news, source:<feed-id>`.
  4. Append new URLs to `seen-urls.txt`, commit + push.
- **Constraints**: Idempotent. If the workflow is interrupted between
  issue-create and seen-urls commit, the next cycle will re-file the same
  issue — annoying but not corrupting. (Could be fixed with a temp-file
  staging step; deferred to "if it becomes a problem".)
- **Rate limiting**: Each cycle caps at ~10 new issues to avoid floods on
  a slow news day catching up after an outage.

### 3.2 Thesis builder

- **Trigger**: `on: issues: types: [opened, labeled]` with
  `if: contains(github.event.issue.labels.*.name, 'news')`.
- **Behavior**:
  1. Checkout wiki repo (read-only mode, no commits to it from here).
  2. Run `~/.claude/skills/news-lens-ab-merge/scripts/ab_merge.sh
     --label "issue-${N}" --text "<issue body>" --no-publish`. This
     produces a thesis under a trial path.
  3. If `skip merge: 1` (claude failed, codex-only fallback), continue
     — the codex draft is the artifact.
  4. Read the resulting manifest. Extract `slug`, `stance`, citations.
  5. Open PR against wiki repo's `main`:
     - Branch: `thesis/issue-<N>`
     - Title: `Thesis: <thesis title>`
     - Body: full thesis Markdown rendered inline + a "Builder summary"
       block (stance, citation count, claude/codex/merged paths,
       link back to the originating issue).
     - Files changed: `raw/news/<slug>.md` + `wiki/theses/<slug>.md`,
       nothing else.
  6. Post a comment on the originating issue: "Draft thesis opened as
     PR #<N>" with the rendered thesis text inlined for quick review.
- **On failure**: Comment on the issue with the failure mode (which
  step, which logs). Don't auto-retry.

### 3.3 Pre-merge lint

- **Trigger**: `on: pull_request` in wiki repo, with `paths:
  ['wiki/theses/**', 'raw/news/**']`.
- **Behavior**:
  1. Run a structural subset of `/wiki:lint` (frontmatter validity, link
     resolution, slug format). Not the full quality lint — that needs
     LLM and would gate every PR on flaky weather.
  2. Reject PRs that touch any path other than `wiki/theses/` and
     `raw/news/` — enforces §0.3.
  3. Surface failures as a PR check.

### 3.4 Propagator

- **Trigger**: `on: push: branches: [main]` in wiki repo, with
  `concurrency: { group: propagate, cancel-in-progress: false }`. The
  serialized queue is the entire conflict-prevention strategy.
- **Behavior**:
  1. Identify newly-merged theses by diffing the push range.
  2. Rebuild `_index.md` files under `wiki/`, `raw/`, and the master
     `_index.md`. The Derived Index Protocol means this is just
     re-scanning frontmatter and rewriting indexes deterministically.
  3. Run `/wiki:lint --fix` to repair See Also bidirectional links.
     For each new thesis, every outbound See Also gets a corresponding
     inbound link added to the cited article.
  4. Append entries to `log.md` for each newly-landed thesis.
  5. Commit the result directly to main with message `propagate: <slugs>`,
     bypassing PR (it's a derived rebuild, not authored content).
  6. Push.
- **Why direct push, not PR**: A PR would re-trigger pre-merge lint
  recursively. The propagator is trusted code rebuilding from authored
  content. Branch protection rule needs an exception for the bot user.

### 3.5 Publisher

- **Trigger**: `on: push: branches: [main]` in wiki repo, separate
  workflow from propagator. Runs after propagator (sequenced by both
  being in the same concurrency group, or chained via `workflow_run`).
- **Behavior**: Invoke existing `publish.sh` to push Quartz output to
  the public repo → GitHub Pages.

---

## 4. Conflict story

The cross-cutting design constraint is: any number of parallel thesis
PRs must be mergeable in any order without conflict.

**Why thesis PRs don't conflict with each other:**

- Each PR adds exactly two new files. New files don't conflict.
- Slugs are date-prefixed (`YYYY-MM-DD-<topic>`) so collisions are vanishingly
  rare. If two theses on the same topic land the same day, the builder
  appends a sequence suffix.
- Indexes, `log.md`, and cross-article backlinks are all out of scope
  for thesis PRs (§0.3, §3.2.5).

**Why propagation doesn't conflict with itself:**

- `concurrency.group: propagate` with `cancel-in-progress: false` makes
  GitHub queue propagation jobs. One runs at a time, end to end.
- Each propagation rebuilds from current state, so even if N merges
  happened during a previous propagation run, the next run picks up
  all of them at once.

**Edge cases:**

- *Two theses cite the same hub article.* Both want it added as a
  backlink target. `/wiki:lint --fix` is idempotent; running once after
  both merged adds both links without duplicating.
- *A PR sits stale while others merge.* Branch protection requires
  "up-to-date with main" + merge queue rebases the PR before merging.
  Because the PR doesn't touch indexes or logs, the rebase has no
  conflicts to resolve — just a fast-forward over later commits.
- *Propagator fails mid-run.* No partial state lands (atomic commit).
  Next propagation run from a later merge will repair everything.

---

## 5. State and dedup

Two state stores:

- **`news-lens/state/seen-urls.txt`** — committed file, append-only,
  one canonical URL per line. Source of truth for "did we already
  file an issue for this URL?"
- **`news-lens/state/feeds.yaml`** — committed file, list of feed
  configs. Source of truth for what the fetcher reads.

No SQLite. No external KV. The old `state.sqlite` from SPEC-libertarian
§7/§10 stays inside news-lens for the harness itself (account state,
fetch cursors per feed), but is not part of the newsroom layer's
correctness model.

GitHub Issues + PRs are themselves the queue/inbox state. No mirror.

---

## 6. Repo topology — Model A vs Model B

**Model A (recommended).** Two repos:

- `news-lens` — code, skill, CI workflows, fetcher state, prompts.
- `wiki` (or whatever the topic wiki repo is called) — content. Receives
  PRs from the news-lens builder workflow. Owns propagation + publish.

Pros: clean separation. The wiki repo's git history is editorial; the
news-lens repo's history is automation. publish.sh keeps working
unchanged.

Cons: the builder workflow in news-lens needs a PAT or App token with
write access to the wiki repo. Cross-repo PR creation is a known but
slightly fiddly pattern.

**Model B.** Single repo:

- `news-lens/wiki/topics/libertarian/...` — content lives here.

Pros: one repo, one set of permissions, no cross-repo tokens.

Cons: editorial content lives under a code repo. The Quartz publish
target either becomes a subdirectory deploy or the wiki gets copied
out to a separate published repo (which puts us back near Model A).

**Recommendation: A.** B's only real win is cross-repo token management,
and that's a one-time setup cost (a GitHub App with `pull_requests:write`
on the wiki repo). The repo separation is worth keeping.

---

## 7. Open questions

1. **Token strategy.** PAT in repo secrets is simplest but ages out and
   ties the bot to one human. GitHub App is cleaner but more setup.
   Decide before §3.2 lands.
2. **PR check requirements.** Should pre-merge lint failure block merge,
   or just annotate? Recommendation: block. But the structural lint
   needs to be fast and deterministic to not become a merge-blocker.
3. **Auto-close on PR merge.** When a thesis PR merges, should the
   originating issue auto-close? `Closes #N` in the PR body does this
   natively, but only if the PR is in the same repo. Cross-repo
   closes don't work that way — we'd need an explicit comment +
   issue-close step in the builder workflow.
4. **Failure visibility.** Where does a `stance=failed` show up? In a
   PR with no thesis attached, just the log? Or no PR at all, just a
   comment on the issue?
5. **News sources for v1.** A concrete list to drop in `feeds.yaml`.
   Candidates: Mises Wire RSS, ZeroHedge RSS, Cato @ Liberty, HN
   `front_page` filtered by economic/political keywords, Reuters
   business RSS, Bloomberg markets RSS. Pick 3-5.
6. **Twitter path.** Track separately. If/when a workable read path
   exists (Bluesky bridge? Nitter mirror? paid API?), add as another
   `feeds.yaml` source.
7. **Backfill policy.** When the cron's been off for a while, should
   the next run flood-fill or skip ahead? §3.1 caps at 10/cycle as a
   safety; decide whether that's right.

---

## 8. Phasing

Each phase ends at a demonstrable state.

### Phase A — Manual issue → manual builder (no fetcher, no PR)

- Add a `workflow_dispatch` workflow in news-lens that takes a URL or
  text input and runs the ab_merge skill.
- Workflow runs on self-hosted runner.
- Output: comment on a manually-created issue with the thesis text.
- No PR yet. No cross-repo. Just validates the runtime.

### Phase B — Builder opens PRs to wiki repo

- Token/App set up for wiki repo write access.
- Builder workflow opens PR with the two new files.
- Pre-merge lint workflow in wiki repo.
- Human merges manually. Existing publish.sh runs as today.

### Phase C — Propagator

- Post-merge workflow in wiki repo: index rebuild + lint --fix + log
  append + direct commit.
- Concurrency group for serialization.
- Publisher workflow chained after propagator.

### Phase D — Fetcher

- Cron workflow in news-lens reading `feeds.yaml`.
- Dedup via `seen-urls.txt`.
- Opens issues with `news` label.
- End-to-end: cron fires → issue → PR → merge → publish.

### Phase E — Operational hardening

- Failure surfaces (issue comments on builder failure).
- Rate limiting / backfill policy.
- Whatever §7 open questions remain.

---

## 9. Non-decisions explicitly deferred

To keep v1 honest about scope:

- **Multi-source corroboration.** If two feeds report the same story,
  the second is currently a duplicate URL and gets skipped. Treating
  them as "two sources for the same news item" (and citing both in
  the thesis) is a v2 concept.
- **Editorial calendar / prioritization.** Issues are FIFO. No
  weighting by source authority or topic recency.
- **Retraction / amendment workflow.** Once published, a thesis is
  edited via normal wiki PRs, not via the newsroom pipeline.
- **Analytics.** How often is each cited article viewed? Out of scope.
- **Multi-lens.** Same news, multiple editorial lenses → multiple
  theses. Possible later but not v1.
