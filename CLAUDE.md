# foldwave: agent norms

Read the org charter first: `D:\projects\nativelite\CLAUDE.md` (or
https://github.com/nativelite/ops/blob/main/CLAUDE.md). It governs everything
here. This file only adds what is specific to foldwave.

## What this repo is

The design, and later the code, for foldwave, the GPU-rendered agentic terminal.
The design is `docs/design.md`:
- Founder decisions in §11 are settled. Don't re-open them without a reason
  backed by data.
- Anything not in §11 is a proposal.

## Rules specific to foldwave

- **Targets are in design §4.** Every performance claim needs a reproducible
  benchmark (`standards/performance.md`), measured on the reference machine and
  reported with median, p90/p99 and sample count.
- **Reuse means inspiration, not copied code** (`standards/zero-dependency.md`,
  "Reusing open-source work").
  - Never copy GPL/AGPL code.
  - If substantial permissive code is ever ported, keep its notice in
    `THIRD-PARTY-NOTICES.md` and justify it in a decision record.
- **Facts about other projects go in `docs/prior-art.md` or `docs/compute.md`,
  with a source URL each.** Mark anything unverified.
- **Stay inside the fence:** design changes go through `docs/design.md`, and a
  decision the founder hasn't made is listed as open, not assumed.

## Worktree discipline (when built with atrium fleets)

- You are already in your assigned worktree. **Do not `cd`.**
- Commit on your current branch; do not switch or create branches.

## Gate before every push

- **Docs only (now):** check that links resolve and that tables render.
- **Once code exists:** `python dev.py check` (dependency guard, `cargo fmt
  --check`, tests) must be green before committing, as in every nativelite repo.
