# news-lens

Wiki-grounded news commentary from a configured editorial lens.

`news-lens` reads posts from X or JSONL fixtures, sends each post to a single
subprocess harness, validates the agent's final JSON, records platform state in
SQLite, and can stage commentary through the existing outbox approval flow.

## Quick Start

```bash
news-lens config init
news-lens doctor
news-lens process --post --text "Test news item" --dry-run --config fixtures/config/stub-harness.toml
news-lens run --dry-run --once
```

## Commands

```bash
news-lens process --post --text "Post content"
news-lens process --jsonl fixtures/posts/source_posts.jsonl
news-lens run --dry-run --once
news-lens run --require-approval --once
news-lens wiki status
news-lens lens list
news-lens lens show austrian-libertarian
news-lens doctor
news-lens config init
```

## Configuration

Configuration is loaded from `--config`, then `./config.toml`, then
environment variables with the `NEWS_LENS__` prefix.

See `news-lens config init` for the full skeleton. The v1 blocks are:

- `[general]`
- `[wiki]`
- `[lens]`
- `[harness]`
- `[watch]`
- `[x.read]`
- `[x.write]`
- `[nostr]`

## Development

```bash
cargo build --workspace
cargo test --workspace
```
