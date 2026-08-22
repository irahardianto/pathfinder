//! Conformance test suite runner executing all assertions across supported languages.

use super::assertions::*;
use super::fixtures::LanguageFixture;
use super::types::{AssertionId, ConformanceMatrix, LanguageId};

/// Runs the complete conformance test matrix across all 5 supported languages.
pub async fn run_conformance_suite() -> ConformanceMatrix {
    let mut matrix = ConformanceMatrix::new();

    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);

        // C-1: locate returns definition file:line
        let c1_status = check_c1(&fixture).await;
        matrix.record(lang, AssertionId::C1, c1_status);

        // C-2: locate(file, line) returns enclosing semantic path
        let c2_status = check_c2(&fixture).await;
        matrix.record(lang, AssertionId::C2, c2_status);

        // C-3: inspect returns self-contained slice with attached comments/decorators
        let c3_status = check_c3(&fixture).await;
        matrix.record(lang, AssertionId::C3, c3_status);

        // C-4: trace(scope=callers) returns known caller with Confidence::Verified
        let c4_status = check_c4(&fixture).await;
        matrix.record(lang, AssertionId::C4, c4_status);

        // C-5: trace(scope=references) includes known non-call reference
        let c5_status = check_c5(&fixture).await;
        matrix.record(lang, AssertionId::C5, c5_status);

        // C-6: goto-implementation on interface resolves to concrete impl file
        let c6_status = check_c6(&fixture).await;
        matrix.record(lang, AssertionId::C6, c6_status);

        // C-7: zero-caller symbol returns Evidence{Verified, []}
        let c7_status = check_c7(&fixture).await;
        matrix.record(lang, AssertionId::C7, c7_status);

        // C-8: cold start returns not_ready or correct answer — never fake zero
        let c8_status = check_c8(&fixture).await;
        matrix.record(lang, AssertionId::C8, c8_status);

        // C-9: probe reaches Ready within readiness_budget()
        let c9_status = check_c9(&fixture).await;
        matrix.record(lang, AssertionId::C9, c9_status);

        // C-10: absent LSP binary returns Unavailable with install hint
        let c10_status = check_c10(&fixture).await;
        matrix.record(lang, AssertionId::C10, c10_status);

        // C-11: name_point indexes byte inside symbol's name identifier
        let c11_status = check_c11(&fixture).await;
        matrix.record(lang, AssertionId::C11, c11_status);

        // C-12: non-ASCII declaration line resolves correctly
        let c12_status = check_c12(&fixture).await;
        matrix.record(lang, AssertionId::C12, c12_status);

        // C-13: impl Trait for TypeElsewhere inspect returns body, never Ok("")
        let c13_status = check_c13(&fixture).await;
        matrix.record(lang, AssertionId::C13, c13_status);

        // C-14: round trip: generate path -> parse -> resolve -> same byte_range
        let c14_status = check_c14(&fixture).await;
        matrix.record(lang, AssertionId::C14, c14_status);

        // C-15: inspect(p).start_line == locate(p).line
        let c15_status = check_c15(&fixture).await;
        matrix.record(lang, AssertionId::C15, c15_status);

        // C-16: warm path: zero confidently-wrong-empties
        let c16_status = check_c16(&fixture).await;
        matrix.record(lang, AssertionId::C16, c16_status);
    }

    matrix
}
