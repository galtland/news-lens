# AGENTS.md

Repository-level guidance for AI coding agents working on news-lens.

## This is a greenfield project — no compatibility layers

news-lens is a hard fork of news-tagger that has never shipped a release. There are no deployed instances, no users with persisted state, and no external consumers of any schema, config, or API surface in this repo.

When working on this codebase, do not introduce backwards-compatibility machinery of any kind. Specifically:

- **Schema changes**: rename struct fields freely. Do not add `#[serde(alias = "...")]` to preserve old names. Do not write SQLite migrations to rename columns — edit the `CREATE TABLE` statement.
- **Config changes**: rename TOML keys freely. Do not keep deprecated keys mapped to new ones. Update `fixtures/config/*.toml` to match.
- **CLI changes**: rename subcommands and flags freely. Do not add aliases or hidden deprecation shims.
- **API changes**: change function signatures, port traits, and method names freely. Do not add wrapper functions that call into the new API from the old one.
- **JSON contracts**: the agent-return JSON, fixture JSON, and on-disk JSON formats can all change shape without versioning.

The relevant spec discipline is in `SPEC-libertarian.md` §0 #8 (KISS): pick the smallest viable answer. Compat layers are the opposite of that.

Reviewers will sometimes flag missing aliases or migrations as correctness issues — they are pattern-matching from production codebases. In this repo, those findings can be declined with a pointer back to this file.
