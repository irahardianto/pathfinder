//! Conformance test types, assertion IDs, and tier evaluation.

use std::collections::BTreeMap;
use std::fmt;

/// The 16 Conformance Assertions (C-1 through C-16) per Scope Card WS-9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssertionId {
    /// C-1: `locate` on a semantic path returns the definition file:line.
    C1,
    /// C-2: `locate(file, line)` returns the enclosing semantic path.
    C2,
    /// C-3: `inspect` returns a self-contained slice with attached comments/decorators (TS-C1).
    C3,
    /// C-4: `trace(scope=callers)` returns known caller with `Confidence::Verified`.
    C4,
    /// C-5: `trace(scope=references)` includes known non-call reference.
    C5,
    /// C-6: goto-implementation on interface resolves to concrete impl file, NOT declaration file (the product thesis).
    C6,
    /// C-7: zero-caller symbol returns `Evidence{Verified, []}` (INV-1 real zero).
    C7,
    /// C-8: cold start: query within 500ms of boot returns `not_ready` or correct answer — never fake zero (INV-1).
    C8,
    /// C-9: probe reaches `Ready` within `readiness_budget()`.
    C9,
    /// C-10: absent LSP binary returns `Unavailable` with install hint, never silent empty (INV-1).
    C10,
    /// C-11: `name_point` indexes byte inside symbol's name identifier (TS-A1, INV-7).
    C11,
    /// C-12: non-ASCII declaration line resolves correctly (TS-A2, INV-7).
    C12,
    /// C-13: `impl Trait for TypeElsewhere` `inspect` returns body/error, never `Ok("")` (TS-A3).
    C13,
    /// C-14: round trip: generate path -> parse -> resolve -> same byte_range (TS-A4/A7/A9/A10/A11, TS-C2).
    C14,
    /// C-15: `inspect(p).start_line == locate(p).line` (TS-A7).
    C15,
    /// C-16: warm path: with LSP fully Ready, zero confidently-wrong-empties on annotated / non-ASCII / cross-file-impl fixtures (TS-D6, INV-7).
    C16,
}

impl AssertionId {
    /// Returns all 16 conformance assertions in order.
    #[must_use]
    pub const fn all() -> [Self; 16] {
        [
            Self::C1,
            Self::C2,
            Self::C3,
            Self::C4,
            Self::C5,
            Self::C6,
            Self::C7,
            Self::C8,
            Self::C9,
            Self::C10,
            Self::C11,
            Self::C12,
            Self::C13,
            Self::C14,
            Self::C15,
            Self::C16,
        ]
    }

    /// The short identifier (e.g. "C-1").
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::C1 => "C-1",
            Self::C2 => "C-2",
            Self::C3 => "C-3",
            Self::C4 => "C-4",
            Self::C5 => "C-5",
            Self::C6 => "C-6",
            Self::C7 => "C-7",
            Self::C8 => "C-8",
            Self::C9 => "C-9",
            Self::C10 => "C-10",
            Self::C11 => "C-11",
            Self::C12 => "C-12",
            Self::C13 => "C-13",
            Self::C14 => "C-14",
            Self::C15 => "C-15",
            Self::C16 => "C-16",
        }
    }

    /// One-line description of the assertion.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::C1 => "locate returns definition file:line",
            Self::C2 => "locate(file, line) returns enclosing semantic path",
            Self::C3 => "inspect returns self-contained slice with attached comments/decorators",
            Self::C4 => "trace(scope=callers) returns known caller with Confidence::Verified",
            Self::C5 => "trace(scope=references) includes known non-call reference",
            Self::C6 => "goto-implementation on interface resolves to concrete impl file, NOT declaration file",
            Self::C7 => "zero-caller symbol returns Evidence{Verified, []}",
            Self::C8 => "cold start: query within 500ms of boot returns not_ready or correct answer",
            Self::C9 => "probe reaches Ready within readiness_budget()",
            Self::C10 => "absent LSP binary returns Unavailable with install hint, never silent empty",
            Self::C11 => "name_point indexes byte inside symbol's name identifier",
            Self::C12 => "non-ASCII declaration line resolves correctly",
            Self::C13 => "impl Trait for TypeElsewhere inspect returns body/error, never Ok(\"\")",
            Self::C14 => "round trip: generate path -> parse -> resolve -> same byte_range",
            Self::C15 => "inspect(p).start_line == locate(p).line",
            Self::C16 => "warm path: with LSP fully Ready, zero confidently-wrong-empties",
        }
    }

    /// Primary finding or invariant defended by this assertion.
    #[must_use]
    pub const fn defends(self) -> &'static str {
        match self {
            Self::C1 => "addressing",
            Self::C2 => "addressing",
            Self::C3 => "TS-C1",
            Self::C4 => "call hierarchy",
            Self::C5 => "references ≠ calls",
            Self::C6 => "product thesis",
            Self::C7 => "INV-1 (real zero)",
            Self::C8 => "INV-1 (fake zero)",
            Self::C9 => "WS-3.1",
            Self::C10 => "INV-1",
            Self::C11 => "TS-A1, INV-7",
            Self::C12 => "TS-A2, INV-7",
            Self::C13 => "TS-A3",
            Self::C14 => "TS-A4/A7/A9/A10/A11, TS-C2",
            Self::C15 => "TS-A7",
            Self::C16 => "TS-D6, INV-7",
        }
    }

    /// Whether this assertion is primarily AST-driven (Tree-sitter) or LSP-driven.
    #[must_use]
    pub const fn is_ast_focused(self) -> bool {
        matches!(
            self,
            Self::C1
                | Self::C2
                | Self::C3
                | Self::C11
                | Self::C12
                | Self::C13
                | Self::C14
                | Self::C15
        )
    }
}

impl fmt::Display for AssertionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code())
    }
}

/// Language identifier for conformance testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LanguageId {
    Rust,
    Go,
    TypeScript,
    Python,
    Java,
}

impl LanguageId {
    /// All supported languages under test.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Rust,
            Self::Go,
            Self::TypeScript,
            Self::Python,
            Self::Java,
        ]
    }

    /// Canonical language name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Go => "Go",
            Self::TypeScript => "TypeScript",
            Self::Python => "Python",
            Self::Java => "Java",
        }
    }

    /// Lowercase directory / ID name.
    #[must_use]
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Java => "java",
        }
    }

    /// Default file extension for the language.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Rust => "rs",
            Self::Go => "go",
            Self::TypeScript => "ts",
            Self::Python => "py",
            Self::Java => "java",
        }
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Conformance tier level according to WS-9 specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LanguageTier {
    /// Tier 1 (supported): all 16 assertions pass.
    Tier1,
    /// Tier 2 (partial): C-1..C-3, C-11, C-14, C-15 pass; some LSP assertion fails. Documented, shipped.
    Tier2,
    /// Unsupported: AST baseline criteria not met.
    Unsupported,
}

impl fmt::Display for LanguageTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tier1 => write!(f, "Tier 1 (Fully Supported)"),
            Self::Tier2 => write!(f, "Tier 2 (Partial / AST-Verified)"),
            Self::Unsupported => write!(f, "Unsupported"),
        }
    }
}

/// Execution outcome of an assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssertionStatus {
    Passed,
    Failed { reason: String },
    Skipped { reason: String },
}

impl AssertionStatus {
    #[must_use]
    pub fn is_passed(&self) -> bool {
        matches!(self, Self::Passed)
    }

    #[must_use]
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Passed => "✅ PASS",
            Self::Failed { .. } => "❌ FAIL",
            Self::Skipped { .. } => "⚠️ SKIP",
        }
    }
}

/// Conformance matrix storing all assertion outcomes across languages.
#[derive(Debug, Clone, Default)]
pub struct ConformanceMatrix {
    results: BTreeMap<(LanguageId, AssertionId), AssertionStatus>,
}

impl ConformanceMatrix {
    /// Create a new empty conformance matrix.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the outcome of an assertion for a language.
    pub fn record(&mut self, lang: LanguageId, assertion: AssertionId, status: AssertionStatus) {
        self.results.insert((lang, assertion), status);
    }

    /// Get the status of a specific assertion for a language.
    #[must_use]
    pub fn get(&self, lang: LanguageId, assertion: AssertionId) -> Option<&AssertionStatus> {
        self.results.get(&(lang, assertion))
    }

    /// Compute the tier level for a given language.
    #[must_use]
    pub fn tier_for(&self, lang: LanguageId) -> LanguageTier {
        let all_pass = AssertionId::all()
            .iter()
            .all(|&a| self.get(lang, a).is_some_and(AssertionStatus::is_passed));

        if all_pass {
            return LanguageTier::Tier1;
        }

        // Tier 2 baseline: C-1, C-2, C-3, C-11, C-14, C-15 must pass
        let ast_baseline = [
            AssertionId::C1,
            AssertionId::C2,
            AssertionId::C3,
            AssertionId::C11,
            AssertionId::C14,
            AssertionId::C15,
        ];

        let ast_pass = ast_baseline
            .iter()
            .all(|&a| self.get(lang, a).is_some_and(AssertionStatus::is_passed));

        if ast_pass {
            LanguageTier::Tier2
        } else {
            LanguageTier::Unsupported
        }
    }

    /// Generates a Markdown Language Tier table for README.md.
    #[must_use]
    pub fn generate_tier_table(&self) -> String {
        let mut out = String::new();
        out.push_str("<!-- CONFORMANCE_TIER_TABLE_START -->\n");
        out.push_str("### Language Support & Conformance Tiers\n\n");
        out.push_str(
            "Language maturity is verified by the automated conformance suite (WS-9, 16 assertions C-1 through C-16):\n\n",
        );
        out.push_str("| Language | Tier | Conformance Score | AST Navigation (C-1..C-3, C-11..C-15) | LSP Resolution (C-4..C-10, C-16) | Product Thesis (C-6) |\n");
        out.push_str("|---|---|---|---|---|---|\n");

        for lang in LanguageId::all() {
            let tier = self.tier_for(lang);
            let passed_count = AssertionId::all()
                .iter()
                .filter(|&&a| self.get(lang, a).is_some_and(AssertionStatus::is_passed))
                .count();

            let ast_pass = [
                AssertionId::C1,
                AssertionId::C2,
                AssertionId::C3,
                AssertionId::C11,
                AssertionId::C12,
                AssertionId::C13,
                AssertionId::C14,
                AssertionId::C15,
            ]
            .iter()
            .all(|&a| self.get(lang, a).is_some_and(AssertionStatus::is_passed));

            let lsp_pass = [
                AssertionId::C4,
                AssertionId::C5,
                AssertionId::C6,
                AssertionId::C7,
                AssertionId::C8,
                AssertionId::C9,
                AssertionId::C10,
                AssertionId::C16,
            ]
            .iter()
            .all(|&a| self.get(lang, a).is_some_and(AssertionStatus::is_passed));

            let c6_pass = self
                .get(lang, AssertionId::C6)
                .is_some_and(AssertionStatus::is_passed);

            let tier_str = match tier {
                LanguageTier::Tier1 => "**Tier 1** (Full)",
                LanguageTier::Tier2 => "**Tier 2** (Partial)",
                LanguageTier::Unsupported => "Unsupported",
            };

            let ast_str = if ast_pass {
                "✅ Verified"
            } else {
                "❌ Failed"
            };
            let lsp_str = if lsp_pass {
                "✅ Verified"
            } else {
                "⚠️ Degraded"
            };
            let c6_str = if c6_pass {
                "✅ Verified"
            } else {
                "❌ Unverified"
            };

            out.push_str(&format!(
                "| **{}** | {} | {}/16 | {} | {} | {} |\n",
                lang.as_str(),
                tier_str,
                passed_count,
                ast_str,
                lsp_str,
                c6_str
            ));
        }

        out.push_str("\n<!-- CONFORMANCE_TIER_TABLE_END -->");
        out
    }

    /// Generates full detailed assertion matrix table for reports.
    #[must_use]
    pub fn generate_matrix_table(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "| Assertion | Description | Defends | Rust | Go | TypeScript | Python | Java |\n",
        );
        out.push_str("|---|---|---|:---:|:---:|:---:|:---:|:---:|\n");

        for assertion in AssertionId::all() {
            let r_rust = self
                .get(LanguageId::Rust, assertion)
                .map_or("—", AssertionStatus::symbol);
            let r_go = self
                .get(LanguageId::Go, assertion)
                .map_or("—", AssertionStatus::symbol);
            let r_ts = self
                .get(LanguageId::TypeScript, assertion)
                .map_or("—", AssertionStatus::symbol);
            let r_py = self
                .get(LanguageId::Python, assertion)
                .map_or("—", AssertionStatus::symbol);
            let r_java = self
                .get(LanguageId::Java, assertion)
                .map_or("—", AssertionStatus::symbol);

            out.push_str(&format!(
                "| **{}** | {} | `{}` | {} | {} | {} | {} | {} |\n",
                assertion.code(),
                assertion.description(),
                assertion.defends(),
                r_rust,
                r_go,
                r_ts,
                r_py,
                r_java
            ));
        }

        out
    }
}
