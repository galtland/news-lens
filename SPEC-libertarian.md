# news-lens — Wiki-Grounded News Commentary (spec)

> Working draft for a fork/derivative of `news-tagger` that comments on news from
> an Austrian-libertarian perspective using the `llm-wiki` corpus as ground truth.

Status: **draft / pre-implementation**, revision 3.

Working name: **news-lens**. Bikeshed later.

---

## 0. Locked-in decisions (revision 3)

These are settled. The rest of the spec elaborates them. Items struck through were locked in earlier revisions but superseded by the v3 simplification.

1. **Hard fork** of `news-tagger`, not a mode added to it.
2. **Single-wiki for v1.** One topic-wiki path, one lens.
3. **The wiki is the system of record.** news-lens is a contributor.
4. **Single agentic call per post.** One harness invocation does ingest, optional commentary, and lint in one transaction. No separate prefilter / commenter / ingest roles in v1. The complexity removed is enormous; we'll add structure back only when measurement demands it. (§5)
5. **No upstream skill changes.** The agent is invoked with a hand-crafted prompt that orchestrates existing skills (`/wiki:ingest`, `/wiki:lint --fix`). A wrapper skill is a v2 option only if the prompt becomes unmaintainable.
6. **Block-then-publish.** Replies are only published after the agent confirms the thesis exists at a known slug.
7. **Source of truth for "what's been done" is split:** filesystem for wiki facts (raw news, theses); `state.sqlite` for platform facts (fetch cursor, X/Nostr post IDs).
8. ~~Three LLM roles with separate config blocks~~ — superseded by #4. One harness, one config block.
9. ~~Three-pass CLI (ingest / comment / publish)~~ — collapsed. One agent call per post + one publish step.
10. ~~Outbox staging directory + slug-claim mechanism~~ — agent picks slugs inside the call; no staging.

---

## 1. Goals and non-goals

### Goals

1. Given a news item (a tweet, a headline, an article excerpt), produce a piece of commentary written from a defined editorial perspective — initially the Austrian-libertarian perspective captured in `~/wiki/topics/libertarian/`.
2. Ground the commentary in the wiki: cite specific concept/reference articles, reuse their framings, and stay consistent with positions already documented there.
3. Build a persistent **archive of contextualized news**: every fetched post is filed as a raw source in the wiki, regardless of whether commentary follows.
4. Reuse `news-tagger`'s plumbing where it still earns its place: hexagonal architecture, X/Nostr publishers, state store, post sources, run loop, rate limiting, dry-run/outbox modes.

### Non-goals

- Multi-label classification into a fixed taxonomy (that's news-tagger's job).
- Editorial perspectives outside of what the connected wiki contains. Swap wikis to swap perspectives.
- A general-purpose RAG framework. Scope is wiki-shaped corpora.

### Out of scope for v1

- Multi-perspective commentary in one run. Multiple lens files over one wiki is OK; multiple wikis is v2.
- Translation of news from other languages.
- Original images / charts.
- The `curate` TUI from news-tagger.
- Embeddings.
- A separate cheap prefilter LLM. The agent handles "is this newsworthy" inline.

---

## 2. Fork rationale (compressed)

A hard fork is the right move because the output shape and corpus shape both genuinely differ from news-tagger. `ClassifyOutput.tags[]` is a discrete bucket assignment; `Commentary` is one piece of prose with citations. `TagDefinition` is a single short rule; a wiki article is a long, hyperlinked node in a graph. Trying to unify them in one binary poisons every downstream consumer for no real reuse.

Under the v3 single-agent design, the fork is also much smaller than originally planned: roughly the post sources, state store, publishers, run loop, and a thin subprocess wrapper. The 9 LLM provider adapters are dropped (§4) because the harness owns provider selection.

---

## 3. What we keep from news-tagger

| Component | Path | Change |
|---|---|---|
| `SourcePost` | `domain/model.rs` | keep |
| `PostSource` port + `JsonlPostSource`, `XPostSource`, `StubPostSource` | `domain/ports.rs`, `adapters/jsonl_source.rs`, `adapters/x_api/` | keep |
| `Publisher` port + `XPublisher`, `NostrPublisher`, `outbox.rs` | `domain/ports.rs`, `adapters/x_api/write.rs`, `adapters/nostr/` | keep |
| `StateStore` port + sqlite/memory impls | `domain/ports.rs`, `adapters/state_*.rs` | keep, simpler schema |
| `Clock`, `RateLimiter`, ignore-pattern compilation | `domain/ports.rs`, `domain/usecases/run_loop.rs` | keep |
| `AppConfig` infra, env-var loading, doctor | `cli/src/{config,commands/doctor}.rs` | keep |
| `Cli` + clap subcommand scaffolding | `cli/src/{args,main}.rs` | keep, replace command bodies |

## 4. What we drop

| Component | Why |
|---|---|
| `TagDefinition`, `Taxonomy`, `taxonomy_hash` | Tag semantics gone. |
| `ClassifyOutput`, `TagMatch`, `usecases/classify.rs`, `usecases/render.rs` | Replaced by the agent. |
| `commands/curate.rs` (TUI) | Out for v1. |
| `policy.rs` `forbidden_patterns` | Editorial guardrails live in the lens, enforced by the agent. |
| **`adapters/llm/{anthropic,openai,gemini,ollama,opencode,openai_compat,claude_code,codex,local_command}.rs`** | The harness owns model + provider selection. news-lens shells out to `claude` / `codex` / `opencode` and parses output. ~2000 LOC removed. |
| Direct wiki-write logic (`FsWikiWriter` was considered, removed) | Agent handles wiki writes via `/wiki:ingest`. |

`adapters/llm/stub.rs` may stay around as a helper for tests; the rest is gone.

---

## 5. Architecture: single-agent

One harness invocation per post, period.

```
news-lens fetch loop (Rust):
  for each new post from X / Nostr / JSONL:
    1. shell out to harness with the process-post prompt
         agent does:
           - read the lens
           - file the post as raw/news/<slug>.md via /wiki:ingest
           - decide if newsworthy enough to comment
           - if yes:
               - retrieve relevant articles from wiki/{concepts,topics,references}/ via Read/Grep
               - draft commentary citing specific slugs
               - write thesis to wiki/theses/<slug>.md
           - run /wiki:lint --fix
           - print final-line JSON: { stance, raw_path, thesis_path?, thesis_slug? }
    2. if a thesis was produced:
         - render reply text (pure Rust, simple template)
         - publish via X / Nostr (existing news-tagger adapter)
    3. record_processed in state.sqlite with both wiki paths and platform IDs
```

That's the whole pipeline. Two phases per post: agent call (which does ingest + maybe comment), then publish (only if there's a thesis).

### What the agent prompt looks like (sketch)

```
You are news-lens, processing a single news post into a wiki at <path>.
The lens at <lens_path> defines the editorial perspective.

Task:
1. File the news verbatim as raw/news/YYYY-MM-DD-<slug>.md using /wiki:ingest.
   Use frontmatter: type=news, source=<url>, captured_at, author, platform.
2. Read the lens. Decide if this post is worth commenting on per the lens
   stances (Endorse | Critique | Contextualize | Decline). Be strict —
   prefer Decline when the wiki has nothing substantive to add.
3. If not Decline:
   - Read 5–12 relevant articles from wiki/{concepts,topics,references}/.
     Do not read wiki/theses/ — avoid feedback loops with prior commentary.
   - Draft a thesis article (markdown) following the wiki's frontmatter and
     See Also conventions. Cite slugs with [[wikilinks]]. Quote the news
     text where you call out a framing.
   - Write it to wiki/theses/<slug>.md.
4. Run /wiki:lint --fix to heal indexes, See Also backlinks, log.md.
5. Print the final line as a single JSON object:
   { "stance": "...", "raw_path": "...", "thesis_path": "...?",
     "thesis_slug": "...?", "lint_warnings": N, "lint_critical": N }

Constraints:
- Never invent positions the wiki does not hold.
- Never cite slugs that don't exist.
- Keep the one_liner you'd put in a reply <= 240 chars; include it as
  the first paragraph of the thesis after the H1.
- If lint_critical > 0, leave the thesis in place but flag it in the JSON.
```

The prompt template is shipped with news-lens at `prompts/process-post.md`. It substitutes `<path>`, `<lens_path>`, post text, post metadata, and a deterministic candidate slug.

### Concurrency

Multiple agent invocations on the same wiki race on `_index.md` and `log.md`. v1 serializes them: `mutex` per wiki path. Multiple wikis (v2) get separate mutexes. Post fetching and publishing are not serialized.

### Slug determinism

The agent picks the slug. We hint a candidate based on date + slugified title. The agent may override if it collides with an existing file (it sees the wiki). Whatever the agent picks is what news-lens uses for the reply text — that's why we wait for the JSON before rendering.

---

## 6. The lens

A short markdown file separate from the wiki proper. Defines tone, register, stance vocabulary, editorial guardrails. Pointed at by `[lens] path` in config.

```markdown
---
id: austrian-libertarian
voice: terse, dry, citation-heavy
register: written, not chatty
---

# Austrian-libertarian editorial policy

You comment on news from the Austrian-libertarian perspective documented
in the connected wiki. Your beliefs are exactly those of the wiki.

## Stances
- Endorse — news reports facts that confirm a wiki claim.
- Critique — news's framing assumes premises the wiki rejects.
- Contextualize — news is descriptive, wiki adds frame.
- Decline — no wiki article speaks to the substance.

## Always
- Cite specific wiki articles by slug.
- Quote the news's exact wording when calling out a framing.
- Distinguish factual disagreement from framing disagreement.

## Never
- Insults, partisan slogans, culture-war shorthand.
- Predictions presented as certainties.
- Citations to articles you weren't shown.
- Endorsement of violence or doxxing.
```

Multiple lens files over one wiki are supported in v1. The active one is selected at runtime.

---

## 7. CLI surface

| Command | Behavior |
|---|---|
| `news-lens run` | Fetch new posts → for each, agent call + optional publish → loop. |
| `news-lens run --once` | One poll cycle. |
| `news-lens process --post <id-or-text>` | Process a single post (fetched-by-id or ad-hoc text). |
| `news-lens process --jsonl <path>` | Process posts from a fixture file. |
| `news-lens wiki status` | Wiki path, raw news count, theses count, uncommented news count. |
| `news-lens lens list \| show <id>` | Inspect lens files. |
| `news-lens doctor` | Existing doctor + wiki readable, lens parseable, harness reachable. |
| `news-lens config init` | Generate config skeleton. |

Standard flags: `--dry-run` (no publishing), `--require-approval` (write to outbox instead of publishing), `--config <path>`.

`fetch`, `classify`, `curate` are dropped.

---

## 8. Configuration

```toml
[general]
state_db_path = "./state.sqlite"
dry_run = true

[wiki]
path = "/home/user/wiki/topics/libertarian"

[lens]
path = "/home/user/wiki/topics/libertarian/lens-austrian-libertarian.md"
id   = "austrian-libertarian"

[harness]
command         = "claude"
args            = ["--print"]
prompt_template = "/home/user/news-lens/prompts/process-post.md"
timeout_secs    = 600
serialize_per_wiki = true   # mutex agent calls against the same wiki path

[watch]
poll_interval_secs = 300
accounts = ["...", "..."]
include_replies = false
include_reposts = false
ignore_patterns = []

[x.read]
bearer_token_env = "X_BEARER_TOKEN"

[x.write]
enabled = false
mode    = "reply"

[nostr]
enabled = false
relays  = ["wss://relay.damus.io"]

[budget]
daily_dollar_limit = 10.00
on_exceed = "halt"   # halt | warn
```

Cost-control is by dollar limit, not token limit, since the harness reports its own cost in stdout in some configurations. If unavailable, news-lens falls back to a per-call cap.

---

## 9. Agent return contract

```json
{
  "stance": "critique",
  "raw_path": "raw/news/2026-05-07-argentina-rent-decontrol.md",
  "raw_slug": "2026-05-07-argentina-rent-decontrol",
  "thesis_path": "wiki/theses/on-argentina-rent-decontrol.md",
  "thesis_slug": "on-argentina-rent-decontrol",
  "one_liner": "The cap simply cuts the supply they were going to consume — see [[state-power-and-intervention]].",
  "lint_warnings": 0,
  "lint_critical": 0
}
```

Validation by news-lens before publishing:
- `raw_path` must exist on disk.
- If `stance != "decline"`: `thesis_path` and `thesis_slug` must be present and exist.
- `one_liner.len() <= 240` (we'll truncate gently if not).
- `lint_critical == 0` — otherwise quarantine the thesis (move to a `quarantine/` sibling) and skip publishing. Manual review.

The agent returning malformed JSON or a missing path counts as a transient failure: log + retry once. After two failures, mark the post `failed` in state and move on.

---

## 10. State store

Schema is intentionally tiny. Each row records what news-lens did with one post:

```sql
CREATE TABLE processed_posts (
    post_id          TEXT PRIMARY KEY,
    lens_id          TEXT NOT NULL,
    processed_at     TEXT NOT NULL,
    stance           TEXT NOT NULL,        -- endorse|critique|contextualize|decline|failed
    raw_path         TEXT,                 -- relative to wiki root
    thesis_slug      TEXT,                 -- nullable when stance=decline|failed
    x_post_id        TEXT,                 -- nullable
    nostr_event_id   TEXT,                 -- nullable
    cost_usd         REAL                  -- harness-reported, nullable
);

CREATE TABLE account_state (
    account     TEXT PRIMARY KEY,
    since_id    TEXT,
    updated_at  TEXT NOT NULL
);
```

That's the entire schema. "Has post X been processed?" is a primary-key lookup. "What did we publish for thesis Y?" is a slug filter. Reconciliation with the wiki is a separate `wiki status` operation; it's never on the hot path.

---

## 11. Phased rollout

**Phase 0 — spec lock-in.** This doc.

**Phase 1 — one-shot agent.** Fork the repo, gut the dropped components, implement:
- `news-lens process --post --text "..."` runs the harness on a synthetic post against a real wiki, prints the agent's JSON.
- No publishing, no state DB writes, no fetching.
- Goal: see whether the prompt produces clean lint output and useful commentary.

**Phase 2 — fetch + state.** Wire post sources back in. `news-lens process --jsonl <fixture>` processes a batch. Records to state DB. Still no publishing.

**Phase 3 — run loop.** `news-lens run --dry-run` polls X, processes posts, writes to wiki, but does not publish. Run for a week, eyeball output.

**Phase 4 — publish.** Enable X/Nostr in `--require-approval` mode (existing outbox flow). Every commentary reviewed before going out. Stay here for at least a week.

**Phase 5 — what we cut.** Re-evaluate based on measurement:
- Cost per non-newsworthy post too high → split a cheap prefilter back out.
- Agent retrieval picks irrelevant articles too often → split a Rust-side retrieval back out.
- Agent can't keep JSON shape stable → move the commentary call to a direct API with `response_format`.

None of these require rearchitecture; each is a localized split.

---

## 12. Open questions

1. **News deduplication.** Two accounts post the same news — agent should detect existing raw news (it can grep) and reuse it, attaching the new post URL as another `source:` reference. Verify the prompt encourages this.
2. **What counts as "news" worth fetching.** Pre-fetch filter knobs (`include_replies`, `include_threads`, `min_text_chars`) stay in config. The agent's Decline handles the rest.
3. **Lens versioning.** If you tweak the lens, should past commentary be regenerated? v1: no. Re-run by hand on selected posts (`process --post <id> --force`) when you want to.
4. **Harness failure modes.** Timeout / lint criticals / malformed JSON — quarantine policy in §9. Open question is whether to auto-retry transient errors with backoff. Default: one retry, then fail-and-move-on.
5. **Cost runaway.** A misbehaving agent can spend an unlimited budget on a single post. Need per-call hard cap (timeout + token limit if the harness exposes it). Daily budget circuit breaker is the safety net.
6. **Concurrency limits.** Single-wiki serialization is enforced; what's the right max for parallel post processing across wikis? Defer until multi-wiki.
7. **Thesis re-edit.** What if the agent wants to update an existing thesis (e.g., new news on the same story)? v1: it doesn't — each thesis is a single news's commentary. Updates happen by writing a *new* thesis that cross-links. Revisit if this gets noisy.
8. **Agent retrieval discipline.** The prompt tells the agent to read 5–12 articles. Without that ceiling the agent can read the entire wiki and blow the context budget. Worth measuring how strictly the agent obeys.

---

## 13. Recommended next steps

1. Stand up `news-lens` repo from a fork of `news-tagger@master` (preserve git history).
2. Delete §4 components: tag types, classify use case, render, curate, the entire `adapters/llm/` provider tree (keep `stub.rs` if useful for tests).
3. Add the harness adapter — a thin async subprocess wrapper that takes the prompt template + post payload and returns parsed final-line JSON.
4. Write `prompts/process-post.md` and iterate on it against a stub wiki (a copy of the libertarian wiki with one news fixture).
5. Implement `news-lens process --post --text "..."` end-to-end. (= Phase 1 done.)
6. Wire post sources, state store, publishers back in. (= Phases 2–4.)
7. Verify the wiki plugin already supports `raw/news/`, `type: news`, `category: thesis` — patch if not.

Phase 1 should be a couple of days of work, not a week — the codebase shrinks dramatically. Phases 2–4 are where the prompt engineering lives, and that's where the time will go.
