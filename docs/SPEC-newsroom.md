# news-lens — Newsroom Automation (spec)

> Draft design for turning the **wiki repository** into a GitHub-native
> newsroom: cron fetcher → issues → thesis-builder CI → PR → merge →
> propagation → publish. `news-lens` is the software installed on the
> runners (and on contributor machines) that drives the steps; it is not
> the newsroom substrate.

Status: **design draft, pre-implementation**, revision 2.

Builds on `SPEC-libertarian.md` (the editorial pipeline itself — claude+codex
A/B+merge wrapped in the `news-lens-ab-merge` skill). This spec covers only
the *operational shell* around that pipeline: how news arrives, how a thesis
becomes a reviewable artifact, how merge propagates to the public site.

---

## 0. Proposed decisions

Numbered so we can lock them in/out one by one. **Italicized** items are
open questions where I've picked a default but want explicit confirmation.

1. **GitHub is the substrate.** Issues are the ingest queue. PRs are the
   review surface. Actions are the runtime. No external daemon, no VM, no
   queue server.
2. **The wiki repo is the newsroom.** All newsroom state — issues, PRs,
   workflows, fetcher state, dedup file, feed list — lives in the wiki
   hub repo at `git@github.com:galtland/galtland-wiki-index.git` (the source repo,
   distinct from the downstream Quartz site at https://index.galtland.org/).
   One repo, one set of permissions, no cross-repo tokens. See §6.
   The hub holds multiple topic wikis under `topics/<topic>/`; v1
   operates on `topics/libertarian/` only.
   **Visibility: private.** `topics/<topic>/raw/news/` stores fetched
   article text verbatim for provenance, which may include
   copyrighted material — the legal posture isn't certain, so the
   conservative default is to keep the source repo private. The
   public face is the Quartz site, which only publishes commentary
   (`wiki/theses/`), not the raw sources.
3. **news-lens is software, not state.** It's a CLI + skill installed on
   the runner (and on contributor machines). CI invokes it; the wiki
   repo never imports it as content. Versioned via the news-lens repo's
   own releases.
4. **Thesis PRs are atomic — exactly two new files.**
   `topics/<topic>/raw/news/<slug>.md` and
   `topics/<topic>/wiki/theses/<slug>.md`. No edits to indexes, no
   edits to `log.md`, no edits to existing articles. Everything else
   is post-merge.
5. **Post-merge propagation is single-threaded and serialized.** Run as
   one workflow with `concurrency.group` so merges queue, never race.
6. **Indexes are derived, not hand-edited.** `_index.md` files are
   rebuilt from frontmatter on every propagation pass. PRs that touch
   indexes are rejected by CI.
7. *Self-hosted runner on the user's machine for v1.* Reuses the existing
   `claude` + `codex` installs, the llm-wiki plugin, and a locally
   installed `news-lens` — no secret management needed. Move to a hosted
   runner only if the laptop being off becomes a problem.
8. **Dedup happens at fetch time.** A committed `newsroom/seen-urls.txt`
   in the wiki repo is the source of truth for "did we already file an
   issue for this URL?" No external KV.
9. *Human merges the PR.* No auto-merge in v1. The whole point of the PR
   is to review the thesis before it goes public.
10. *Twitter is not a v1 source.* API access is paywalled to uselessness.
    v1 fetches from a small set of RSS feeds + HN front page. Twitter/X
    moves to v2 if/when a workable access path appears.
11. **Failure modes don't auto-retry.** A thesis build that fails records
    `stance=failed` and stops. Human reruns the workflow manually.
    (Mirrors SPEC-libertarian §0.8 KISS rule.)

---

## 1. Goals and non-goals

### Goals

1. Eliminate the manual loop currently needed to run the
   `news-lens-ab-merge` skill against incoming news. Make the laptop's
   role optional, not mandatory.
2. Give every thesis a reviewable preview before it's published. PRs are
   the natural fit — diff renders the thesis Markdown inline.
3. Keep the pipeline conflict-free even when many theses are in flight.
4. Preserve the existing wiki content layout. No content migration.
5. Keep `news-lens` cleanly a *tool*. Anyone who wants to run their own
   newsroom on their own wiki repo installs news-lens and reuses the
   same workflow files. The wiki repo carries the newsroom; news-lens
   carries the machinery.

### Non-goals

1. **Multi-wiki orchestration.** v1 is single wiki
   (`topics/libertarian/`), mirroring SPEC-libertarian §0.2.
2. **High-volume throughput.** Target rate is a handful of theses per
   day, not a firehose. The 5-min cron is for latency, not volume.
3. **Editorial autonomy.** A human still reads each thesis before merge.
   We are not building a publish-without-review pipeline.
4. **Cross-wiki backlinks.** Propagation operates inside one topic wiki.

---

## 2. Pipeline shape

Everything below the dashed line runs inside the wiki repo.

```
┌─ contributor machine ─────────────────┐
│ news-lens CLI installed locally       │
│ runs ab_merge skill against drafts;   │
│ pushes manual PRs as needed           │
└───────────────────────────────────────┘

═════════════════════════════════════════════════════════════
                    wiki repository
═════════════════════════════════════════════════════════════

  cron */5 * * * *  (.github/workflows/fetch.yml)
      │
      ▼
  fetcher job ────── runs `news-lens fetch` (or equivalent)
      │              reads newsroom/feeds.yaml
      │              skips URLs in newsroom/seen-urls.txt
      │              gh issue create --label news
      │              commits updated seen-urls.txt
      │
      ▼
  on: issues opened  (.github/workflows/build-thesis.yml)
      │  if: contains(labels, 'news')
      ▼
  builder job ────── invokes news-lens-ab-merge skill
      │              writes 2 new files to a branch
      │              pushes branch
      │              opens PR (Closes #<issue>)
      │              comments on the issue with rendered thesis
      │
      ▼
  on: pull_request   (.github/workflows/pr-lint.yml)
      │
      ▼
  pre-merge lint ─── structural lint (frontmatter, links, slug)
      │              reject PRs touching paths outside
      │              topics/*/raw/news/** and topics/*/wiki/theses/**
      │
      ▼
  human review + merge
      │
      ▼
  on: push main      (.github/workflows/propagate.yml)
      │  concurrency: { group: propagate }
      ▼
  propagator job ─── rebuild _index.md files
      │              run `news-lens lint --fix` for backlinks
      │              append log.md entry
      │              commit + push directly to main
      │
      ▼
  on: push main      (.github/workflows/publish.yml)
      │  needs: propagate
      ▼
  publisher job ──── publish.sh → Quartz repo → GitHub Pages
```

---

## 3. Components

All five components are GitHub Actions workflows in the **wiki repo**,
under `.github/workflows/`. Each invokes a CLI subcommand of `news-lens`
or the bundled `news-lens-ab-merge` skill.

### 3.1 Fetcher (`fetch.yml`)

- **Trigger**: `on: schedule: cron: "*/5 * * * *"` +
  `workflow_dispatch` for manual runs.
- **Inputs**: `newsroom/feeds.yaml` (list of RSS URLs + HN filters),
  `newsroom/seen-urls.txt` (committed dedup set).
- **Behavior**:
  1. Read `feeds.yaml`, fetch each feed (RSS XML or HN API).
  2. For each item, compute a canonical URL (strip tracking params).
     Skip if present in `seen-urls.txt`.
  3. For each new item, `gh issue create` with title = headline, body =
     URL + first paragraph of fetched content + source feed name,
     labels = `news, source:<feed-id>`.
  4. Append new URLs to `seen-urls.txt`, commit + push directly to
     `main` (this is bot-authored derived state, not authored content).
- **Constraints**: Idempotent. If the workflow is interrupted between
  issue-create and seen-urls commit, the next cycle re-files the same
  issue — annoying but not corrupting. (Could be fixed with a temp-file
  staging step; deferred to "if it becomes a problem".)
- **Rate limiting**: Each cycle caps at ~10 new issues to avoid floods
  catching up after an outage.

### 3.2 Thesis builder (`build-thesis.yml`)

- **Trigger**: `on: issues: types: [opened, labeled]` with
  `if: contains(github.event.issue.labels.*.name, 'news')`.
- **Behavior**:
  1. Check out the wiki repo on a fresh branch
     `thesis/issue-<N>`.
  2. Run `news-lens-ab-merge` skill (installed on the runner) with the
     issue body as `--text`. Skill produces a thesis under the wiki's
     trial path and writes a manifest.
  3. If the claude leg failed (`skip merge: 1`), continue — the
     codex-only draft is the artifact, flagged in the PR body.
  4. Read the manifest. Extract `slug`, `stance`, `citations`.
  5. Commit `topics/libertarian/raw/news/<slug>.md` +
     `topics/libertarian/wiki/theses/<slug>.md` to the branch
     (nothing else). Push the branch.
  6. Open PR against `main`:
     - Title: `Thesis: <thesis title>`
     - Body: full thesis Markdown inlined + a "Builder summary" block
       (stance, citation count, both backends or codex-only, link to
       the originating issue) + `Closes #<N>`.
  7. Post a comment on the issue: "Draft thesis opened as PR #<M>"
     with the rendered thesis inlined for quick review.
- **Same-repo wins**: `Closes #N` auto-closes the issue on merge.
  `GITHUB_TOKEN` has the perms it needs out of the box. No PAT or App.
- **On failure**: Comment on the issue with the failure mode (which
  step, which log path on the runner). Don't auto-retry.

### 3.3 Pre-merge lint (`pr-lint.yml`)

- **Trigger**: `on: pull_request` with
  `paths: ['topics/*/wiki/theses/**', 'topics/*/raw/news/**']`.
- **Behavior**:
  1. Run a structural subset of `news-lens lint` (frontmatter
     validity, link resolution, slug format). Not the full quality
     lint — that needs LLM and would gate every PR on flaky weather.
  2. Reject PRs that touch any path other than
     `topics/*/wiki/theses/` and `topics/*/raw/news/` — enforces §0.4.
  3. Surface failures as a PR check.

### 3.4 Propagator (`propagate.yml`)

- **Trigger**: `on: push: branches: [main]` with
  `concurrency: { group: propagate, cancel-in-progress: false }`. The
  serialized queue is the entire conflict-prevention strategy.
- **Skip-self guard**: ignore push events authored by the propagator
  itself (check commit author email or message prefix) to avoid
  infinite loops.
- **Behavior**:
  1. Identify newly-merged theses by diffing the push range.
  2. Rebuild `_index.md` files under
     `topics/libertarian/wiki/`, `topics/libertarian/raw/`, and the
     topic's master `_index.md`. The Derived Index Protocol means
     this is just re-scanning frontmatter and rewriting indexes
     deterministically.
  3. Run `news-lens lint --fix` to repair See Also bidirectional
     links. For each new thesis, every outbound See Also gets a
     corresponding inbound link added to the cited article.
  4. Append entries to `log.md` for each newly-landed thesis.
  5. Commit the result directly to `main` with message
     `propagate: <slugs>`, bypassing PR (it's a derived rebuild, not
     authored content). Branch protection needs an exception for the
     bot identity, or the propagator pushes via a token that's allowed
     to bypass.
- **Why direct push, not PR**: a PR would re-trigger pre-merge lint
  recursively. The propagator is trusted code rebuilding from
  authored content.

### 3.5 Publisher (`publish.yml`)

- **Trigger**: `on: push: branches: [main]`, chained after the
  propagator via `workflow_run: workflows: [propagate]` so it only
  fires once propagation finishes.
- **Behavior**: Invoke `publish.sh` to push Quartz output to the public
  repo → GitHub Pages. (Unchanged from today's manual publish.)

---

## 4. Conflict story

The cross-cutting design constraint: any number of parallel thesis PRs
must be mergeable in any order without conflict.

**Why thesis PRs don't conflict with each other:**

- Each PR adds exactly two new files. New files don't conflict.
- Slugs are date-prefixed (`YYYY-MM-DD-<topic>`) so collisions are
  vanishingly rare. If two theses on the same topic land the same day,
  the builder appends a sequence suffix.
- Indexes, `log.md`, and cross-article backlinks are all out of scope
  for thesis PRs (§0.4, §3.2.5).

**Why propagation doesn't conflict with itself:**

- `concurrency.group: propagate` with `cancel-in-progress: false` makes
  GitHub queue propagation jobs. One runs at a time, end to end.
- Each propagation rebuilds from current state, so even if N merges
  happened during a previous propagation run, the next run picks up
  all of them at once.

**Edge cases:**

- *Two theses cite the same hub article.* Both want it added as a
  backlink target. `news-lens lint --fix` is idempotent; running once
  after both merged adds both links without duplicating.
- *A PR sits stale while others merge.* Branch protection requires
  "up-to-date with main" + GitHub's merge queue rebases the PR before
  merging. Because the PR doesn't touch indexes or logs, the rebase
  has no conflicts to resolve — just a fast-forward over later commits.
- *Propagator fails mid-run.* No partial state lands (atomic commit).
  The next propagation run from a later merge will repair everything.
- *Propagator's own push triggers propagator.* §3.4 skip-self guard
  short-circuits.

---

## 5. State and dedup

All newsroom state lives in the wiki repo under `newsroom/`:

- **`newsroom/feeds.yaml`** — committed, list of feed configs. Source
  of truth for what the fetcher reads.
- **`newsroom/seen-urls.txt`** — committed, append-only, one canonical
  URL per line. Source of truth for "did we already file an issue for
  this URL?"

No SQLite. No external KV. GitHub Issues + PRs are themselves the
queue/inbox state — no mirror.

`news-lens`'s own `state.sqlite` (per SPEC-libertarian §7/§10) lives on
the runner's filesystem and tracks per-run things like X account state.
It is **not** part of the newsroom layer's correctness model; losing it
costs at most a re-fetch.

---

## 6. news-lens distribution

CI workflows need `news-lens` (the binary + the `news-lens-ab-merge`
skill) available on the runner. Three options, picked per runner type:

**Self-hosted runner (v1).** news-lens is already installed locally
(via `cargo install` or the existing nix flake). The skill lives at
`~/.claude/skills/news-lens-ab-merge/`. Workflows just call them.
Pinning happens by "whatever version is checked out on that machine."

**Hosted runner (v2+).** Two viable paths:

- *Nix flake* — workflows do `nix run github:galtland/news-lens@<rev>
  -- <subcommand>`. Cached after first build. Trivially pinnable to a
  specific revision. Skill files come along in the flake output.
- *Release binary* — news-lens publishes tagged releases with
  pre-built binaries; workflows `curl` the release archive, extract
  binary + bundled skill into `$PATH`. Faster than nix-from-scratch
  but needs a release pipeline in the news-lens repo.

**Pinning.** Workflows reference a specific news-lens version. Bumping
the version is itself a PR against the wiki repo (touches
`.github/workflows/*.yml`), reviewed like any other.

---

## 7. Open questions

1. **Propagator push permissions.** The propagator commits directly to
   `main`, which collides with branch protection. Either: (a) exempt
   the bot identity, or (b) use a fine-grained token. (a) is simpler;
   (b) is more auditable.
2. **PR check requirements.** Should pre-merge lint failure block
   merge, or just annotate? Recommendation: block, but only if the
   structural lint is fast and deterministic enough not to become a
   merge-blocker on weather alone.
3. **Failure visibility.** When the builder fails outright, do we
   open a PR with no thesis attached (just the log)? Or no PR at all,
   only an issue comment? Latter is cleaner.
4. **News sources for v1.** A concrete list to drop in `feeds.yaml`.
   Candidates: Mises Wire RSS, ZeroHedge RSS, Cato @ Liberty, HN
   front-page filtered by economic/political keywords, Reuters
   business RSS, Bloomberg markets RSS. Pick 3–5.
5. **Twitter path.** Track separately. If/when a workable read path
   exists (Bluesky bridge? Nitter mirror? paid API?), add as another
   `feeds.yaml` source.
6. **Backfill policy.** When the cron's been off for a while, should
   the next run flood-fill or skip ahead? §3.1 caps at 10/cycle as a
   safety; decide whether that's right.
7. **news-lens version pinning strategy.** Self-hosted runner gets
   whatever is checked out locally — fine for v1 but means CI
   behavior changes silently when you `git pull` in news-lens.
   Acceptable trade for v1 simplicity?

---

## 8. Phasing

Each phase ends at a demonstrable state.

### Phase A — Newsroom scaffolding in the wiki repo

- Wiki repo already exists at `git@github.com:galtland/galtland-wiki-index.git`,
  hub layout with `topics/libertarian/` populated. No content
  migration needed.
- Add `newsroom/` directory at hub root with placeholder
  `feeds.yaml` and empty `seen-urls.txt`.
- Validates: the layout matches what news-lens expects;
  publish.sh still works untouched.

### Phase B — Manual builder workflow

- Add `.github/workflows/build-thesis.yml` triggered by
  `workflow_dispatch` (not yet by issues).
- Self-hosted runner (your machine) with news-lens already installed.
- Workflow: take a URL or text input, run the skill, open a PR with
  the two new files.
- Validates: news-lens + skill runs cleanly under Actions, PR opens
  with correct contents, `GITHUB_TOKEN` has needed perms.

### Phase C — Issue-triggered builder + pre-merge lint

- Switch the builder trigger from `workflow_dispatch` to
  `on: issues`.
- Add `pr-lint.yml`.
- Manually open issues with the `news` label to drive the pipeline.
- Validates: end-to-end from issue → reviewable PR.

### Phase D — Propagator + publisher

- Add `propagate.yml` (post-merge serialized job) and chain
  `publish.yml` after it.
- Branch protection rule exempting the bot identity.
- Validates: merging a thesis PR produces correct indexes, backlinks,
  log entry, and a published site update — all without human action.

### Phase E — Fetcher

- Add `fetch.yml` cron workflow reading `newsroom/feeds.yaml`.
- Populate `feeds.yaml` with the 3–5 sources chosen in §7.4.
- Validates: full loop runs untouched for a day; theses appear as
  PRs without any manual step.

### Phase F — Operational hardening

- Failure surfaces (issue comments on builder failure).
- Rate limiting / backfill policy tuning.
- Move to hosted runner if §0.7 reconsidered.
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
- **news-lens release engineering.** Tagged releases, binary
  distribution, nix output caching — all deferred to whenever a
  hosted runner becomes necessary.
