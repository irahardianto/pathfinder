//! Conformance fixtures definitions and metadata per supported language.

use super::types::LanguageId;
use std::path::{Path, PathBuf};

/// Metadata and symbol descriptors for a language conformance fixture repository.
#[derive(Debug, Clone)]
pub struct LanguageFixture {
    /// Target language ID.
    pub language: LanguageId,
    /// Root directory of the fixture.
    pub root_dir: PathBuf,
    /// Trait/interface declaration file (relative to root).
    pub trait_file: &'static str,
    /// Trait/interface method semantic path.
    pub trait_method_path: &'static str,
    /// Concrete implementation file (relative to root).
    pub impl_file: &'static str,
    /// Concrete implementation method semantic path.
    pub impl_method_path: &'static str,
    /// Call hierarchy chain file (relative to root).
    pub chain_file: &'static str,
    /// Leaf function in caller chain.
    pub chain_leaf_path: &'static str,
    /// Middle function in caller chain.
    pub chain_middle_path: &'static str,
    /// Root function in caller chain.
    pub chain_root_path: &'static str,
    /// Types and definitions file (relative to root).
    pub types_file: &'static str,
    /// Symbol containing non-call reference (type annotation / param).
    pub non_call_ref_path: &'static str,
    /// Symbol with genuinely zero callers.
    pub zero_caller_path: &'static str,
    /// Annotated / decorated declaration.
    pub decorated_path: &'static str,
    /// File containing non-ASCII characters on declaration line.
    pub non_ascii_file: &'static str,
    /// Symbol on non-ASCII declaration line.
    pub non_ascii_path: &'static str,
    /// File with impl block whose type is declared elsewhere.
    pub external_impl_file: &'static str,
    /// Symbol of method on type declared elsewhere.
    pub external_impl_path: &'static str,
}

impl LanguageFixture {
    /// Locate base directory for conformance fixtures.
    #[must_use]
    pub fn find_fixtures_root() -> PathBuf {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let direct = manifest_dir.join("tests/fixtures/conformance");
        if direct.exists() {
            return direct;
        }

        // Search upward if needed
        for ancestor in manifest_dir.ancestors() {
            let candidate = ancestor.join("crates/pathfinder/tests/fixtures/conformance");
            if candidate.exists() {
                return candidate;
            }
        }

        direct
    }

    /// Load fixture specification for a given language.
    #[must_use]
    pub fn for_language(lang: LanguageId) -> Self {
        let base_dir = Self::find_fixtures_root().join(lang.dir_name());

        match lang {
            LanguageId::Rust => Self {
                language: lang,
                root_dir: base_dir,
                trait_file: "src/service.rs",
                trait_method_path: "src/service.rs::Greeter.greet",
                impl_file: "src/service_impl.rs",
                impl_method_path: "src/service_impl.rs::GreeterService.greet",
                chain_file: "src/chain.rs",
                chain_leaf_path: "src/chain.rs::chain_leaf",
                chain_middle_path: "src/chain.rs::chain_middle",
                chain_root_path: "src/chain.rs::chain_root",
                types_file: "src/types.rs",
                non_call_ref_path: "src/types.rs::take_config",
                zero_caller_path: "src/types.rs::zero_caller_symbol",
                decorated_path: "src/types.rs::decorated_function",
                non_ascii_file: "src/types.rs",
                non_ascii_path: "src/types.rs::non_ascii_hello",
                external_impl_file: "src/external_impl.rs",
                external_impl_path: "src/external_impl.rs::ExternalConfig.summarize",
            },
            LanguageId::Go => Self {
                language: lang,
                root_dir: base_dir,
                trait_file: "service.go",
                trait_method_path: "service.go::Greeter.Greet",
                impl_file: "service_impl.go",
                impl_method_path: "service_impl.go::GreeterService.Greet",
                chain_file: "chain.go",
                chain_leaf_path: "chain.go::ChainLeaf",
                chain_middle_path: "chain.go::ChainMiddle",
                chain_root_path: "chain.go::ChainRoot",
                types_file: "types.go",
                non_call_ref_path: "types.go::UseConfig",
                zero_caller_path: "types.go::ZeroCallerSymbol",
                decorated_path: "types.go::DecoratedFunction",
                non_ascii_file: "types.go",
                non_ascii_path: "types.go::NonAsciiHello",
                external_impl_file: "external_impl.go",
                external_impl_path: "external_impl.go::AppConfig.String",
            },
            LanguageId::TypeScript => Self {
                language: lang,
                root_dir: base_dir,
                trait_file: "service.ts",
                trait_method_path: "service.ts::Greeter.greet",
                impl_file: "service_impl.ts",
                impl_method_path: "service_impl.ts::GreeterService.greet",
                chain_file: "chain.ts",
                chain_leaf_path: "chain.ts::chainLeaf",
                chain_middle_path: "chain.ts::chainMiddle",
                chain_root_path: "chain.ts::chainRoot",
                types_file: "types.ts",
                non_call_ref_path: "types.ts::useConfig",
                zero_caller_path: "types.ts::zeroCallerSymbol",
                decorated_path: "types.ts::DecoratedService",
                non_ascii_file: "types.ts",
                non_ascii_path: "types.ts::nonAsciiHello",
                external_impl_file: "external_impl.ts",
                external_impl_path: "external_impl.ts::ConfigManager.getTimeout",
            },
            LanguageId::Python => Self {
                language: lang,
                root_dir: base_dir,
                trait_file: "service.py",
                trait_method_path: "service.py::Greeter.greet",
                impl_file: "service_impl.py",
                impl_method_path: "service_impl.py::GreeterService.greet",
                chain_file: "chain.py",
                chain_leaf_path: "chain.py::chain_leaf",
                chain_middle_path: "chain.py::chain_middle",
                chain_root_path: "chain.py::chain_root",
                types_file: "types.py",
                non_call_ref_path: "types.py::use_config",
                zero_caller_path: "types.py::zero_caller_symbol",
                decorated_path: "types.py::decorated_function",
                non_ascii_file: "types.py",
                non_ascii_path: "types.py::non_ascii_hello",
                external_impl_file: "external_impl.py",
                external_impl_path: "external_impl.py::ConfigWrapper.get_timeout",
            },
            LanguageId::Java => Self {
                language: lang,
                root_dir: base_dir,
                trait_file: "Greeter.java",
                trait_method_path: "Greeter.java::Greeter.greet",
                impl_file: "GreeterImpl.java",
                impl_method_path: "GreeterImpl.java::GreeterImpl.greet",
                chain_file: "Chain.java",
                chain_leaf_path: "Chain.java::Chain.chainLeaf",
                chain_middle_path: "Chain.java::Chain.chainMiddle",
                chain_root_path: "Chain.java::Chain.chainRoot",
                types_file: "Types.java",
                non_call_ref_path: "Types.java::Types.useConfig",
                zero_caller_path: "Types.java::Types.zeroCallerSymbol",
                decorated_path: "GreeterImpl.java::GreeterImpl.greet",
                non_ascii_file: "Types.java",
                non_ascii_path: "Types.java::Types.nonAsciiHello",
                external_impl_file: "ExternalImpl.java",
                external_impl_path: "ExternalImpl.java::ExternalImpl.getTimeout",
            },
        }
    }
}
