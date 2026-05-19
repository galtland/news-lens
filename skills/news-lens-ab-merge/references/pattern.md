# Pattern Reference: Why This Skill Exists

## Origin

This skill formalizes a workflow that emerged organically over a long news-lens iteration session. Across five wealth-tax/argentina/fed/digital-euro/nato/wealth-tax trials, two facts became clear:

1. **Single-backend output had predictable defaults that didn't match user preference.** Claude tended toward bookish openings (news summary → cite → rebut). Codex tended toward concise causal-chain openings. Neither alone gave the user what they wanted: causal chain + rebuttal-via-tradition + plain English.

2. **Having both drafts in context for a merge step produced better output than either alone.** The merge agent (codex with both drafts visible) could pick the sharpest causal claim from one and the rebuttal move from the other, ending up tighter than either source.

## The five trials

| Item | Claude draft | Codex draft | Merge result |
|---|---|---|---|
| wealth-tax | competent, layered | concise, sharp causal chain | merge added the "capital is not a static pile waiting to be morally reassigned" line — the sharpest in the series |
| argentina | rent-ceiling mechanism + Mises/Rothbard | same frame, tighter | merge cited 3 concepts in one sentence vs 2 separately |
| fed | ABCT-heavy, dense | sequence-focused (rate cut → mortgage → home sales) | merge linked credit-deferred-payment + ABCT + Rothbard induced-boom in one chain |
| digital-euro | Hillebrand + Rothbard typology | Hillebrand + concrete €3000/12-month | merge added "intervention written into the monetary medium" — the structural point claude had buried |
| nato | Tilly racketeer test, well-quoted | Treaty Article 3 reference, audit-as-extraction | merge added "conversion of domestic productive capacity into an alliance compliance duty" |

Across all five, the merge output had zero "the wiki" self-referential phrases (after the voice rule was added to the lens), clean dual-link wikilinks, and an argumentative causal-chain opening.

## The lens rules that the workflow depends on

The merge step assumes the editorial lens (at `/home/user/wiki/topics/libertarian/lens-austrian-libertarian.md`) carries these rules:

- **Thesis opening: argumentative, not bookish** — causal chain from news → wiki concepts → broader consequence
- **No "the wiki" in body** — voice rule, attributed directly to authors/concepts
- **Wikilink hygiene** — no italics inside `[[slug|...]]` display text
- **Title rule** — no stance noun in title; use `: Analysis` suffix if needed
- **Citation discipline** — cite only load-bearing wiki articles

If the lens is older and missing any of these rules, the merge may produce a thesis that needs manual cleanup. If the user reports stale-feeling output, check the lens first.

## Codex plugin version dependency

The codex backend invokes `@wiki` for deep queries during the draft step. The codex llm-wiki plugin must be at version 0.9.0 or later for the query semantics to work correctly. Earlier versions (0.3.x) used a different directory layout and produced lower-quality discovery.

If the user reports codex returning no relevant articles even when the wiki clearly has coverage, check `~/.codex/plugins/cache/llm-wiki/` for the cached plugin version. Refresh by:

```sh
codex plugin marketplace remove llm-wiki
codex plugin marketplace add /home/user/llm-wiki
rsync -a /home/user/llm-wiki/plugins/llm-wiki/ \
        ~/.codex/plugins/cache/llm-wiki/wiki/0.9.0/
```

## When NOT to use this skill

- The news item is not destined for the libertarian wiki. The skill is hard-coded to that wiki's lens, paths, and editorial conventions.
- The user wants to A/B-test a single backend against itself or compare two prompt variants. That's a different workflow.
- The user wants to update the lens itself (not generate a thesis). Different task.
- The wiki has no existing scaffolding (concepts, references, raw articles). The skill assumes a populated wiki with at least the Austrian-libertarian corpus already ingested.

## When to use this skill even if user doesn't ask explicitly

If the user pastes a news item and says anything that implies they want commentary in the libertarian wiki, use this skill. It produces consistently better output than running news-lens once.
