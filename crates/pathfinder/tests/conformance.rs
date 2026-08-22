//! Pathfinder Language Conformance Test Suite (WS-9).
//!
//! Validates that all supported languages meet the 16 Conformance Assertions
//! (C-1 through C-16) per Scope Card WS-9 and INV-1 / INV-4 / INV-7.
//!
//! Run with:
//!   `cargo test -p pathfinder-mcp --test conformance`

#![allow(warnings, clippy::all, clippy::pedantic)]

#[path = "conformance/mod.rs"]
mod conformance_suite;

use conformance_suite::{
    assertions::*,
    fixtures::LanguageFixture,
    runner::run_conformance_suite,
    types::{AssertionId, AssertionStatus, LanguageId, LanguageTier},
};

// ── Full Matrix Conformance Test ───────────────────────────────────────────

#[tokio::test]
async fn test_full_conformance_matrix_all_languages() {
    let matrix = run_conformance_suite().await;

    println!("\n=== PATHFINDER CONFORMANCE MATRIX ===");
    println!("{}", matrix.generate_matrix_table());
    println!("\n=== LANGUAGE TIERING ===");
    println!("{}", matrix.generate_tier_table());

    for lang in LanguageId::all() {
        let tier = matrix.tier_for(lang);
        println!("{}: {}", lang.as_str(), tier);

        for assertion in AssertionId::all() {
            let status = matrix
                .get(lang, assertion)
                .expect("assertion must be evaluated");
            if let AssertionStatus::Failed { reason } = status {
                panic!(
                    "Language '{}' FAILED assertion {} ({}): {}",
                    lang.as_str(),
                    assertion.code(),
                    assertion.description(),
                    reason
                );
            }
        }

        assert_eq!(
            tier,
            LanguageTier::Tier1,
            "Language '{}' must achieve Tier 1 (all 16 assertions pass)",
            lang.as_str()
        );
    }
}

// ── Individual Table-Driven Assertion Tests (C-1 .. C-16) ──────────────────

#[tokio::test]
async fn test_c1_locate_definition_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c1(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-1 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c2_locate_enclosing_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c2(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-2 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c3_inspect_slice_boundaries_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c3(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-3 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c4_trace_callers_verified_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c4(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-4 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c5_trace_references_non_call_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c5(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-5 failed for {}",
            lang.as_str()
        );
    }
}

/// Product thesis test: goto-implementation on interface resolves to concrete impl file, NOT declaration file.
#[tokio::test]
async fn test_c6_goto_implementation_product_thesis_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c6(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-6 (product thesis) failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c7_zero_caller_verified_empty_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c7(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-7 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c8_cold_start_never_fake_zero_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c8(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-8 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c9_probe_reaches_ready_budget_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c9(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-9 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c10_absent_lsp_returns_unavailable_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c10(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-10 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c11_name_point_inside_identifier_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c11(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-11 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c12_non_ascii_declaration_resolves_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c12(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-12 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c13_cross_file_impl_inspect_non_empty_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c13(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-13 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c14_round_trip_path_parse_resolve_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c14(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-14 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c15_inspect_locate_line_parity_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c15(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-15 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_c16_warm_path_zero_fake_empties_all_languages() {
    for lang in LanguageId::all() {
        let fixture = LanguageFixture::for_language(lang);
        let status = check_c16(&fixture).await;
        assert_eq!(
            status,
            AssertionStatus::Passed,
            "C-16 failed for {}",
            lang.as_str()
        );
    }
}

#[tokio::test]
async fn test_readme_conformance_tier_table_up_to_date() {
    let matrix = run_conformance_suite().await;
    let expected_table = matrix.generate_tier_table();

    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut readme_path = manifest_dir.join("../../README.md");
    if !readme_path.exists() {
        readme_path = manifest_dir.join("README.md");
    }

    if let Ok(content) = std::fs::read_to_string(&readme_path) {
        assert!(
            content.contains("<!-- CONFORMANCE_TIER_TABLE_START -->"),
            "README.md must contain the CONFORMANCE_TIER_TABLE_START marker"
        );
        assert!(
            content.contains("<!-- CONFORMANCE_TIER_TABLE_END -->"),
            "README.md must contain the CONFORMANCE_TIER_TABLE_END marker"
        );
        assert!(
            content.contains(&expected_table),
            "README.md conformance tier table must match generated table:\n{}",
            expected_table
        );
    }
}
