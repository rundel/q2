/*
 * downstream_block_diagnostics.rs
 * Copyright (c) 2026 Posit, PBC
 *
 * Regression tests for issue.md: a parse error in an early block must not
 * suppress diagnostics for later blocks. Tree-sitter, on an unrecoverable
 * inline error, skips to EOF and emits an empty document, so historically
 * only the first failing block produced a diagnostic. The pampa-level
 * prefix-masked re-parse should surface one diagnostic per failing block.
 */

use pampa::readers;

/// Parse `input` and return the diagnostics (the inputs here always error).
fn diagnostics_for(input: &str) -> Vec<quarto_error_reporting::DiagnosticMessage> {
    let input_bytes = input.as_bytes();
    let mut output = Vec::new();
    readers::qmd::read(input_bytes, false, "test.qmd", &mut output, false, None)
        .expect_err("input is expected to produce parse errors")
}

fn render_all(input: &str, diags: &[quarto_error_reporting::DiagnosticMessage]) -> String {
    let mut source_context = quarto_source_map::SourceContext::new();
    source_context.add_file("test.qmd".to_string(), Some(input.to_string()));
    diags
        .iter()
        .map(|d| d.to_text(Some(&source_context)))
        .collect::<Vec<_>>()
        .join("\n---\n")
}

#[test]
fn second_paragraph_unclosed_underscore_is_reported() {
    // Paragraph 1 (rows 0): unclosed `_` and `*`.
    // Paragraph 2 (row 2, byte offset 33): unclosed `_`.
    // Byte layout: line 0 is 31 chars + '\n' (offsets 0..=31), line 1 is the
    // blank line ('\n' at 32), paragraph 2 begins at offset 33.
    let input = "This is the _first *paragraph_.\n\nThis is the _2nd paragraph\n";
    let diags = diagnostics_for(input);

    let para2_start = 33;
    let para2_diags: Vec<_> = diags
        .iter()
        .filter(|d| {
            d.location
                .as_ref()
                .map(|loc| loc.start_offset() >= para2_start)
                .unwrap_or(false)
        })
        .collect();

    assert!(
        !para2_diags.is_empty(),
        "expected at least one diagnostic anchored in paragraph 2 (offset >= {para2_start}); \
         got diagnostics:\n{}",
        render_all(input, &diags)
    );

    // The paragraph-2 error is an unclosed underscore emphasis (Q-2-5).
    assert!(
        para2_diags
            .iter()
            .any(|d| d.code.as_deref() == Some("Q-2-5")),
        "expected a Q-2-5 (Unclosed Underscore Emphasis) diagnostic for paragraph 2; \
         got diagnostics:\n{}",
        render_all(input, &diags)
    );

    // Paragraph 1's diagnostic must still be present too (offset < para2_start).
    assert!(
        diags.iter().any(|d| d
            .location
            .as_ref()
            .map(|loc| loc.start_offset() < para2_start)
            .unwrap_or(false)),
        "expected paragraph 1's diagnostic to remain; got diagnostics:\n{}",
        render_all(input, &diags)
    );
}

#[test]
fn three_failing_paragraphs_each_report_their_own_code() {
    // Three paragraphs, each with a distinct unclosed delimiter. Every block
    // must surface its own specific Q-code, in source order.
    let input = "Para one *bad.\n\nPara two _bad.\n\nPara three *bad.\n";
    let diags = diagnostics_for(input);

    let codes: Vec<&str> = diags.iter().filter_map(|d| d.code.as_deref()).collect();
    assert_eq!(
        codes,
        vec!["Q-2-12", "Q-2-5", "Q-2-12"],
        "expected one specific Q-code per failing block in source order; got:\n{}",
        render_all(input, &diags)
    );

    // The three diagnostics must be anchored on three distinct, increasing
    // source offsets (one per paragraph), not collapsed onto the first block.
    let offsets: Vec<usize> = diags
        .iter()
        .filter_map(|d| d.location.as_ref().map(|l| l.start_offset()))
        .collect();
    assert_eq!(offsets.len(), 3, "expected three located diagnostics");
    assert!(
        offsets[0] < offsets[1] && offsets[1] < offsets[2],
        "diagnostics should be at strictly increasing offsets, got {offsets:?}"
    );
}

#[test]
fn clean_first_block_then_bad_block_is_unchanged() {
    // Regression guard: when the first block parses cleanly, the second block's
    // error is reported normally with its specific Q-code (this path never
    // needed the re-parse and must be untouched by it).
    let input = "First good paragraph.\n\nSecond *bad paragraph.\n";
    let diags = diagnostics_for(input);

    let codes: Vec<&str> = diags.iter().filter_map(|d| d.code.as_deref()).collect();
    assert_eq!(
        codes,
        vec!["Q-2-12"],
        "expected exactly one Q-2-12 for the second block; got:\n{}",
        render_all(input, &diags)
    );
}

// ─── Block-detection robustness ──────────────────────────────────────────────
//
// The downstream re-parse resyncs at the next blank line. That heuristic alone
// would be wrong for constructs that legitimately contain blank lines (fenced
// code, display math, raw HTML, loose lists). It holds up because masking only
// *blanks* the handled prefix and never *truncates* the document: as long as
// resync lands at or before a construct's opening line, the construct is
// re-parsed in its entirety. These tests pin that property — a benign construct
// must not synthesize a spurious downstream diagnostic, and real errors on
// either side of it must still be found.

fn codes_for(input: &str) -> Vec<String> {
    diagnostics_for(input)
        .iter()
        .filter_map(|d| d.code.clone())
        .collect()
}

#[test]
fn fenced_code_with_internal_blank_after_error_is_not_spurious() {
    // Case A: error paragraph, blank, fenced code containing a blank line, then
    // a clean paragraph. Only the paragraph error should surface.
    let input = "Bad *para.\n\n```\ncode a\n\ncode b\n```\n\nClean para.\n";
    assert_eq!(codes_for(input), vec!["Q-2-12".to_string()]);
}

#[test]
fn emphasis_like_text_inside_fenced_code_is_not_reported() {
    // Case B: the fence content looks like an unclosed emphasis. Code is not
    // parsed for inlines, so only the real paragraph error should surface.
    let input = "Bad *para.\n\n```\nthis *is code not emphasis\n\nmore code\n```\n";
    assert_eq!(codes_for(input), vec!["Q-2-12".to_string()]);
}

#[test]
fn loose_list_after_error_is_not_spurious() {
    // Case C: a loose list (blank lines between items) after an erroring
    // paragraph is well-formed and must not add diagnostics.
    let input = "Bad *para.\n\n- item one\n\n- item two\n\n- item three\n";
    assert_eq!(codes_for(input), vec!["Q-2-12".to_string()]);
}

#[test]
fn fence_with_no_blank_separation_does_not_produce_spurious_error() {
    // Case D (adversarial): an erroring paragraph immediately followed (no blank
    // line) by a fence with an internal blank line. The blank-line scan can land
    // inside the fence, but the orphaned tail auto-closes at EOF rather than
    // erroring, so only the paragraph error surfaces.
    let input = "Bad *para\n```\ncode stuff\n\nmore code\n```\n";
    assert_eq!(codes_for(input), vec!["Q-2-12".to_string()]);
}

#[test]
fn display_math_with_no_blank_separation_does_not_produce_spurious_error() {
    // Case E (adversarial): same shape as D but with display math.
    let input = "Bad *para\n$$\nx = 1\n\ny = 2\n$$\n";
    assert_eq!(codes_for(input), vec!["Q-2-12".to_string()]);
}

#[test]
fn fence_between_two_errors_is_skipped_and_both_errors_found() {
    // Case F: a fence with internal blank lines sits between two erroring
    // paragraphs. Both real errors must be found and the fence skipped cleanly.
    let input = "First *bad\n\n```\na\n\nb\n```\n\nSecond _bad\n";
    assert_eq!(
        codes_for(input),
        vec!["Q-2-12".to_string(), "Q-2-5".to_string()]
    );
}

#[test]
fn raw_html_block_between_errors_is_skipped() {
    // Case G: a raw HTML block with an internal blank line between two erroring
    // paragraphs must not error, and both real errors must be found.
    let input = "Bad *para\n\n<div>\n\n</div>\n\nTail _bad\n";
    assert_eq!(
        codes_for(input),
        vec!["Q-2-12".to_string(), "Q-2-5".to_string()]
    );
}
