#!/bin/bash
# news-lens-ab-merge: orchestrate the A/B+merge thesis workflow
#
# Usage:
#   ab_merge.sh --label <slug> --text <news-text> [--no-publish]
#   ab_merge.sh --label <slug> --text-file <path> [--no-publish]
#
# Environment overrides:
#   NL_LIVE_WIKI         (default /home/user/wiki/topics/libertarian)
#   NL_PUBLIC_CONTENT    (default /home/user/projects/douglaz.github.io/content)
#   NL_BIN               (default /home/user/news-lens/target/release/news-lens)
#   NL_PROMPT_TEMPLATE   (default /home/user/news-lens/prompts/process-post.md)
#   NL_SCRATCH           (default /tmp)
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MERGE_INSTRUCTIONS="$SKILL_DIR/references/merge-instructions.md"

# Defaults
LIVE_WIKI="${NL_LIVE_WIKI:-/home/user/wiki/topics/libertarian}"
PUBLIC_CONTENT="${NL_PUBLIC_CONTENT:-/home/user/projects/douglaz.github.io/content}"
NL_BIN="${NL_BIN:-/home/user/news-lens/target/release/news-lens}"
PROMPT_TEMPLATE="${NL_PROMPT_TEMPLATE:-/home/user/news-lens/prompts/process-post.md}"
SCRATCH="${NL_SCRATCH:-/tmp}"
PUBLISH=1
LABEL=""
NEWS_TEXT=""

# Parse args
while [[ $# -gt 0 ]]; do
  case "$1" in
    --label)       LABEL="$2"; shift 2 ;;
    --text)        NEWS_TEXT="$2"; shift 2 ;;
    --text-file)   NEWS_TEXT="$(cat "$2")"; shift 2 ;;
    --no-publish)  PUBLISH=0; shift ;;
    -h|--help)
      sed -n '2,/^set/p' "$0" | sed '$d' | sed 's/^# \?//'
      exit 0 ;;
    *) echo "Unknown arg: $1" >&2; exit 2 ;;
  esac
done

[[ -z "$LABEL" ]] && { echo "Missing --label" >&2; exit 2; }
[[ -z "$NEWS_TEXT" ]] && { echo "Missing --text or --text-file" >&2; exit 2; }

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
LIVE_RAW_DATE=""
if [[ -n "$LIVE_RAW_NEWS" ]]; then
  LIVE_RAW_DATE="$(echo "$LIVE_RAW_NEWS" | grep -oE '^[0-9]{4}-[0-9]{2}-[0-9]{2}')"
fi
echo "[stage] live raw/news: ${LIVE_RAW_NEWS:-<none>}" >&2

# Stage 2: stage two trial wikis (claude + codex) with thesis/focused/raw deleted
stage_trial() {
  local backend="$1"
  local dir="$SCRATCH/lib-m-$LABEL-$backend"
  rm -rf "$dir"
  cp -r "$LIVE_WIKI" "$dir"
  rm -rf "$dir/.news-lens"
  if [[ -n "$TARGET_THESIS" ]]; then
    rm -f "$dir/wiki/theses/$TARGET_THESIS"
    while IFS= read -r slug; do
      [[ -n "$slug" ]] || continue
      rm -f "$dir/wiki/concepts/$slug.md"
    done < <(detect_focused "$LIVE_WIKI/wiki/theses/$TARGET_THESIS")
  fi
  if [[ -n "$LIVE_RAW_NEWS" ]]; then
    rm -f "$dir/raw/news/$LIVE_RAW_NEWS"
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
    args='["--print", "--permission-mode", "acceptEdits", "--allowedTools", "Bash,Edit,Write,Read,Glob,Grep,Skill"]'
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
public_base_url = "https://douglaz.github.io"

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

# Find the new thesis files (date-prefixed, agent's slug)
find_thesis() {
  local dir="$1"
  ls -t "$dir/wiki/theses/"*"$LABEL"*.md 2>/dev/null | head -1
}

DRAFT_CLAUDE="$(find_thesis "$DIR_CLAUDE")"
DRAFT_CODEX="$(find_thesis "$DIR_CODEX")"

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

  echo "[merge] running codex exec with merge prompt ($(wc -l < "$PROMPT") lines)…" >&2
  codex exec --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check -C "$SCRATCH" \
    < "$PROMPT" > "$OUTDIR/$LABEL-merge.out" 2>&1

  # Stage 6: extract clean merge
  awk '/^tokens used$/{p=1; next} p' "$OUTDIR/$LABEL-merge.out" \
    | sed '1{/^[0-9,]*$/d}' > "$OUTDIR/$LABEL-merge-clean.md"
fi

CLEAN="$OUTDIR/$LABEL-merge-clean.md"
[[ -s "$CLEAN" ]] || { echo "[fatal] merge clean output empty" >&2; exit 1; }

# Voice rule sanity check
WIKI_MENTIONS="$(grep -c "the wiki\|wiki's" "$CLEAN" || echo 0)"
if [[ "$WIKI_MENTIONS" -gt 0 ]]; then
  echo "[warn] merged thesis has $WIKI_MENTIONS 'the wiki' mentions — lens voice rule violated" >&2
fi

# Stage 7: promote — adjust dates to match live raw/news
PROMOTE_TARGET=""
if [[ -n "$TARGET_THESIS" ]]; then
  PROMOTE_TARGET="$LIVE_WIKI/wiki/theses/$TARGET_THESIS"
else
  PROMOTE_TARGET="$LIVE_WIKI/wiki/theses/$(date +%Y-%m-%d)-$LABEL.md"
fi
echo "[promote] target: $PROMOTE_TARGET" >&2

if [[ -n "$LIVE_RAW_NEWS" && -n "$LIVE_RAW_DATE" ]]; then
  TODAY="$(date +%Y-%m-%d)"
  sed -e "s|raw/news/$TODAY-|raw/news/$LIVE_RAW_DATE-|g" "$CLEAN" > "$PROMOTE_TARGET"
else
  cp "$CLEAN" "$PROMOTE_TARGET"
  RAW_REF="$(grep -oE 'raw/news/[0-9]{4}-[0-9]{2}-[0-9]{2}-[a-z0-9-]+\.md' "$CLEAN" | head -1)"
  if [[ -n "$RAW_REF" ]]; then
    SRC=""
    for d in "$DIR_CLAUDE" "$DIR_CODEX"; do
      [[ -f "$d/$RAW_REF" ]] && SRC="$d/$RAW_REF" && break
    done
    [[ -n "$SRC" ]] && cp "$SRC" "$LIVE_WIKI/$RAW_REF"
  fi
fi

echo "[promote] wrote $PROMOTE_TARGET" >&2

# Publish chain
if [[ "$PUBLISH" == 1 ]]; then
  echo "[publish] publish.sh + git push (source + public)…" >&2
  /home/user/wiki/scripts/publish.sh libertarian "$PUBLIC_CONTENT" > /dev/null

  WIKI_REPO_ROOT="$(git -C "$LIVE_WIKI" rev-parse --show-toplevel 2>/dev/null || echo /home/user/wiki)"
  pushd "$WIKI_REPO_ROOT" > /dev/null
  git add -A
  git commit -m "Update $LABEL thesis via A/B+merge

Generated through the news-lens-ab-merge skill: parallel claude + codex
drafts, codex-merged into one coherent thesis following the current
editorial lens." > /dev/null 2>&1 || echo "[warn] source commit empty/failed" >&2
  git push > /dev/null 2>&1 && echo "[publish] source pushed" >&2 || echo "[warn] source push failed" >&2
  popd > /dev/null

  PUBLIC_REPO_ROOT="$(git -C "$PUBLIC_CONTENT" rev-parse --show-toplevel 2>/dev/null || echo "$(dirname "$PUBLIC_CONTENT")")"
  pushd "$PUBLIC_REPO_ROOT" > /dev/null
  git add -A
  git commit -m "sync libertarian: $LABEL thesis (A/B+merge)" > /dev/null 2>&1 || echo "[warn] public commit empty/failed" >&2
  git push > /dev/null 2>&1 && echo "[publish] public pushed" >&2 || echo "[warn] public push failed" >&2
  popd > /dev/null
else
  echo "[publish] skipped (--no-publish)" >&2
fi

# Summary
STANCE="$(grep -h '"stance"' "$MF_CLAUDE" "$MF_CODEX" 2>/dev/null | head -1 | grep -oE '"[a-z]+"' | tail -1 | tr -d '"')"
CITES="$(grep -oE '\[\[[a-z0-9-]+' "$PROMOTE_TARGET" | sort -u | wc -l)"

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
