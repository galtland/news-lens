This file is the verbatim suffix appended to every merge prompt, after the lens + news + Draft A + Draft B.

---

# Your task

Produce a single merged thesis markdown file. Requirements:

1. Follow the lens above — especially the **Thesis opening: argumentative, not bookish** rule (causal chain from news → wiki concepts → broader consequence), the **no "the wiki" in body** Voice rule, and the **wikilink hygiene** (no italics inside wikilink display text) rule.
2. Pick the best argumentative moves from each draft. Don't concatenate; synthesize.
3. The opening should be the strongest first paragraph the merge can produce — combine causal chain (claim → consequence → broader stake) with rebuttal-via-tradition where applicable.
4. Eliminate redundancy across sections. Keep each section earning its place.
5. Preserve the dual-link citation format `[[slug|Title]] ([Title](relative-path.md))` for every wiki article reference.
6. Quote integrity (the lens Integrity rules apply, and merging is where they most often break): every blockquote must be a verbatim quotation from a cited **raw** source and must carry its own attribution line that **dual-links the author and the work** so the published page links the book — `> — [[author-slug|Author]] ([Author](../references/author-slug.md)), [[work-slug|Work]] ([Work](../references/work-slug.md))` (add a chapter/section when known; fall back to plain `*italics*` for books / "curly quotes" for essays only when no reference page exists). When combining the drafts, do NOT drop or down-grade a source attribution to plain text, stitch two non-contiguous passages into one quotation without an ellipsis, promote your own connective analysis into a blockquote, or quote a wiki concept/reference article. If either draft did any of these, demote that block to prose. Keep nothing in quotation marks in the `summary`/`short` unless it is verbatim.
7. Output ONLY the merged thesis — starting with `---` (the YAML frontmatter) and ending after the last `## Sources` entry. No preamble, no "here is my merge:", no trailing commentary.
