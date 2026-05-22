# Downstream-block diagnostics via prefix-masked re-parse

## Overview

When an early block has an unrecoverable parse error, the tree-sitter parser
drops into error state 0 and skips every remaining token to EOF, so blocks
after the first failing one produce no diagnostics at all (see `issue.md`,
Example 1). A scanner-level recovery spike (see
`~/.claude/plans/can-you-attempt-to-zippy-pebble.md`) showed tree-sitter *can*
be forced to resync, but only by bypassing the Merr corpus, which regresses
the specific Q-codes for every unclosed-X case. That cost is not cheaply
recoverable on `main`.

This plan fixes the problem at the `pampa` level instead, leaving the grammar
and scanner untouched. Scope is **diagnostics only** — when a document already
has parse errors (so `read()` returns `Err` and produces no AST), we surface a
diagnostic for *each* failing block instead of only the first. AST recovery is
explicitly out of scope.

## Key idea: prefix masking (no offset rebasing)

Re-parsing a byte *slice* would force us to rebase every diagnostic's
`offset`/`row` back to absolute coordinates. Instead we re-parse the **whole
input with the already-handled prefix masked**: every byte in
`[0, resync_offset)` that is not `\n`/`\r` is replaced with a space, and
newlines are kept. This preserves total length, byte offsets, and row numbers
exactly, so:

- the masked prefix becomes whitespace-only (blank) lines the parser ignores,
- the first still-live block is now the first block in the document, which is
  the known-good "clean start, then bad block" shape (baseline Example 2 gives
  the correct `Q-2-12`),
- diagnostics come back already at absolute positions with their proper
  Q-codes — no rebasing, full reuse of `produce_diagnostic_messages`.

## Algorithm (in `crates/pampa/src/readers/qmd.rs`, error branch ~L122-145)

After the existing first-parse diagnostics are produced:

1. Seed `resync_offset` = start of the next block after the first error: scan
   `input_bytes` forward from the first error's end for the next blank line
   (whitespace-only line), and take the start of the line following it. If none,
   stop (error was in the last block).
2. Loop (bounded by a max iteration count = line count, as a safety guard):
   a. Build `masked` = copy of `input_bytes` with `[0, resync_offset)`
      space-masked (keep `\n`/`\r`).
   b. Parse `masked` with a fresh `TreeSitterLogObserverFast`; if no errors,
      break.
   c. Re-parse with a full `TreeSitterLogObserver` and run
      `produce_diagnostic_messages(masked, ...)`. (Optionally prune as the main
      path does.)
   d. Take the earliest diagnostic at/after `resync_offset` — that is the next
      block's error. Append it to the collected list.
   e. Advance `resync_offset` to the next blank-line boundary after that
      diagnostic's block. If it does not strictly advance, break.
3. Merge collected downstream diagnostics into the main `diagnostics` vec,
   dedup by (start offset, code), sort by start offset, and `return Err`.

One diagnostic per block (matching the parser's own "first error then skip"
behaviour within a block); multiple errors inside a single block remain a
separate, harder problem and are out of scope.

## Known limitation (document, do not paper over)

Resync is heuristic: a blank line is treated as a safe block boundary. For
constructs that legitimately contain blank lines (fenced code blocks, some list
/ blockquote continuations) the resync point could land inside the construct.
The unclosed-*inline* family — the motivating case — only occurs in
paragraph-like content where blank-line resync is correct, so this is acceptable
for the target bug. If a future case needs precise block boundaries, that is a
follow-up (proper block segmentation), not a patch here.

## Work items

- [x] Phase 1 (TDD): failing integration test in
      `crates/pampa/tests/downstream_block_diagnostics.rs` asserting Example 1
      yields a `Q-2-5` for paragraph 2 (offset >= 33) plus paragraph 1's.
      Confirmed it failed (only one diagnostic before the fix).
- [x] Phase 2: implemented `mask_prefix`, `next_block_start_after`, and
      `collect_downstream_diagnostics` in `qmd.rs`; wired into the error branch
      after pruning.
- [x] Phase 3: test passes; added `three_failing_paragraphs_each_report_their_own_code`
      (codes [Q-2-12, Q-2-5, Q-2-12] at increasing offsets) and
      `clean_first_block_then_bad_block_is_unchanged`. Full `pampa` suite green
      (3776 passed, 2 skipped).
- [x] Phase 4: end-to-end through the binary confirmed — Example 1 reports both
      paragraphs; three-paragraph case reports rows 1/3/5 with correct codes;
      control still `Q-2-12`; well-formed still parses to a clean AST.

## Outcome

Implemented. Each failing block now surfaces its own specific Q-code instead of
only the first. No regressions in the `pampa` crate.

Real-world validation: ran all 573 `q2r/tests/fixtures/quarto-web/*.qmd`
documents through the built binary — 0 panics, 0 timeouts (>30s), 0 slow files
(>3s); 513 clean parses (unaffected, never enter the error branch), 60 graceful
errors all with distinct per-block diagnostic locations and 0 duplicate-location
diagnostics. Example: `docs/projects/virtual-environments.qmd` now reports all 8
grid-table errors (`Q-2-39`) at distinct lines in one pass instead of only the
first.

## Block-detection robustness

`next_block_start_after` resyncs at the next blank line. As a standalone rule
that is wrong for constructs that legitimately contain blank lines (fenced code,
display math, raw HTML, loose lists). It holds up in practice for one structural
reason: **masking only blanks the handled prefix, it never truncates the
document.** So as long as resync lands at or before a construct's opening line,
the whole construct is re-parsed intact. Two facts make that the normal case:

1. Top-level blocks are almost always blank-line separated, so resync lands on a
   construct's *start*, not its interior.
2. Even when resync lands strictly inside a construct (only possible with no
   blank-line separation between the error block and the construct), the
   orphaned tail is typically benign — an unclosed fence / math / HTML
   auto-closes at EOF rather than erroring.

Failure modes, ranked:

- **Under-reporting** (miss a downstream error because its content got partly
  blanked): possible, and the *safe* direction — still strictly better than the
  baseline, which reports zero downstream errors.
- **Over-reporting** (a spurious diagnostic from an orphaned construct tail):
  the concerning direction. Could not be triggered with fenced code, display
  math, raw HTML, or loose lists (adversarial cases A–G, all pinned as tests in
  `crates/pampa/tests/downstream_block_diagnostics.rs`), and the 573-doc corpus
  produced 0 duplicate-location diagnostics.

This is deliberately *not* a correct block tokenizer. It is sufficient for the
target families (unclosed-inline and unsupported-construct errors, which occur
in paragraph-like content) and degrades toward under-reporting elsewhere. If a
future case needs precise boundaries (e.g. an error genuinely buried inside a
fence with no blank-line separation), the right fix is real block segmentation —
a follow-up, not an extension of this patch.

## Pending

Workspace-wide verification (`cargo nextest run --workspace` / `cargo xtask
verify`) — running. The change is additive and localized to the qmd error path,
but `pampa` has downstream consumers, so confirm before considering this done.

## Verification

```bash
strip_ansi() { sed -E 's/\x1b\[[0-9;]*[a-zA-Z]//g'; }
# Target: BOTH paragraphs' errors reported.
printf  "%s\n" "This is the _first *paragraph_." "" "This is the _2nd paragraph" \
  | cargo run --quiet --bin pampa -- --no-prune-errors -t native 2>&1 | strip_ansi
# Regression guards:
printf  "%s\n" "First good paragraph." "" "Second *bad paragraph." \
  | cargo run --quiet --bin pampa -- --no-prune-errors -t native 2>&1 | strip_ansi   # Q-2-12
printf  "%s\n" 'A [link](./t.html "T") and *emph* and "quote".' \
  | cargo run --quiet --bin pampa -- -t native 2>&1 | strip_ansi                      # clean AST
# Focused tests:
cargo nextest run -p pampa
```
