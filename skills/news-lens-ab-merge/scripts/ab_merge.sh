#!/bin/bash
# news-lens-ab-merge: orchestrate the A/B+merge thesis workflow
#
# Usage:
#   ab_merge.sh --label <slug> --text <news-text> --source <url-or-citation> [--no-publish]
#   ab_merge.sh --label <slug> --text-file <path> --source <url-or-citation> [--no-publish]
#   ab_merge.sh --label <slug> --text <news-text> --scenario [--no-publish]
#
# Provenance guard (REQUIRED — exactly one of):
#   --source <url-or-citation>   The news item is real and attributable; the
#                                value is recorded as the thesis source.
#   --scenario                   The news item is a synthetic/illustrative
#                                prompt, NOT a confirmed event. The produced
#                                thesis is stamped as an illustrative scenario
#                                (stance: scenario, confidence: low, plus a
#                                "not a sourced event" banner) instead of being
#                                passed off as real news. This prevents
#                                publishing fabricated events as fact (the
#                                2026-05 incident where six unsourced fixtures
#                                became live theses).
#   The run aborts if neither is given.
#
# Environment overrides:
#   NL_LIVE_WIKI         (default /home/user/wiki/topics/libertarian)
#   NL_PUBLIC_CONTENT    (default /home/user/projects/galtland.github.io/content)
#   NL_BIN               (default /home/user/news-lens/target/release/news-lens)
#   NL_PROMPT_TEMPLATE   (default /home/user/news-lens/prompts/process-post.md)
#   NL_SCRATCH           (default /tmp)
#   NL_MAX_NEW_ARTICLES  (default 10)
#   NL_ARTICLE_BODY_MIN_WORDS (default 150)
#   NL_ARTICLE_BODY_MAX_WORDS (default 500)
#   NL_ARTICLE_DRAFT_ATTEMPTS (default 2)
#   NL_ARTICLE_DRAFT_TIMEOUT_SECS (default 900)
#   NL_ARTICLE_PROMPT_CONTEXT_WARN_BYTES (default 250000)
#   NL_ARTICLE_PROMPT_CONTEXT_MAX_BYTES (default 500000)
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MERGE_INSTRUCTIONS="$SKILL_DIR/references/merge-instructions.md"

# Defaults
LIVE_WIKI="${NL_LIVE_WIKI:-/home/user/wiki/topics/libertarian}"
PUBLIC_CONTENT="${NL_PUBLIC_CONTENT:-/home/user/projects/galtland.github.io/content}"
NL_BIN="${NL_BIN:-/home/user/news-lens/target/release/news-lens}"
PROMPT_TEMPLATE="${NL_PROMPT_TEMPLATE:-/home/user/news-lens/prompts/process-post.md}"
SCRATCH="${NL_SCRATCH:-/tmp}"
MAX_NEW_ARTICLES="${NL_MAX_NEW_ARTICLES:-10}"
ARTICLE_BODY_MIN_WORDS="${NL_ARTICLE_BODY_MIN_WORDS:-150}"
ARTICLE_BODY_MAX_WORDS="${NL_ARTICLE_BODY_MAX_WORDS:-500}"
ARTICLE_DRAFT_ATTEMPTS="${NL_ARTICLE_DRAFT_ATTEMPTS:-2}"
ARTICLE_DRAFT_TIMEOUT_SECS="${NL_ARTICLE_DRAFT_TIMEOUT_SECS:-900}"
ARTICLE_PROMPT_CONTEXT_WARN_BYTES="${NL_ARTICLE_PROMPT_CONTEXT_WARN_BYTES:-250000}"
ARTICLE_PROMPT_CONTEXT_MAX_BYTES="${NL_ARTICLE_PROMPT_CONTEXT_MAX_BYTES:-500000}"
RAW_NEWS_REF_GREP_RE='raw/news/[0-9]{4}-[0-9]{2}-[0-9]{2}-[a-z0-9]+(-[a-z0-9]+)*\.md'
PUBLISH=1
LABEL=""
NEWS_TEXT=""
NEWS_SOURCE=""
SCENARIO=0

# Parse args
while [[ $# -gt 0 ]]; do
  case "$1" in
    --label)       LABEL="$2"; shift 2 ;;
    --text)        NEWS_TEXT="$2"; shift 2 ;;
    --text-file)   NEWS_TEXT="$(cat "$2")"; shift 2 ;;
    --source)      NEWS_SOURCE="$2"; shift 2 ;;
    --scenario)    SCENARIO=1; shift ;;
    --no-publish)  PUBLISH=0; shift ;;
    -h|--help)
      sed -n '2,/^set/p' "$0" | sed '$d' | sed 's/^# \?//'
      exit 0 ;;
    *) echo "Unknown arg: $1" >&2; exit 2 ;;
  esac
done

[[ -z "$LABEL" ]] && { echo "Missing --label" >&2; exit 2; }
[[ -z "$NEWS_TEXT" ]] && { echo "Missing --text or --text-file" >&2; exit 2; }

# Provenance guard: the pipeline promotes+publishes its output as a libertarian
# wiki thesis, so the triggering news must be either (a) real and attributable
# via --source, or (b) explicitly acknowledged as synthetic via --scenario (in
# which case the thesis is stamped illustrative, not passed off as a real event).
# Refusing the un-annotated case is what stops fabricated-event theses from
# going live. NL_ALLOW_UNSOURCED=1 is an explicit, logged escape hatch.
if [[ -n "$NEWS_SOURCE" && "$SCENARIO" == 1 ]]; then
  echo "Provide only one of --source or --scenario, not both." >&2; exit 2
fi
if [[ -z "$NEWS_SOURCE" && "$SCENARIO" == 0 ]]; then
  if [[ "${NL_ALLOW_UNSOURCED:-0}" == 1 ]]; then
    echo "[guard] WARNING: no --source and no --scenario; NL_ALLOW_UNSOURCED=1 set — proceeding as UNSOURCED. The thesis will be stamped as an illustrative scenario." >&2
    SCENARIO=1
  else
    echo "[guard] REFUSING: news provenance is required. Pass --source <url-or-citation> for real news, or --scenario for a synthetic/illustrative prompt (the thesis will be stamped as a scenario, not published as fact). Override with NL_ALLOW_UNSOURCED=1 only if you understand the risk." >&2
    exit 2
  fi
fi
if [[ "$SCENARIO" == 1 ]]; then
  echo "[guard] mode: SCENARIO — output will be stamped as an illustrative scenario (not a sourced event)." >&2
else
  echo "[guard] mode: SOURCED — source: $NEWS_SOURCE" >&2
fi
if ! [[ "$LABEL" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]]; then
  echo "Invalid --label '$LABEL': labels must be lowercase slug segments joined by single hyphens" >&2
  exit 2
fi
if ! [[ "$MAX_NEW_ARTICLES" =~ ^[1-9][0-9]*$ ]]; then
  echo "Invalid NL_MAX_NEW_ARTICLES '$MAX_NEW_ARTICLES': must be a positive integer" >&2
  exit 2
fi
if ! [[ "$ARTICLE_BODY_MIN_WORDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "Invalid NL_ARTICLE_BODY_MIN_WORDS '$ARTICLE_BODY_MIN_WORDS': must be a positive integer" >&2
  exit 2
fi
if ! [[ "$ARTICLE_BODY_MAX_WORDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "Invalid NL_ARTICLE_BODY_MAX_WORDS '$ARTICLE_BODY_MAX_WORDS': must be a positive integer" >&2
  exit 2
fi
if (( ARTICLE_BODY_MIN_WORDS > ARTICLE_BODY_MAX_WORDS )); then
  echo "Invalid article body word bounds: NL_ARTICLE_BODY_MIN_WORDS must be <= NL_ARTICLE_BODY_MAX_WORDS" >&2
  exit 2
fi
if ! [[ "$ARTICLE_DRAFT_ATTEMPTS" =~ ^[1-9][0-9]*$ ]]; then
  echo "Invalid NL_ARTICLE_DRAFT_ATTEMPTS '$ARTICLE_DRAFT_ATTEMPTS': must be a positive integer" >&2
  exit 2
fi
if ! [[ "$ARTICLE_DRAFT_TIMEOUT_SECS" =~ ^[1-9][0-9]*$ ]]; then
  echo "Invalid NL_ARTICLE_DRAFT_TIMEOUT_SECS '$ARTICLE_DRAFT_TIMEOUT_SECS': must be a positive integer" >&2
  exit 2
fi
if ! [[ "$ARTICLE_PROMPT_CONTEXT_WARN_BYTES" =~ ^[1-9][0-9]*$ ]]; then
  echo "Invalid NL_ARTICLE_PROMPT_CONTEXT_WARN_BYTES '$ARTICLE_PROMPT_CONTEXT_WARN_BYTES': must be a positive integer" >&2
  exit 2
fi
if ! [[ "$ARTICLE_PROMPT_CONTEXT_MAX_BYTES" =~ ^[1-9][0-9]*$ ]]; then
  echo "Invalid NL_ARTICLE_PROMPT_CONTEXT_MAX_BYTES '$ARTICLE_PROMPT_CONTEXT_MAX_BYTES': must be a positive integer" >&2
  exit 2
fi
if (( ARTICLE_PROMPT_CONTEXT_WARN_BYTES > ARTICLE_PROMPT_CONTEXT_MAX_BYTES )); then
  echo "Invalid article prompt context thresholds: NL_ARTICLE_PROMPT_CONTEXT_WARN_BYTES must be <= NL_ARTICLE_PROMPT_CONTEXT_MAX_BYTES" >&2
  exit 2
fi

LENS="$LIVE_WIKI/lens-austrian-libertarian.md"

# Preflight
echo "[preflight] checking environment…" >&2
for f in "$NL_BIN" "$PROMPT_TEMPLATE" "$LENS" "$MERGE_INSTRUCTIONS"; do
  [[ -f "$f" ]] || { echo "Missing: $f" >&2; exit 1; }
done
[[ -d "$LIVE_WIKI" ]] || { echo "Missing live wiki: $LIVE_WIKI" >&2; exit 1; }
[[ -d "$PUBLIC_CONTENT" ]] || { echo "Missing public content: $PUBLIC_CONTENT" >&2; exit 1; }
command -v claude  >/dev/null || { echo "Missing claude CLI on PATH" >&2; exit 1; }
command -v codex   >/dev/null || { echo "Missing codex CLI on PATH" >&2; exit 1; }
command -v git     >/dev/null || { echo "Missing git on PATH" >&2; exit 1; }
command -v jq      >/dev/null || { echo "Missing jq on PATH" >&2; exit 1; }
jq -n 'null // ""' >/dev/null 2>/dev/null || { echo "jq on PATH is too old: need jq >= 1.5 (// alternative operator support)" >&2; exit 1; }

OUTDIR="$SCRATCH/nl-m-out-$LABEL"
mkdir -p "$OUTDIR"

# Stage 1: detect target thesis + raw/news (if exists)
detect_target() {
  local label="$1" wiki="$2"
  local thesis=""
  for f in "$wiki/wiki/theses/"*.md; do
    [[ -f "$f" ]] || continue
    local bn; bn=$(basename "$f" .md)
    [[ "$bn" == "_index" ]] && continue
    if [[ "$bn" == *"$label"* ]]; then
      thesis="$bn.md"
      break
    fi
  done
  echo "$thesis"
}

TARGET_THESIS="$(detect_target "$LABEL" "$LIVE_WIKI")"
echo "[stage] target thesis: ${TARGET_THESIS:-<none — new news>}" >&2

# Extract focused-article slugs cited by the target thesis (for deletion)
detect_focused() {
  local thesis_path="$1"
  [[ -f "$thesis_path" ]] || return
  grep -oE '\[\[([a-z][a-z0-9-]*-on-[a-z][a-z0-9-]*)' "$thesis_path" \
    | sed 's/\[\[//' | sort -u
}

# Detect live raw/news file matching the news content
detect_raw_news() {
  local label="$1" wiki="$2"
  for f in "$wiki/raw/news/"*.md; do
    [[ -f "$f" ]] || continue
    local bn; bn=$(basename "$f" .md)
    [[ "$bn" == "_index" ]] && continue
    case "$bn" in
      *"$label"*) echo "$bn.md"; return ;;
    esac
  done
}

LIVE_RAW_NEWS="$(detect_raw_news "$LABEL" "$LIVE_WIKI")"
echo "[stage] live raw/news: ${LIVE_RAW_NEWS:-<none>}" >&2

remove_matching_index_lines() {
  local needle="$1" idx="$2" tmp idx_base
  [[ -n "$needle" ]] || return 0
  [[ -f "$idx" ]] || return 0
  idx_base="${idx##*/}"
  tmp="$(mktemp "$OUTDIR/$idx_base.tmp.XXXXXX")"
  # Empty output is valid for scratch indexes that only referenced the removed
  # slug; keeping those stale lines causes the trial agents to reuse deleted
  # work. Match the slug as a token so removing "tax" does not also remove
  # lines for "taxation" or "tax-policy".
  if ! awk -v needle="$needle" '
    function is_token_char(c) {
      return index("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-", c) > 0
    }
    function contains_token(line,    pos, offset, before, after) {
      offset = 0
      while ((pos = index(substr(line, offset + 1), needle)) > 0) {
        pos += offset
        before = pos == 1 ? "" : substr(line, pos - 1, 1)
        after = substr(line, pos + length(needle), 1)
        if ((before == "" || !is_token_char(before)) &&
            (after == "" || !is_token_char(after))) {
          return 1
        }
        offset = pos
      }
      return 0
    }
    !contains_token($0) { print }
  ' "$idx" > "$tmp"; then
    echo "[stage] failed to rewrite index $idx while removing $needle" >&2
    rm -f "$tmp"
    return 1
  fi
  if ! mv "$tmp" "$idx"; then
    echo "[stage] failed to install rewritten index $idx while removing $needle" >&2
    rm -f "$tmp"
    return 1
  fi
}

# Stage 2: stage two trial wikis (claude + codex) with thesis/focused/raw deleted.
# Also scrub stale references to the deleted thesis from indexes — otherwise the
# agent reads e.g. wiki/theses/_index.md, sees the slug listed, and may emit a
# manifest pointing at a slug whose file we just removed.
stage_trial() {
  local backend="$1"
  local dir="$SCRATCH/lib-m-$LABEL-$backend"
  rm -rf "$dir"
  cp -r "$LIVE_WIKI" "$dir"
  rm -rf "$dir/.news-lens"
  if [[ -n "$TARGET_THESIS" ]]; then
    local target_slug="${TARGET_THESIS%.md}"
    rm -f "$dir/wiki/theses/$TARGET_THESIS"
    # Strip any index lines that reference the deleted thesis slug, so the
    # agent doesn't try to "reuse" a slug whose file no longer exists.
    for idx in "$dir/wiki/theses/_index.md" "$dir/_index.md" "$dir/log.md"; do
      remove_matching_index_lines "$target_slug" "$idx"
    done
    while IFS= read -r slug; do
      [[ -n "$slug" ]] || continue
      rm -f "$dir/wiki/concepts/$slug.md"
      for idx in "$dir/wiki/concepts/_index.md" "$dir/_index.md"; do
        remove_matching_index_lines "$slug" "$idx"
      done
    done < <(detect_focused "$LIVE_WIKI/wiki/theses/$TARGET_THESIS")
  fi
  if [[ -n "$LIVE_RAW_NEWS" ]]; then
    local raw_slug="${LIVE_RAW_NEWS%.md}"
    rm -f "$dir/raw/news/$LIVE_RAW_NEWS"
    for idx in "$dir/raw/news/_index.md" "$dir/raw/_index.md" "$dir/_index.md"; do
      remove_matching_index_lines "$raw_slug" "$idx"
    done
  fi
  echo "$dir"
}

DIR_CLAUDE="$(stage_trial claude)"
DIR_CODEX="$(stage_trial codex)"
echo "[stage] trial wikis: $DIR_CLAUDE / $DIR_CODEX" >&2

# Stage 3: build toml configs
write_toml() {
  local backend="$1" dir="$2"
  local toml="$OUTDIR/$LABEL-$backend.toml"
  local cmd args
  if [[ "$backend" == "claude" ]]; then
    cmd="claude"
    # --output-format=stream-json fixes a claude --print quirk where the agent
    # sometimes exits cleanly without writing the manifest file (mode 2 in the
    # debug session). Switching to stream-json + --include-partial-messages
    # restored reliable completion. The harness reads the manifest from the
    # filesystem, not from stdout, so the change is safe.
    #
    # Tool set: Skill + Web. The "DO NOT STOP HERE" guard in the prompt
    # (added 2026-05-21) prevents the agent from treating the /wiki:query
    # result as the final answer, which was the underlying cause of mode-2
    # silent exits. Expanding past Skill+Web (e.g. adding Task/Monitor) still
    # degrades reliability — those tools push the workflow further down the
    # context window and the guard alone isn't enough. Skill is required for
    # /wiki:query, /wiki:ingest, /wiki:lint; Web gives the agent reach for
    # filling external-source gaps the wiki doesn't yet cover.
    args='["--print", "--no-session-persistence", "--output-format", "stream-json", "--include-partial-messages", "--verbose", "--permission-mode", "acceptEdits", "--allowedTools", "Bash,Edit,Write,Read,Glob,Grep,Skill,WebSearch,WebFetch"]'
  else
    cmd="codex"
    args="[\"exec\", \"--dangerously-bypass-approvals-and-sandbox\", \"-C\", \"$dir\", \"--skip-git-repo-check\"]"
  fi
  cat > "$toml" <<EOF
[general]
state_db_path = "$OUTDIR/$LABEL-$backend-state.sqlite"
log_level = "info"
dry_run = true

[wiki]
path = "$dir"

[lens]
path = "$dir/lens-austrian-libertarian.md"
id = "austrian-libertarian"

[harness]
command = "$cmd"
args = $args
prompt_template = "$PROMPT_TEMPLATE"
timeout_secs = 1800

[publish]
public_base_url = "https://index.galtland.org"

[watch]
poll_interval_secs = 300
accounts = []
include_replies = false
include_reposts = false
ignore_patterns = []

[x.read]
bearer_token_env = "X_BEARER_TOKEN"

[x.write]
enabled = false
mode = "reply"
oauth2_user_token_env = "X_USER_TOKEN"
max_chars = 280

[nostr]
enabled = false
secret_key_env = "NOSTR_NSEC"
relays = []
EOF
  echo "$toml"
}

TOML_CLAUDE="$(write_toml claude "$DIR_CLAUDE")"
TOML_CODEX="$(write_toml codex "$DIR_CODEX")"

# Stage 4: run drafts in parallel
run_draft() {
  local backend="$1" toml="$2"
  local out="$OUTDIR/$LABEL-$backend-draft.out"
  "$NL_BIN" process --config "$toml" \
    --post "$LABEL" --text "$NEWS_TEXT" --dry-run > "$out" 2>&1 \
    || echo "(draft $backend exit $?)" >&2
}

echo "[drafts] running claude + codex in parallel…" >&2
run_draft claude "$TOML_CLAUDE" &
PID_C=$!
run_draft codex "$TOML_CODEX" &
PID_X=$!
wait "$PID_C" || true
wait "$PID_X" || true

MF_CLAUDE="$DIR_CLAUDE/.news-lens/$LABEL.json"
MF_CODEX="$DIR_CODEX/.news-lens/$LABEL.json"
[[ -f "$MF_CLAUDE" ]] || echo "[warn] claude draft missing — manifest not at $MF_CLAUDE" >&2
[[ -f "$MF_CODEX"  ]] || echo "[warn] codex draft missing — manifest not at $MF_CODEX" >&2

# Find the new thesis file using the manifest's own thesis_path (the agent's own slug,
# which is often more descriptive than the scratch LABEL — e.g. "imf-conditionality-
# carbon-wage-planning" instead of "imf-conditionality-test").
find_thesis() {
  local dir="$1" mf="$2"
  local candidate base newest nullglob_was_set
  if [[ -f "$mf" ]]; then
    local rel
    rel=$(jq -r '.thesis_path // ""' "$mf" 2>/dev/null) || rel=""
    if [[ -n "$rel" && -f "$dir/$rel" ]]; then
      echo "$dir/$rel"
      return 0
    fi
  fi
  # Fallback: substring match against label, choosing the newest match without
  # GNU find extensions. Empty match sets are valid and print nothing.
  newest=""
  nullglob_was_set=0
  shopt -q nullglob && nullglob_was_set=1
  shopt -s nullglob
  for candidate in "$dir/wiki/theses/"*.md; do
    [[ -f "$candidate" ]] || continue
    base="${candidate##*/}"
    if [[ "$base" == *"$LABEL"* ]] && [[ -z "$newest" || "$candidate" -nt "$newest" ]]; then
      newest="$candidate"
    fi
  done
  if [[ "$nullglob_was_set" == 0 ]]; then
    shopt -u nullglob
  fi
  if [[ -n "$newest" ]]; then
    echo "$newest"
  fi
  return 0
}

DRAFT_CLAUDE="$(find_thesis "$DIR_CLAUDE" "$MF_CLAUDE")"
DRAFT_CODEX="$(find_thesis "$DIR_CODEX" "$MF_CODEX")"

# Fallback handling if one backend failed
SKIP_MERGE=0
if [[ -z "$DRAFT_CLAUDE" && -z "$DRAFT_CODEX" ]]; then
  echo "[fatal] both drafts failed — nothing to merge" >&2
  exit 1
fi
if [[ -z "$DRAFT_CLAUDE" ]]; then
  echo "[fallback] claude draft missing; using codex draft alone (skip merge step)" >&2
  cp "$DRAFT_CODEX" "$OUTDIR/$LABEL-merge-clean.md"
  SKIP_MERGE=1
elif [[ -z "$DRAFT_CODEX" ]]; then
  echo "[fallback] codex draft missing; using claude draft alone (skip merge step)" >&2
  cp "$DRAFT_CLAUDE" "$OUTDIR/$LABEL-merge-clean.md"
  SKIP_MERGE=1
fi

# Stage 5: build merge prompt + run merge
if [[ "$SKIP_MERGE" == 0 ]]; then
  PROMPT="$OUTDIR/$LABEL-merge-prompt.md"
  {
    cat <<'H'
You are merging two independent thesis drafts of the same news commentary into one coherent piece. Both drafts follow the same editorial lens (below). Each has strengths and weaknesses. Your job is to produce a merged thesis that is BETTER than either — preserving the best argumentative moves of each, eliminating redundancy, and reading as one writer's voice (not stitched together).

# Editorial lens (in effect for the merge)

H
    cat "$LENS"
    printf '\n# The news item\n\n"%s"\n\n# Draft A (Claude)\n\n' "$NEWS_TEXT"
    cat "$DRAFT_CLAUDE"
    printf '\n# Draft B (Codex)\n\n'
    cat "$DRAFT_CODEX"
    printf '\n'
    cat "$MERGE_INSTRUCTIONS"
  } > "$PROMPT"

  # Merge with claude (better at the synthesized opening than codex was — codex
  # tended to weld both drafts' framings into one over-compressed lede). Mirror
  # the claude article-draft invocation: prompt on stdin, read-only tools, cwd
  # at SCRATCH (matches the old codex -C "$SCRATCH"). No timeout wrapper here —
  # run_with_optional_timeout is defined later in the file, and the workflow's
  # job-level timeout is the backstop, as it was for the old codex merge.
  echo "[merge] running claude with merge prompt ($(wc -l < "$PROMPT") lines)…" >&2
  (
    cd "$SCRATCH" || exit 1
    claude --print --no-session-persistence --max-turns 5 --permission-mode default \
      --allowedTools "Read,Glob,Grep" \
      --disallowedTools "Bash,Edit,Write" \
      < "$PROMPT" > "$OUTDIR/$LABEL-merge.out" 2> "$OUTDIR/$LABEL-merge.err"
  ) || echo "[merge] claude merge exited non-zero (see $OUTDIR/$LABEL-merge.err)" >&2

  # Stage 6: extract clean merge — keep content from the first frontmatter
  # delimiter, ignoring any CLI accounting prelude (matches extract_article_content).
  awk 'found || /^---$/ { found=1; print }' "$OUTDIR/$LABEL-merge.out" \
    > "$OUTDIR/$LABEL-merge-clean.md"

  # Fallback: claude's content filter can non-deterministically block output on
  # some topics (revolution/resistance framings), yielding empty/frontmatter-less
  # output. Fall back to the codex merge so those items still produce a thesis.
  if [[ ! -s "$OUTDIR/$LABEL-merge-clean.md" ]]; then
    echo "[merge] claude produced no usable merge; falling back to codex exec" >&2
    codex exec --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check -C "$SCRATCH" \
      < "$PROMPT" > "$OUTDIR/$LABEL-merge.out" 2>&1 \
      || echo "[merge] codex fallback exited non-zero" >&2
    awk '/^tokens used$/{p=1; next} p' "$OUTDIR/$LABEL-merge.out" \
      | sed '1{/^[0-9,]*$/d}' > "$OUTDIR/$LABEL-merge-clean.md"
  fi
fi

CLEAN="$OUTDIR/$LABEL-merge-clean.md"
[[ -s "$CLEAN" ]] || { echo "[fatal] merge clean output empty" >&2; exit 1; }

# Voice rule sanity check
WIKI_MENTIONS=$(grep -c "the wiki\|wiki's" "$CLEAN" 2>/dev/null) || WIKI_MENTIONS=0
if [[ "$WIKI_MENTIONS" -gt 0 ]]; then
  echo "[warn] merged thesis has $WIKI_MENTIONS 'the wiki' mentions — lens voice rule violated" >&2
fi

extract_wikilink_targets() {
  local file="$1"
  [[ -f "$file" ]] || return 0
  # Match the newsroom resolver: strip display text and section anchors, but
  # do not normalize case or spacing. Malformed targets must fail loudly.
  awk -v file="$file" '
    BEGIN { bad = 0 }
    {
      rest = $0
      while ((start = index(rest, "[[")) > 0) {
        rest = substr(rest, start + 2)
        close_pos = index(rest, "]]")
        if (close_pos == 0) {
          malformed = rest
          sub(/[[:space:]]+$/, "", malformed)
          if (malformed == "") malformed = "<empty>"
          printf "[promote] FATAL: malformed wikilink without closing ]] in %s:%d: [[%s\n", file, NR, malformed > "/dev/stderr"
          bad = 1
          break
        }
        link_text = substr(rest, 1, close_pos - 1)
        stop = 0
        for (i = 1; i <= length(link_text); i++) {
          c = substr(link_text, i, 1)
          if (c == "]" || c == "#" || c == "|") {
            stop = i
            break
          }
        }
        target = stop ? substr(link_text, 1, stop - 1) : link_text
        trimmed = target
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", trimmed)
        if (trimmed == "") {
          printf "[promote] FATAL: empty wikilink target in %s:%d: [[%s]]\n", file, NR, link_text > "/dev/stderr"
          bad = 1
        } else if (target != trimmed) {
          printf "[promote] FATAL: wikilink target has surrounding whitespace in %s:%d: [[%s]]; use [[%s]]\n", file, NR, link_text, trimmed > "/dev/stderr"
          bad = 1
        } else {
          print target
        }
        rest = substr(rest, close_pos + 2)
      }
    }
    END { exit bad ? 1 : 0 }
  ' "$file" \
    | sort -u
}

count_wikilink_targets() {
  local file="$1"
  extract_wikilink_targets "$file" | awk 'END { print NR + 0 }'
}

line_count_file() {
  local file="$1"
  awk 'END { print NR + 0 }' "$file"
}

relative_path_under() {
  local path="$1" root="$2" prefix
  prefix="$root/"
  case "$path" in
    "$prefix"*) printf '%s\n' "${path:${#prefix}}" ;;
    *) printf '%s\n' "$path" ;;
  esac
}

wiki_inventory_contains() {
  local target="$1" wiki_inventory="$2"
  if [[ -f "$wiki_inventory" ]]; then
    if grep -Fxq -- "$target" "$wiki_inventory"; then
      return 0
    fi
  fi
  return 1
}

add_inventory_slug() {
  local target="$1" wiki_inventory="$2"
  [[ -n "$target" ]] || return 0
  printf '%s\n' "$target" >> "$wiki_inventory"
  sort -u "$wiki_inventory" -o "$wiki_inventory"
}

wiki_article_file_exists() {
  local target="$1" wiki_inventory="${2:-}" match bn
  if [[ -n "$wiki_inventory" && -f "$wiki_inventory" ]]; then
    wiki_inventory_contains "$target" "$wiki_inventory"
    return
  fi
  [[ -d "$LIVE_WIKI/wiki" ]] || return 1
  while IFS= read -r match; do
    bn="${match##*/}"
    bn="${bn%.md}"
    [[ "$bn" == "$target" ]] && return 0
  done < <(find "$LIVE_WIKI/wiki" -type f -name '*.md' -print)
  return 1
}

missing_wikilinks() {
  local file="$1" wiki_inventory="$2"
  local target targets_file
  targets_file="$(mktemp "$OUTDIR/$LABEL-wikilink-targets.XXXXXX")"
  if ! extract_wikilink_targets "$file" > "$targets_file"; then
    rm -f "$targets_file"
    return 1
  fi
  while IFS= read -r target; do
    [[ -n "$target" ]] || continue
    if ! wiki_inventory_contains "$target" "$wiki_inventory"; then
      printf '%s\n' "$target"
    fi
  done < "$targets_file"
  rm -f "$targets_file"
}

write_wiki_inventory() {
  local out="$1"
  local path bn
  : > "$out"
  while IFS= read -r path; do
    bn="$(basename "$path" .md)"
    [[ "$bn" == "_index" ]] && continue
    printf '%s\n' "$bn"
  done < <(find "$LIVE_WIKI/wiki" -type f -name '*.md' -print) \
    | sort -u > "$out"
}

write_reference_inventory() {
  local out="$1"
  local path bn
  : > "$out"
  [[ -d "$LIVE_WIKI/wiki/references" ]] || return 0
  while IFS= read -r path; do
    bn="$(basename "$path" .md)"
    [[ "$bn" == "_index" ]] && continue
    printf '%s\n' "$bn"
  done < <(find "$LIVE_WIKI/wiki/references" -maxdepth 1 -type f -name '*.md' -print) \
    | sort -u > "$out"
}

write_source_inventory() {
  local out="$1"
  local path rel
  : > "$out"
  [[ -d "$LIVE_WIKI/raw/articles" ]] || return 0
  while IFS= read -r path; do
    rel="$(relative_path_under "$path" "$LIVE_WIKI")"
    [[ "$rel" == */_index.md ]] && continue
    printf '%s\n' "$rel"
  done < <(find "$LIVE_WIKI/raw/articles" -type f -name '*.md' -print) \
    | sort -u > "$out"
}

file_size_bytes() {
  local file="$1"
  wc -c < "$file" | awk '{ print $1 + 0 }'
}

check_article_prompt_context_size() {
  local wiki_inventory="$1" source_inventory="$2"
  local lens_bytes wiki_bytes source_bytes total
  lens_bytes="$(file_size_bytes "$LENS")"
  wiki_bytes="$(file_size_bytes "$wiki_inventory")"
  source_bytes="$(file_size_bytes "$source_inventory")"
  total=$((lens_bytes + wiki_bytes + source_bytes))
  if (( total > ARTICLE_PROMPT_CONTEXT_WARN_BYTES )); then
    echo "[promote] warn: article drafting prompt context is ${total} bytes before instructions" >&2
    echo "[promote] warn: lens=${lens_bytes} wiki_inventory=${wiki_bytes} source_inventory=${source_bytes}; threshold NL_ARTICLE_PROMPT_CONTEXT_WARN_BYTES=$ARTICLE_PROMPT_CONTEXT_WARN_BYTES" >&2
  fi
  if (( total > ARTICLE_PROMPT_CONTEXT_MAX_BYTES )); then
    echo "[promote] FATAL: article drafting prompt context is ${total} bytes before instructions" >&2
    echo "[promote] FATAL: lens=${lens_bytes} wiki_inventory=${wiki_bytes} source_inventory=${source_bytes}; maximum NL_ARTICLE_PROMPT_CONTEXT_MAX_BYTES=$ARTICLE_PROMPT_CONTEXT_MAX_BYTES" >&2
    return 1
  fi
}

valid_new_article_target() {
  local target="$1"
  # Focused wiki article slugs intentionally start with a letter. Numeric or
  # date-prefixed thesis wikilinks fail loud instead of being papered over.
  [[ "$target" =~ ^[a-z][a-z0-9]*(-[a-z0-9]+)*$ ]]
}

reference_slug_hint() {
  local target="$1" reference_inventory="$2"
  # Keep classification data-driven: the live references directory is the
  # source of truth for known reference slugs. Genuinely new or ambiguous
  # targets default to concepts and can be promoted by an operator later.
  wiki_inventory_contains "$target" "$reference_inventory"
}

classify_new_article() {
  local target="$1" reference_inventory="$2"
  if reference_slug_hint "$target" "$reference_inventory"; then
    echo "reference"
  else
    echo "concept"
  fi
}

article_dest_for_kind() {
  local kind="$1" target="$2"
  case "$kind" in
    concept) echo "$LIVE_WIKI/wiki/concepts/$target.md" ;;
    reference) echo "$LIVE_WIKI/wiki/references/$target.md" ;;
    *)
      echo "[promote] FATAL: unknown new article kind '$kind' for $target" >&2
      return 2
      ;;
  esac
}

select_article_backend() {
  local skip_merge="$1" draft_codex="$2"
  # Codex drafted leg B, so prefer it for the citation-completion stub articles
  # (the merge itself now runs on claude, with codex only as the merge fallback).
  if [[ "$skip_merge" == 0 || -n "$draft_codex" ]]; then
    echo "codex"
  else
    echo "claude"
  fi
}

prepare_article_draft_workspace() {
  local workdir="$1"
  [[ -n "$workdir" ]] || { echo "article draft workdir is empty"; return 1; }
  [[ "$workdir" != "$LIVE_WIKI" ]] || { echo "article draft workdir must be a scratch copy, not LIVE_WIKI"; return 1; }
  [[ -d "$workdir/wiki" ]] || { echo "missing wiki directory in article draft workdir: $workdir"; return 1; }
}

create_article_draft_workspace() {
  local workdir="$1"
  [[ -n "$workdir" ]] || { echo "article draft workdir is empty"; return 1; }
  if ! rm -rf "$workdir"; then
    echo "failed to remove stale article draft workdir: $workdir"
    return 1
  fi
  if ! cp -R "$LIVE_WIKI" "$workdir"; then
    echo "failed to copy live wiki into article draft workdir: $workdir"
    return 1
  fi
  prepare_article_draft_workspace "$workdir"
}

write_article_prompt() {
  local target="$1" kind="$2" wiki_inventory="$3" source_inventory="$4" prompt="$5" today="$6"
  local lens_file="$LENS" body_min_words="$ARTICLE_BODY_MIN_WORDS" body_max_words="$ARTICLE_BODY_MAX_WORDS"
  [[ -f "$lens_file" ]] || { echo "Missing lens file: $lens_file" >&2; return 1; }
  [[ "$body_min_words" =~ ^[1-9][0-9]*$ ]] || { echo "Invalid article body min words: $body_min_words" >&2; return 1; }
  [[ "$body_max_words" =~ ^[1-9][0-9]*$ ]] || { echo "Invalid article body max words: $body_max_words" >&2; return 1; }
  {
    cat <<'ARTICLE_PROMPT'
You are writing a focused wiki article from the Austrian-libertarian
editorial lens.

You are running in a scratch copy of the live wiki for this drafting
call. Read files if needed, but do not create or edit files; return the
finished markdown article on stdout only.

Editorial lens (full text):
ARTICLE_PROMPT
    cat "$lens_file"
    cat <<'ARTICLE_PROMPT'

Valid wiki inventory (filenames only, for valid [[wikilink]] targets
in your body):
ARTICLE_PROMPT
    cat "$wiki_inventory"
    cat <<'ARTICLE_PROMPT'

Available source files (use these for frontmatter sources and quote
attribution when relevant):
ARTICLE_PROMPT
    cat "$source_inventory"
    printf '\nTask: write the article for slug "%s" classified as a\n%s (concept or reference).\n\n' "$target" "$kind"
    cat <<'ARTICLE_PROMPT'
The output must be a single markdown file with this exact shape:

---
title: "<reader-facing title - convert the slug to title case>"
volatility: warm
category: <concept|reference>
sources:
  - <one or more exact paths from the available source files>
ARTICLE_PROMPT
    printf 'created: %s\nupdated: %s\n' "$today" "$today"
    cat <<'ARTICLE_PROMPT'
tags: [<3-6 tags>]
aliases: [<1-3 reader-friendly alias names>]
short: "<one-sentence summary of the article's core claim>"
---

# <title>

ARTICLE_PROMPT
    printf 'Body: target %d to %d words.\n' "$body_min_words" "$body_max_words"
    cat <<'ARTICLE_PROMPT'
State the core claim in 1-2 sentences.
Include at least one direct quote from a cited source, formatted
as a markdown blockquote. Link to related wiki articles using
[[wikilink|Display Text]] form, but ONLY to articles already in
the inventory above. End with a short "See Also" bullet list of
2-4 related articles (also from the inventory).

Constraints:
ARTICLE_PROMPT
    printf -- '- Keep the body between %d and %d words (excluding frontmatter).\n' "$body_min_words" "$body_max_words"
    cat <<'ARTICLE_PROMPT'
- Every [[wikilink]] in your body MUST be in the inventory list.
- Every frontmatter source MUST be copied exactly from the available
  source files list above.
- Quote at least one source verbatim.
- Match the tone of the editorial lens.
- Output ONLY the markdown file. No commentary before or after.
ARTICLE_PROMPT
  } > "$prompt"
}

run_with_optional_timeout() {
  local timeout_secs="$1"
  shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$timeout_secs" "$@"
  else
    "$@"
  fi
}

run_article_backend() {
  local backend="$1" prompt="$2" raw="$3" err="$4" workdir="$5"
  local transcript
  case "$backend" in
    codex)
      transcript="$raw.transcript"
      run_with_optional_timeout "$ARTICLE_DRAFT_TIMEOUT_SECS" \
        codex exec --sandbox read-only --skip-git-repo-check -C "$workdir" \
          --output-last-message "$raw" \
        < "$prompt" > "$transcript" 2> "$err"
      ;;
    claude)
      (
        cd "$workdir" || exit 1
        run_with_optional_timeout "$ARTICLE_DRAFT_TIMEOUT_SECS" \
          claude --print --no-session-persistence --max-turns 5 --permission-mode default \
          --allowedTools "Read,Glob,Grep" \
          --disallowedTools "Bash,Edit,Write" \
          < "$prompt" > "$raw" 2> "$err"
      )
      ;;
    *)
      echo "unknown article backend: $backend" >&2
      return 2
      ;;
  esac
}

extract_article_content() {
  local raw="$1" clean="$2"
  # Keep content from the first frontmatter delimiter. This ignores any CLI
  # accounting prelude without depending on its wording.
  if ! grep -q '^---$' "$raw" 2>/dev/null; then
    rm -f "$clean"
    echo "backend output contains no markdown frontmatter delimiter"
    return 1
  fi
  awk 'found || /^---$/ { found=1; print }' "$raw" > "$clean"
}

extract_article_sources() {
  local file="$1" closing_line="$2"
  awk -v end="$closing_line" '
    NR <= 1 || NR >= end { next }
    /^sources:[[:space:]]*$/ { in_sources = 1; next }
    in_sources && /^[[:space:]]*-[[:space:]]*/ {
      item = $0
      sub(/^[[:space:]]*-[[:space:]]*/, "", item)
      gsub(/^[[:space:]"\047]+|[[:space:]"\047]+$/, "", item)
      if (item != "") print item
      next
    }
    in_sources && /^[^[:space:]][^:]*:/ { in_sources = 0 }
  ' "$file"
}

validate_article_sources() {
  local file="$1" closing_line="$2" source_inventory="$3"
  local source seen
  seen=0
  [[ -s "$source_inventory" ]] || { echo "source inventory is empty"; return 1; }
  if awk -v end="$closing_line" 'NR > 1 && NR < end && /^sources:[[:space:]]*\[/ { found = 1 } END { exit found ? 0 : 1 }' "$file"; then
    echo "sources must use a YAML block list, not a flow sequence"
    return 1
  fi
  while IFS= read -r source; do
    [[ -n "$source" ]] || continue
    seen=1
    if ! grep -Fxq -- "$source" "$source_inventory"; then
      echo "frontmatter source not in source inventory: $source"
      return 1
    fi
  done < <(extract_article_sources "$file" "$closing_line")
  [[ "$seen" == 1 ]] || { echo "missing non-empty sources list"; return 1; }
}

validate_article_content() {
  local file="$1" kind="$2" wiki_inventory="$3" source_inventory="$4"
  local first closing_line title category body_words link link_targets bad_link
  [[ -s "$file" ]] || { echo "empty output"; return 1; }
  IFS= read -r first < "$file" || first=""
  [[ "$first" == "---" ]] || { echo "missing opening frontmatter delimiter"; return 1; }
  closing_line="$(awk 'NR > 1 && /^---$/ { print NR; exit }' "$file")"
  [[ -n "$closing_line" ]] || { echo "missing closing frontmatter delimiter"; return 1; }
  title="$(awk -v end="$closing_line" 'NR <= 1 || NR >= end { next } /^title:[[:space:]]*/ { sub(/^title:[[:space:]]*/, ""); gsub(/^[[:space:]"\047]+|[[:space:]"\047]+$/, ""); print; exit }' "$file")"
  [[ -n "$title" ]] || { echo "missing non-empty title"; return 1; }
  category="$(awk -v end="$closing_line" 'NR <= 1 || NR >= end { next } /^category:[[:space:]]*/ { sub(/^category:[[:space:]]*/, ""); gsub(/^[[:space:]"\047]+|[[:space:]"\047]+$/, ""); print; exit }' "$file")"
  [[ "$category" == "$kind" ]] || { echo "category '$category' does not match '$kind'"; return 1; }
  validate_article_sources "$file" "$closing_line" "$source_inventory" || return 1
  body_words="$(awk -v start="$closing_line" '
    BEGIN { c = 0 }
    NR <= start { next }
    !seen_body && /^[[:space:]]*$/ { next }
    !seen_body && /^#[[:space:]]+/ { seen_body = 1; next }
    { seen_body = 1; for (i = 1; i <= NF; i++) c++ }
    END { print c + 0 }
  ' "$file")"
  [[ "$body_words" -gt 0 ]] || { echo "empty body"; return 1; }
  [[ "$body_words" -ge "$ARTICLE_BODY_MIN_WORDS" ]] || { echo "body too short: $body_words words; minimum is $ARTICLE_BODY_MIN_WORDS"; return 1; }
  [[ "$body_words" -le "$ARTICLE_BODY_MAX_WORDS" ]] || { echo "body too long: $body_words words"; return 1; }
  if ! awk -v start="$closing_line" '
    NR > start && /^[[:space:]]*>[[:space:]]*[^[:space:]>]/ { found = 1 }
    END { exit found ? 0 : 1 }
  ' "$file"; then
    echo "missing non-empty markdown blockquote"
    return 1
  fi
  if grep -q '^```' "$file"; then
    echo "output contains a markdown code fence"
    return 1
  fi
  link_targets="$file.wikilinks"
  if ! extract_wikilink_targets "$file" > "$link_targets"; then
    rm -f "$link_targets"
    echo "article contains malformed wikilink markup"
    return 1
  fi
  bad_link=""
  while IFS= read -r link; do
    [[ -n "$link" ]] || continue
    if ! grep -Fxq -- "$link" "$wiki_inventory"; then
      bad_link="$link"
      break
    fi
  done < "$link_targets"
  rm -f "$link_targets"
  if [[ -n "$bad_link" ]]; then
    echo "article contains wikilink not in inventory: $bad_link"
    return 1
  fi
}

draft_new_article() {
  local target="$1" kind="$2" wiki_inventory="$3" source_inventory="$4" backend="$5"
  local workdir="$6" final="$7"
  local attempt prompt raw err clean today reason last_reason backend_status
  today="$(date +%Y-%m-%d)"
  last_reason="draft did not run"
  if ! reason="$(prepare_article_draft_workspace "$workdir")"; then
    printf '%s\n' "$reason"
    return 1
  fi
  for ((attempt = 1; attempt <= ARTICLE_DRAFT_ATTEMPTS; attempt++)); do
    prompt="$OUTDIR/$LABEL-article-$target-attempt-$attempt.prompt.md"
    raw="$OUTDIR/$LABEL-article-$target-attempt-$attempt.raw"
    err="$OUTDIR/$LABEL-article-$target-attempt-$attempt.err"
    clean="$OUTDIR/$LABEL-article-$target-attempt-$attempt.md"
    if ! reason="$(write_article_prompt "$target" "$kind" "$wiki_inventory" "$source_inventory" "$prompt" "$today" 2>&1)"; then
      last_reason="failed to build article prompt: $reason"
      echo "[promote] article draft $target attempt $attempt failed: $last_reason" >&2
      continue
    fi
    backend_status=0
    run_article_backend "$backend" "$prompt" "$raw" "$err" "$workdir" || backend_status=$?
    if [[ "$backend_status" != 0 ]]; then
      if [[ "$backend_status" == 124 ]]; then
        last_reason="$backend timed out after ${ARTICLE_DRAFT_TIMEOUT_SECS}s; see $err"
      else
        last_reason="$backend exited non-zero ($backend_status); see $err"
      fi
      echo "[promote] article draft $target attempt $attempt failed: $last_reason" >&2
      continue
    fi
    if ! reason="$(extract_article_content "$raw" "$clean" 2>&1)"; then
      last_reason="failed to extract markdown article from backend output: $reason"
      echo "[promote] article draft $target attempt $attempt failed: $last_reason" >&2
      continue
    fi
    if reason="$(validate_article_content "$clean" "$kind" "$wiki_inventory" "$source_inventory")"; then
      if cp "$clean" "$final"; then
        return 0
      fi
      last_reason="failed to stage validated article at $final"
      echo "[promote] article draft $target attempt $attempt failed: $last_reason" >&2
      continue
    fi
    last_reason="$reason"
    echo "[promote] article draft $target attempt $attempt failed: $last_reason" >&2
  done
  printf '%s\n' "$last_reason"
  return 1
}

record_drafting_attempt() {
  local attempts_file="$1" target="$2" status="$3"
  printf '%s\t%s\n' "$target" "$status" >> "$attempts_file"
}

report_unresolved_wikilinks() {
  local unresolved_file="$1" attempts_file="$2" cap_hit="$3"
  local count target status
  count="$(line_count_file "$unresolved_file")"
  echo "[promote] FATAL: $count wikilink(s) in the thesis still unresolved" >&2
  echo "after the citation-completion pass:" >&2
  while IFS= read -r target; do
    [[ -n "$target" ]] || continue
    echo "  - $target" >&2
  done < "$unresolved_file"
  echo "Drafting attempts:" >&2
  if [[ -s "$attempts_file" ]]; then
    while IFS=$'\t' read -r target status; do
      [[ -n "$target" ]] || continue
      echo "  - $target: $status" >&2
    done < "$attempts_file"
  else
    echo "  - none: not attempted" >&2
  fi
  echo "Cap (NL_MAX_NEW_ARTICLES=$MAX_NEW_ARTICLES) hit: $cap_hit" >&2
  echo "See wiki repo issue #11 for the design decision." >&2
}

PROMOTE_ACTIVE_TMP=""
PROMOTE_ROLLBACK_ACTIVE=0
PROMOTE_BACKUP=""
PROMOTE_PREEXISTED=0
PROMOTE_ROLLBACK_TARGET=""
PROMOTE_INSTALLED_ARTICLES_FILE=""
PROMOTE_HAS_PREVIOUS_EXIT_TRAP=0
PROMOTE_PREVIOUS_EXIT_TRAP_ACTION=""

cleanup_promote_tmp_files() {
  [[ -n "$PROMOTE_ACTIVE_TMP" ]] || return 0
  rm -f "$PROMOTE_ACTIVE_TMP"
  PROMOTE_ACTIVE_TMP=""
}

rollback_promote_writes() {
  local path
  if [[ -f "$PROMOTE_INSTALLED_ARTICLES_FILE" ]]; then
    while IFS= read -r path; do
      [[ -n "$path" ]] || continue
      rm -f "$path" || true
    done < "$PROMOTE_INSTALLED_ARTICLES_FILE"
  fi

  if [[ "$PROMOTE_PREEXISTED" == 1 && -f "$PROMOTE_BACKUP" ]]; then
    cp "$PROMOTE_BACKUP" "$PROMOTE_ROLLBACK_TARGET" || true
  else
    rm -f "$PROMOTE_ROLLBACK_TARGET" || true
  fi
  PROMOTE_ROLLBACK_ACTIVE=0
  echo "[promote] rolled back thesis/article/raw writes after failure" >&2
}

cleanup_promote_on_exit() {
  local status=$? restore_errexit=0
  if [[ "$PROMOTE_ROLLBACK_ACTIVE" == 1 && "$status" != 0 ]]; then
    rollback_promote_writes
  fi
  cleanup_promote_tmp_files
  if [[ "$PROMOTE_HAS_PREVIOUS_EXIT_TRAP" == 1 && -n "$PROMOTE_PREVIOUS_EXIT_TRAP_ACTION" ]]; then
    case "$-" in
      *e*) restore_errexit=1; set +e ;;
    esac
    ( exit "$status" )
    eval "$PROMOTE_PREVIOUS_EXIT_TRAP_ACTION"
    if [[ "$restore_errexit" == 1 ]]; then
      set -e
    fi
  fi
  return "$status"
}

install_promote_exit_trap() {
  local existing_trap action
  existing_trap="$(trap -p EXIT || true)"
  if [[ -n "$existing_trap" ]]; then
    action="${existing_trap#trap -- }"
    action="${action% EXIT}"
    if ! eval "PROMOTE_PREVIOUS_EXIT_TRAP_ACTION=$action"; then
      echo "[promote] FATAL: could not preserve pre-existing EXIT trap: $existing_trap" >&2
      exit 1
    fi
    PROMOTE_HAS_PREVIOUS_EXIT_TRAP=1
  fi
  trap cleanup_promote_on_exit EXIT
}

begin_promote_rollback() {
  local target="$1" installed_articles_file="$2"
  PROMOTE_ROLLBACK_TARGET="$target"
  PROMOTE_INSTALLED_ARTICLES_FILE="$installed_articles_file"
  : > "$PROMOTE_INSTALLED_ARTICLES_FILE"
  PROMOTE_BACKUP="$OUTDIR/$LABEL-promote-target.before"
  if [[ -f "$target" ]]; then
    cp "$target" "$PROMOTE_BACKUP"
    PROMOTE_PREEXISTED=1
  else
    rm -f "$PROMOTE_BACKUP"
    PROMOTE_PREEXISTED=0
  fi
  PROMOTE_ROLLBACK_ACTIVE=1
}

finish_promote_rollback() {
  PROMOTE_ROLLBACK_ACTIVE=0
  PROMOTE_ROLLBACK_TARGET=""
  PROMOTE_INSTALLED_ARTICLES_FILE=""
  rm -f "$PROMOTE_BACKUP"
}

install_file_without_overwrite() {
  local src="$1" dest="$2" tmp
  mkdir -p "$(dirname "$dest")" || return 1
  [[ ! -e "$dest" ]] || return 2
  tmp="$dest.tmp.$$"
  PROMOTE_ACTIVE_TMP="$tmp"
  rm -f "$tmp"
  if ! cp "$src" "$tmp"; then
    cleanup_promote_tmp_files
    return 1
  fi
  # tmp is created in the destination directory, so this hard link is an
  # atomic no-overwrite install. If the filesystem refuses hard links, fail
  # loud instead of falling back to a non-portable or overwriting move.
  if ln "$tmp" "$dest"; then
    cleanup_promote_tmp_files
    return 0
  fi
  cleanup_promote_tmp_files
  [[ -e "$dest" ]] && return 2
  return 1
}

valid_raw_news_ref() {
  local raw_ref="$1"
  [[ "$raw_ref" =~ ^${RAW_NEWS_REF_GREP_RE}$ ]]
}

resolve_required_raw_ref() {
  local thesis="$1"
  shift
  local raw_ref mf candidate

  raw_ref="$(grep -oE "$RAW_NEWS_REF_GREP_RE" "$thesis" | head -1 || true)"
  if [[ -n "$raw_ref" ]]; then
    printf '%s\n' "$raw_ref"
    return 0
  fi

  for mf in "$@"; do
    [[ -f "$mf" ]] || continue
    candidate="$(jq -r '.raw_path // ""' "$mf" 2>/dev/null || true)"
    if valid_raw_news_ref "$candidate"; then
      if ! find_raw_news_source "$candidate" >/dev/null; then
        echo "[promote] raw/news reference absent from thesis body; skipping unavailable manifest raw_path from $mf: $candidate" >&2
        continue
      fi
      echo "[promote] raw/news reference absent from thesis body; using manifest raw_path from $mf: $candidate" >&2
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  echo "[promote] FATAL: merged thesis has no safe raw/news/*.md reference and no usable backend manifest raw_path" >&2
  echo "[promote] FATAL: refusing to promote a thesis with empty raw_path/raw_slug in the final manifest" >&2
  return 1
}

find_raw_news_source() {
  local raw_ref="$1" live d
  [[ -n "$raw_ref" ]] || return 1
  live="$LIVE_WIKI/$raw_ref"
  if [[ -f "$live" ]]; then
    printf '%s\n' "$live"
    return 0
  fi
  for d in "$DIR_CODEX" "$DIR_CLAUDE"; do
    if [[ -f "$d/$raw_ref" ]]; then
      printf '%s\n' "$d/$raw_ref"
      return 0
    fi
  done
  return 1
}

verify_raw_news_available() {
  local raw_ref="$1"
  if ! find_raw_news_source "$raw_ref" >/dev/null; then
    echo "[promote] FATAL: raw/news reference not found in live wiki or trial wikis before thesis promotion: $raw_ref" >&2
    return 1
  fi
}

promote_raw_news_if_needed() {
  local raw_ref="$1" installed_paths_file="${2:-}" live src install_status
  [[ -n "$raw_ref" ]] || return 0
  live="$LIVE_WIKI/$raw_ref"
  [[ -f "$live" ]] && return 0

  if ! src="$(find_raw_news_source "$raw_ref")"; then
    echo "[promote] FATAL: raw/news reference not found in trial wikis before thesis promotion: $raw_ref" >&2
    return 1
  fi
  install_status=0
  install_file_without_overwrite "$src" "$live" || install_status=$?
  if [[ "$install_status" == "0" ]]; then
    [[ -z "$installed_paths_file" ]] || printf '%s\n' "$live" >> "$installed_paths_file"
    echo "[promote] new raw/news: $raw_ref" >&2
    return 0
  fi
  if [[ "$install_status" == "2" ]]; then
    if [[ -f "$live" ]]; then
      echo "[promote] raw/news appeared before write: $raw_ref" >&2
      return 0
    fi
  fi
  echo "[promote] FATAL: failed to promote raw/news before thesis promotion: $raw_ref" >&2
  return 1
}

complete_citation_coverage() {
  local thesis_source="$1" repo_root="$2" new_articles_jsonl="$3" staged_articles_tsv="$4" attempts_file="$5"
  local missing_file wiki_inventory article_inventory planned_inventory reference_inventory source_inventory
  local unresolved_file
  local missing_count cap_hit target kind dest article backend rel failure_reason pending_thesis_slug article_draft_workdir

  missing_file="$OUTDIR/$LABEL-missing-wikilinks.txt"
  wiki_inventory="$OUTDIR/$LABEL-live-wiki-inventory.txt"
  article_inventory="$OUTDIR/$LABEL-article-link-inventory.txt"
  planned_inventory="$OUTDIR/$LABEL-planned-wiki-inventory.txt"
  reference_inventory="$OUTDIR/$LABEL-live-reference-inventory.txt"
  source_inventory="$OUTDIR/$LABEL-live-source-inventory.txt"
  unresolved_file="$OUTDIR/$LABEL-unresolved-wikilinks.txt"
  : > "$attempts_file"
  : > "$new_articles_jsonl"
  : > "$staged_articles_tsv"

  write_wiki_inventory "$wiki_inventory"
  cp "$wiki_inventory" "$article_inventory"
  cp "$wiki_inventory" "$planned_inventory"
  pending_thesis_slug="$(basename "$PROMOTE_TARGET" .md)"
  add_inventory_slug "$pending_thesis_slug" "$planned_inventory"
  missing_wikilinks "$thesis_source" "$planned_inventory" > "$missing_file"
  missing_count="$(line_count_file "$missing_file")"
  if [[ "$missing_count" == "0" ]]; then
    echo "[promote] citation coverage complete: all thesis wikilinks already resolve" >&2
    return 0
  fi

  cap_hit="no"
  if (( missing_count > MAX_NEW_ARTICLES )); then
    cap_hit="yes"
    while IFS= read -r target; do
      [[ -n "$target" ]] || continue
      record_drafting_attempt "$attempts_file" "$target" "failed: not attempted because missing wikilinks exceed cap"
    done < "$missing_file"
    report_unresolved_wikilinks "$missing_file" "$attempts_file" "$cap_hit"
    return 1
  fi

  write_reference_inventory "$reference_inventory"
  write_source_inventory "$source_inventory"
  if ! check_article_prompt_context_size "$article_inventory" "$source_inventory"; then
    while IFS= read -r target; do
      [[ -n "$target" ]] || continue
      record_drafting_attempt "$attempts_file" "$target" "failed: not attempted because article drafting prompt context exceeds NL_ARTICLE_PROMPT_CONTEXT_MAX_BYTES"
    done < "$missing_file"
    report_unresolved_wikilinks "$missing_file" "$attempts_file" "$cap_hit"
    return 1
  fi
  if [[ ! -s "$source_inventory" ]]; then
    echo "[promote] FATAL: raw/articles source inventory is empty; cannot draft cited articles with required sources" >&2
    while IFS= read -r target; do
      [[ -n "$target" ]] || continue
      record_drafting_attempt "$attempts_file" "$target" "failed: not attempted because raw/articles source inventory is empty"
    done < "$missing_file"
    report_unresolved_wikilinks "$missing_file" "$attempts_file" "$cap_hit"
    return 1
  fi
  article_draft_workdir="$OUTDIR/$LABEL-article-draft-wiki"
  if ! failure_reason="$(create_article_draft_workspace "$article_draft_workdir" 2>&1)"; then
    echo "[promote] FATAL: cannot prepare scratch wiki for article drafting: $failure_reason" >&2
    while IFS= read -r target; do
      [[ -n "$target" ]] || continue
      record_drafting_attempt "$attempts_file" "$target" "failed: not attempted because scratch wiki preparation failed"
    done < "$missing_file"
    report_unresolved_wikilinks "$missing_file" "$attempts_file" "$cap_hit"
    return 1
  fi
  backend="$(select_article_backend "$SKIP_MERGE" "$DRAFT_CODEX")"
  echo "[promote] citation completion: drafting $missing_count missing article(s) with $backend" >&2

  while IFS= read -r target; do
    [[ -n "$target" ]] || continue
    if ! valid_new_article_target "$target"; then
      record_drafting_attempt "$attempts_file" "$target" "failed: invalid wiki article slug; thesis wikilinks resolve case-sensitively and must be lowercase kebab-case slugs starting with a letter"
      continue
    fi

    kind="$(classify_new_article "$target" "$reference_inventory")"
    if [[ "$kind" == "reference" ]]; then
      echo "[promote] missing wikilink $target classified as reference (reference hint)" >&2
    else
      echo "[promote] missing wikilink $target classified as concept (default; no reference inventory match)" >&2
    fi

    if wiki_inventory_contains "$target" "$planned_inventory"; then
      record_drafting_attempt "$attempts_file" "$target" "succeeded: already resolved before drafting"
      continue
    fi

    article="$OUTDIR/$LABEL-article-$target.md"
    if ! failure_reason="$(draft_new_article "$target" "$kind" "$article_inventory" "$source_inventory" "$backend" "$article_draft_workdir" "$article")"; then
      record_drafting_attempt "$attempts_file" "$target" "failed: $failure_reason"
      continue
    fi

    if wiki_article_file_exists "$target"; then
      record_drafting_attempt "$attempts_file" "$target" "succeeded: resolved before write"
      printf '%s\n' "$target" >> "$planned_inventory"
      sort -u "$planned_inventory" -o "$planned_inventory"
      continue
    fi

    dest="$(article_dest_for_kind "$kind" "$target")"
    if [[ -e "$dest" ]]; then
      record_drafting_attempt "$attempts_file" "$target" "failed: destination appeared before write: $dest"
      continue
    fi
    rel="$(relative_path_under "$dest" "$repo_root")"
    printf '%s\t%s\t%s\t%s\t%s\n' "$target" "$kind" "$article" "$dest" "$rel" >> "$staged_articles_tsv"
    printf '%s\n' "$target" >> "$planned_inventory"
    sort -u "$planned_inventory" -o "$planned_inventory"
    record_drafting_attempt "$attempts_file" "$target" "succeeded: staged for write"
    echo "[promote] staged new $kind article: $dest" >&2
  done < "$missing_file"

  missing_wikilinks "$thesis_source" "$planned_inventory" > "$unresolved_file"
  if [[ -s "$unresolved_file" ]]; then
    report_unresolved_wikilinks "$unresolved_file" "$attempts_file" "$cap_hit"
    return 1
  fi

  echo "[promote] citation coverage staged: all thesis wikilinks have planned files" >&2
}

install_staged_articles() {
  local staged_articles_tsv="$1" new_articles_jsonl="$2" installed_articles_file="$3"
  local target kind article dest rel install_status live_inventory
  live_inventory="$OUTDIR/$LABEL-install-live-wiki-inventory.txt"
  : > "$new_articles_jsonl"
  [[ -f "$installed_articles_file" ]] || : > "$installed_articles_file"
  write_wiki_inventory "$live_inventory"

  while IFS=$'\t' read -r target kind article dest rel; do
    [[ -n "$target" ]] || continue
    if wiki_article_file_exists "$target" "$live_inventory"; then
      echo "[promote] skipped staged $kind article already resolved: $target" >&2
      continue
    fi
    if [[ -e "$dest" ]]; then
      echo "[promote] FATAL: destination appeared before staged write: $dest" >&2
      return 1
    fi
    install_status=0
    install_file_without_overwrite "$article" "$dest" || install_status=$?
    if [[ "$install_status" == "0" ]]; then
      printf '%s\n' "$dest" >> "$installed_articles_file"
      if ! jq -cn --arg slug "$target" --arg kind "$kind" --arg path "$rel" \
        '{slug: $slug, kind: $kind, path: $path}' >> "$new_articles_jsonl"; then
        echo "[promote] FATAL: failed to record new article manifest entry for $target" >&2
        return 1
      fi
      add_inventory_slug "$target" "$live_inventory"
      echo "[promote] wrote new $kind article: $dest" >&2
      continue
    fi
    if [[ "$install_status" == "2" ]]; then
      echo "[promote] FATAL: destination appeared before staged write: $dest" >&2
    else
      echo "[promote] FATAL: failed to write staged $kind article to $dest" >&2
    fi
    return 1
  done < "$staged_articles_tsv"
}

verify_live_citation_coverage() {
  local thesis="$1" attempts_file="$2"
  local wiki_inventory unresolved_file
  wiki_inventory="$OUTDIR/$LABEL-final-wiki-inventory.txt"
  unresolved_file="$OUTDIR/$LABEL-final-unresolved-wikilinks.txt"

  write_wiki_inventory "$wiki_inventory"
  missing_wikilinks "$thesis" "$wiki_inventory" > "$unresolved_file"
  if [[ -s "$unresolved_file" ]]; then
    report_unresolved_wikilinks "$unresolved_file" "$attempts_file" "no"
    return 1
  fi
  echo "[promote] citation coverage complete: all thesis wikilinks resolve" >&2
}

fallback_wiki_repo_root() {
  local parent_dir parent_name
  parent_dir="$(dirname "$LIVE_WIKI")"
  parent_name="$(basename "$parent_dir")"
  # Common local layout is <repo>/topics/<topic>; if that heuristic does not
  # match, keep paths relative to the topic directory itself.
  if [[ "$parent_name" == "topics" && -d "$LIVE_WIKI/../.." ]]; then
    (cd "$LIVE_WIKI/../.." && pwd)
  else
    (cd "$LIVE_WIKI" && pwd)
  fi
}

wiki_repo_root() {
  git -C "$LIVE_WIKI" rev-parse --show-toplevel 2>/dev/null || fallback_wiki_repo_root
}

# stamp_scenario <thesis-file>
# Rewrite a merged thesis so it reads as an illustrative scenario rather than a
# report of a real event: force stance/verdict/confidence in the frontmatter and
# insert a "not a sourced event" admonition right after the first H1. Idempotent.
# This is the on-disk counterpart of the --scenario guard; it is what keeps a
# synthetic prompt from being published as fact.
stamp_scenario() {
  local f="$1" tmp has_banner
  # Idempotent: don't add a second banner if one is already present.
  if grep -q 'Illustrative scenario — not a sourced event' "$f"; then
    has_banner=1
  else
    has_banner=0
  fi
  tmp="$(mktemp)"
  awk -v has_banner="$has_banner" '
    BEGIN { infm=0; fm_done=0; banner_done=has_banner }
    NR==1 && $0=="---" { infm=1; print; next }
    infm==1 && $0=="---" { infm=0; fm_done=1; print; next }
    infm==1 {
      if ($0 ~ /^stance:/)     { print "stance: scenario"; next }
      if ($0 ~ /^verdict:/)    { print "verdict: illustrative-scenario"; next }
      if ($0 ~ /^confidence:/) { print "confidence: low"; next }
      print; next
    }
    fm_done==1 && banner_done==0 && /^# / {
      print
      print ""
      print "> [!warning] Illustrative scenario — not a sourced event"
      print "> The triggering item is a synthetic/illustrative prompt, not a confirmed news event. Specific figures, dates, and any attributed quotations are hypothetical and may not match the real-world record. What this page demonstrates is the framework application — do not cite the event details as fact."
      banner_done=1
      next
    }
    { print }
  ' "$f" > "$tmp" && mv "$tmp" "$f"
  # If the merge omitted stance/verdict/confidence entirely, inject them.
  grep -qE '^stance:'     "$f" || sed -i '0,/^---$/{/^---$/a stance: scenario
}' "$f"
  grep -qE '^verdict:'    "$f" || sed -i '0,/^---$/{/^---$/a verdict: illustrative-scenario
}' "$f"
  grep -qE '^confidence:' "$f" || sed -i '0,/^---$/{/^---$/a confidence: low
}' "$f"
}

# Stage 7: promote — complete citation coverage against the staged merged
# thesis before overwriting the live thesis. This keeps a failed completion
# pass from leaving unresolved links in wiki/theses/.
PROMOTE_TARGET=""
if [[ -n "$TARGET_THESIS" ]]; then
  PROMOTE_TARGET="$LIVE_WIKI/wiki/theses/$TARGET_THESIS"
else
  # New-news case: take the agent's own thesis_slug (more descriptive than LABEL).
  for MF in "$MF_CLAUDE" "$MF_CODEX"; do
    [[ -f "$MF" ]] || continue
    AGENT_SLUG="$(jq -r '.thesis_slug // ""' "$MF" 2>/dev/null)"
    if [[ -n "$AGENT_SLUG" ]]; then
      PROMOTE_TARGET="$LIVE_WIKI/wiki/theses/$AGENT_SLUG.md"
      break
    fi
  done
  # Last-resort fallback: today-dated LABEL
  if [[ -z "$PROMOTE_TARGET" ]]; then
    PROMOTE_TARGET="$LIVE_WIKI/wiki/theses/$(date +%Y-%m-%d)-$LABEL.md"
  fi
fi
echo "[promote] target: $PROMOTE_TARGET" >&2

if [[ "$SKIP_MERGE" == "1" && -n "$DRAFT_CLAUDE" && -z "$DRAFT_CODEX" ]]; then
  RAW_REF_MANIFESTS=("$MF_CLAUDE" "$MF_CODEX")
else
  RAW_REF_MANIFESTS=("$MF_CODEX" "$MF_CLAUDE")
fi
RAW_REF="$(resolve_required_raw_ref "$CLEAN" "${RAW_REF_MANIFESTS[@]}")"

# Final manifest at the live wiki path so downstream consumers
# (e.g., newsroom build-thesis.sh) have a stable contract.
WIKI_REPO_ROOT_FOR_MANIFEST="$(wiki_repo_root)"
NEW_ARTICLES_JSONL="$OUTDIR/$LABEL-new-articles.jsonl"
NEW_ARTICLES_JSON="$OUTDIR/$LABEL-new-articles.json"
STAGED_ARTICLES_TSV="$OUTDIR/$LABEL-staged-articles.tsv"
CITATION_ATTEMPTS_FILE="$OUTDIR/$LABEL-citation-attempts.txt"
INSTALLED_ARTICLES_FILE="$OUTDIR/$LABEL-installed-articles.txt"
verify_raw_news_available "$RAW_REF"
# Keep this as a simple command: wrapping it in if/|| would suppress errexit
# inside the function, while a non-zero return still exits the script here.
complete_citation_coverage "$CLEAN" "$WIKI_REPO_ROOT_FOR_MANIFEST" "$NEW_ARTICLES_JSONL" "$STAGED_ARTICLES_TSV" "$CITATION_ATTEMPTS_FILE"

# Write the thesis verbatim, then install the staged articles under a rollback
# guard. RAW_REF was resolved above for raw promotion and the final manifest;
# the citation-completion pass does not edit thesis text.
# (Earlier versions of this script tried to date-substitute the thesis's
# raw/news references to match a previous live raw/news date prefix; that
# broke when the agent generated a fresh slug instead of reusing the old
# one. Better to just let each re-run produce its own dated raw/news and
# leave the thesis pointing at the agent's new file.)
install_promote_exit_trap
begin_promote_rollback "$PROMOTE_TARGET" "$INSTALLED_ARTICLES_FILE"
promote_raw_news_if_needed "$RAW_REF" "$INSTALLED_ARTICLES_FILE"
if [[ "$SCENARIO" == 1 ]]; then
  echo "[promote] stamping thesis as an illustrative scenario (unsourced input)" >&2
  stamp_scenario "$CLEAN"
fi
cp "$CLEAN" "$PROMOTE_TARGET"
install_staged_articles "$STAGED_ARTICLES_TSV" "$NEW_ARTICLES_JSONL" "$INSTALLED_ARTICLES_FILE"
verify_live_citation_coverage "$PROMOTE_TARGET" "$CITATION_ATTEMPTS_FILE"
finish_promote_rollback
echo "[promote] wrote $PROMOTE_TARGET" >&2

if [[ -s "$NEW_ARTICLES_JSONL" ]]; then
  jq -s '.' "$NEW_ARTICLES_JSONL" > "$NEW_ARTICLES_JSON"
else
  printf '[]\n' > "$NEW_ARTICLES_JSON"
fi

THESIS_REL="$(relative_path_under "$PROMOTE_TARGET" "$WIKI_REPO_ROOT_FOR_MANIFEST")"
RAW_REL=""
if [[ -n "${RAW_REF:-}" ]]; then
  LIVE_RAW_ABS="$LIVE_WIKI/$RAW_REF"
  RAW_REL="$(relative_path_under "$LIVE_RAW_ABS" "$WIKI_REPO_ROOT_FOR_MANIFEST")"
fi
THESIS_SLUG_FINAL="$(basename "$PROMOTE_TARGET" .md)"
RAW_SLUG_FINAL=""
[[ -n "$RAW_REL" ]] && RAW_SLUG_FINAL="$(basename "$RAW_REL" .md)"
if ! CITES_FINAL="$(count_wikilink_targets "$PROMOTE_TARGET")"; then
  echo "[promote] FATAL: cannot count citations because the promoted thesis contains malformed wikilink markup: $PROMOTE_TARGET" >&2
  exit 1
fi
STANCE_FINAL=""
for d in "$DIR_CODEX" "$DIR_CLAUDE"; do
  if [[ -f "$d/.news-lens/$LABEL.json" ]]; then
    STANCE_FINAL=$(jq -r '.stance // ""' "$d/.news-lens/$LABEL.json" 2>/dev/null || echo "")
    [[ -n "$STANCE_FINAL" ]] && break
  fi
done
[[ -z "$STANCE_FINAL" ]] && STANCE_FINAL="unknown"
MERGED_FINAL="true"
[[ "$SKIP_MERGE" == 1 ]] && MERGED_FINAL="false"
LIVE_MANIFEST_DIR="$LIVE_WIKI/.news-lens"
mkdir -p "$LIVE_MANIFEST_DIR"
LIVE_MANIFEST_PATH="$LIVE_MANIFEST_DIR/$LABEL.json"
jq -n \
  --arg slug "$THESIS_SLUG_FINAL" \
  --arg thesis_slug "$THESIS_SLUG_FINAL" \
  --arg raw_slug "$RAW_SLUG_FINAL" \
  --arg thesis_path "$THESIS_REL" \
  --arg raw_path "$RAW_REL" \
  --arg stance "$STANCE_FINAL" \
  --argjson citations "$CITES_FINAL" \
  --argjson merged "$MERGED_FINAL" \
  --argjson skip_merge "$SKIP_MERGE" \
  --slurpfile new_articles "$NEW_ARTICLES_JSON" \
  '{slug: $slug, thesis_slug: $thesis_slug, raw_slug: $raw_slug,
    thesis_path: $thesis_path, raw_path: $raw_path,
    stance: $stance, citations: $citations, merged: $merged,
    skip_merge: $skip_merge, new_articles: $new_articles[0]}' \
  > "$LIVE_MANIFEST_PATH"
echo "[promote] wrote final manifest $LIVE_MANIFEST_PATH" >&2

# Publish chain
if [[ "$PUBLISH" == 1 ]]; then
  echo "[publish] publish.sh + git push (source + public)…" >&2
  /home/user/wiki/scripts/publish.sh libertarian "$PUBLIC_CONTENT" > /dev/null

  WIKI_REPO_ROOT="$(wiki_repo_root)"
  pushd "$WIKI_REPO_ROOT" > /dev/null
  git add -A
  git commit -m "Update $LABEL thesis via A/B+merge

Generated through the news-lens-ab-merge skill: parallel claude + codex
drafts, claude-merged (codex fallback) into one coherent thesis following
the current editorial lens." > /dev/null 2>&1 || echo "[warn] source commit empty/failed" >&2
  git push > /dev/null 2>&1 && echo "[publish] source pushed" >&2 || echo "[warn] source push failed" >&2
  popd > /dev/null

  PUBLIC_REPO_ROOT="$(git -C "$PUBLIC_CONTENT" rev-parse --show-toplevel 2>/dev/null || dirname "$PUBLIC_CONTENT")"
  pushd "$PUBLIC_REPO_ROOT" > /dev/null
  git add -A
  git commit -m "sync libertarian: $LABEL thesis (A/B+merge)" > /dev/null 2>&1 || echo "[warn] public commit empty/failed" >&2
  git push > /dev/null 2>&1 && echo "[publish] public pushed" >&2 || echo "[warn] public push failed" >&2
  popd > /dev/null
else
  echo "[publish] skipped (--no-publish)" >&2
fi

# Summary — be tolerant of missing manifest files under `set -euo pipefail`.
MFS=()
[[ -f "$MF_CLAUDE" ]] && MFS+=("$MF_CLAUDE")
[[ -f "$MF_CODEX"  ]] && MFS+=("$MF_CODEX")
STANCE=""
if (( ${#MFS[@]} > 0 )); then
  STANCE=$(grep -h '"stance"' "${MFS[@]}" 2>/dev/null \
    | head -1 | grep -oE '"[a-z]+"' | tail -1 | tr -d '"' || true)
fi
if ! CITES="$(count_wikilink_targets "$PROMOTE_TARGET")"; then
  echo "[warn] could not count unique cites after publish because the promoted thesis contains malformed wikilink markup" >&2
  CITES="?"
fi

cat <<EOF
========================================
news-lens-ab-merge: done
  label:        $LABEL
  thesis:       $PROMOTE_TARGET
  stance:       ${STANCE:-?}
  unique cites: $CITES
  wiki-mentions in body: $WIKI_MENTIONS (target: 0)
  skip merge:   $SKIP_MERGE
  publish:      $PUBLISH
  scratch:      $OUTDIR
  trials:       $DIR_CLAUDE
                $DIR_CODEX
========================================
EOF
