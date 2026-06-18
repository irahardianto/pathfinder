use super::*;

// ── Trait object safety ─────────────────────────────────────────────

#[test]
fn test_trait_is_object_safe() {
    // If this compiles, the trait is object-safe.
    let _: Box<dyn LanguagePlugin> = Box::new(RustPlugin);
    let _: &dyn LanguagePlugin = &GoPlugin;
}

// ── RustPlugin ──────────────────────────────────────────────────────

#[test]
fn test_rust_plugin_language_id() {
    assert_eq!(RustPlugin.language_id(), "rust");
}

#[test]
fn test_rust_plugin_file_extensions() {
    assert_eq!(RustPlugin.file_extensions(), &["rs"]);
}

#[test]
fn test_rust_plugin_marker_files() {
    assert_eq!(RustPlugin.marker_files(), &["Cargo.toml"]);
}

#[test]
fn test_rust_plugin_marker_search_depth() {
    assert_eq!(RustPlugin.marker_search_depth(), 2);
}

#[test]
fn test_rust_plugin_lsp_candidates() {
    let candidates = RustPlugin.lsp_candidates();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].binary, "rust-analyzer");
    assert!(candidates[0].default_args.is_empty());
}

#[test]
fn test_rust_plugin_install_hint() {
    let hint = RustPlugin.install_hint();
    assert!(hint.contains("rust-analyzer"));
}

// ── GoPlugin ────────────────────────────────────────────────────────

#[test]
fn test_go_plugin_language_id() {
    assert_eq!(GoPlugin.language_id(), "go");
}

#[test]
fn test_go_plugin_file_extensions() {
    assert_eq!(GoPlugin.file_extensions(), &["go"]);
}

#[test]
fn test_go_plugin_marker_files() {
    assert_eq!(GoPlugin.marker_files(), &["go.mod"]);
}

#[test]
fn test_go_plugin_marker_search_depth() {
    assert_eq!(GoPlugin.marker_search_depth(), 2);
}

#[test]
fn test_go_plugin_lsp_candidates() {
    let candidates = GoPlugin.lsp_candidates();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].binary, "gopls");
}

#[test]
fn test_go_plugin_install_hint() {
    let hint = GoPlugin.install_hint();
    assert!(hint.contains("gopls"));
}

// ── TypeScriptPlugin ────────────────────────────────────────────────

#[test]
fn test_typescript_plugin_language_id() {
    assert_eq!(TypeScriptPlugin.language_id(), "typescript");
}

#[test]
fn test_typescript_plugin_file_extensions() {
    let exts = TypeScriptPlugin.file_extensions();
    assert!(exts.contains(&"ts"));
    assert!(exts.contains(&"tsx"));
    assert!(exts.contains(&"js"));
    assert!(exts.contains(&"jsx"));
    assert!(exts.contains(&"mjs"));
    assert!(exts.contains(&"cjs"));
    assert!(exts.contains(&"vue"));
}

#[test]
fn test_typescript_plugin_marker_files() {
    let markers = TypeScriptPlugin.marker_files();
    assert_eq!(markers, &["tsconfig.json", "package.json"]);
}

#[test]
fn test_typescript_plugin_marker_search_depth() {
    assert_eq!(TypeScriptPlugin.marker_search_depth(), 2);
}

#[test]
fn test_typescript_plugin_lsp_candidates() {
    let candidates = TypeScriptPlugin.lsp_candidates();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].binary, "typescript-language-server");
    assert_eq!(candidates[0].default_args, &["--stdio"]);
}

#[test]
fn test_typescript_plugin_install_hint() {
    let hint = TypeScriptPlugin.install_hint();
    assert!(hint.contains("typescript-language-server"));
}

// ── PythonPlugin ────────────────────────────────────────────────────

#[test]
fn test_python_plugin_language_id() {
    assert_eq!(PythonPlugin.language_id(), "python");
}

#[test]
fn test_python_plugin_file_extensions() {
    let exts = PythonPlugin.file_extensions();
    assert!(exts.contains(&"py"));
    assert!(exts.contains(&"pyi"));
}

#[test]
fn test_python_plugin_marker_files() {
    let markers = PythonPlugin.marker_files();
    assert_eq!(markers, &["pyproject.toml", "setup.py", "requirements.txt"]);
}

#[test]
fn test_python_plugin_marker_search_depth() {
    assert_eq!(PythonPlugin.marker_search_depth(), 2);
}

#[test]
fn test_python_plugin_lsp_candidates() {
    let candidates = PythonPlugin.lsp_candidates();
    assert_eq!(candidates.len(), 5);
    assert_eq!(candidates[0].binary, "pyright-langserver");
    assert_eq!(candidates[0].default_args, &["--stdio"]);
    assert_eq!(candidates[1].binary, "basedpyright-langserver");
    assert_eq!(candidates[1].default_args, &["--stdio"]);
    assert_eq!(candidates[2].binary, "pylsp");
    assert_eq!(candidates[3].binary, "ruff");
    assert_eq!(candidates[3].default_args, &["server", "--stdio"]);
    assert_eq!(candidates[4].binary, "jedi-language-server");
}

#[test]
fn test_python_plugin_install_hint() {
    let hint = PythonPlugin.install_hint();
    assert!(hint.contains("pyright"));
    assert!(hint.contains("pylsp"));
}

// ── Registry ────────────────────────────────────────────────────────

#[test]
fn test_all_plugins_contains_all_five_languages() {
    let plugins = all_plugins();
    assert_eq!(plugins.len(), 5);
    let ids: Vec<&str> = plugins.iter().map(|p| p.language_id()).collect();
    assert!(ids.contains(&"rust"));
    assert!(ids.contains(&"go"));
    assert!(ids.contains(&"typescript"));
    assert!(ids.contains(&"python"));
    assert!(ids.contains(&"java"));
}

#[test]
fn test_plugin_for_language_found() {
    let plugin = plugin_for_language("rust").unwrap();
    assert_eq!(plugin.language_id(), "rust");
}

#[test]
fn test_plugin_for_language_found_java() {
    let plugin = plugin_for_language("java").unwrap();
    assert_eq!(plugin.language_id(), "java");
}

#[test]
fn test_plugin_for_language_not_found() {
    assert!(plugin_for_language("kotlin").is_none());
}

#[test]
fn test_plugin_for_extension_rs() {
    let plugin = plugin_for_extension("rs").unwrap();
    assert_eq!(plugin.language_id(), "rust");
}

#[test]
fn test_plugin_for_extension_go() {
    let plugin = plugin_for_extension("go").unwrap();
    assert_eq!(plugin.language_id(), "go");
}

#[test]
fn test_plugin_for_extension_ts() {
    let plugin = plugin_for_extension("ts").unwrap();
    assert_eq!(plugin.language_id(), "typescript");
}

#[test]
fn test_plugin_for_extension_vue() {
    let plugin = plugin_for_extension("vue").unwrap();
    assert_eq!(plugin.language_id(), "typescript");
}

#[test]
fn test_plugin_for_extension_py() {
    let plugin = plugin_for_extension("py").unwrap();
    assert_eq!(plugin.language_id(), "python");
}

#[test]
fn test_plugin_for_extension_java() {
    let plugin = plugin_for_extension("java").unwrap();
    assert_eq!(plugin.language_id(), "java");
}

#[test]
fn test_plugin_for_extension_unknown() {
    assert!(plugin_for_extension("kt").is_none());
}

// ── Cross-validation with existing code ─────────────────────────────

#[test]
fn test_plugins_match_language_id_for_extension() {
    // Verify that the plugin registry returns the same language_id
    // as the existing language_id_for_extension function for all known extensions.
    use crate::client::language_id_for_extension;

    for ext in &[
        "rs", "go", "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "py", "pyi", "java",
    ] {
        let from_fn = language_id_for_extension(ext);
        let from_plugin = plugin_for_extension(ext).map(LanguagePlugin::language_id);
        assert_eq!(
            from_fn, from_plugin,
            "Mismatch for extension .{ext}: fn={from_fn:?}, plugin={from_plugin:?}"
        );
    }
}

#[test]
fn test_all_plugins_have_unique_language_ids() {
    let plugins = all_plugins();
    let ids: Vec<&str> = plugins.iter().map(|p| p.language_id()).collect();
    let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(
        ids.len(),
        unique.len(),
        "Duplicate language IDs found: {ids:?}"
    );
}

fn check_extension_overlap(plugins: &[&dyn LanguagePlugin]) {
    let mut seen = std::collections::HashMap::new();
    for plugin in plugins {
        for ext in plugin.file_extensions() {
            if let Some(existing) = seen.insert(*ext, plugin.language_id()) {
                panic!(
                    "Extension .{ext} claimed by both '{existing}' and '{}'",
                    plugin.language_id()
                );
            }
        }
    }
}

#[test]
fn test_no_extension_overlap_between_plugins() {
    // Each extension should map to exactly one plugin.
    check_extension_overlap(all_plugins());
}

#[test]
#[should_panic(expected = "Extension .rs claimed by both 'rust' and 'rust'")]
fn test_extension_overlap_panic() {
    struct MockPlugin;
    impl LanguagePlugin for MockPlugin {
        fn language_id(&self) -> &'static str {
            "rust"
        }
        fn file_extensions(&self) -> &'static [&'static str] {
            &["rs"]
        }
        fn marker_files(&self) -> &'static [&'static str] {
            &[]
        }
        fn marker_search_depth(&self) -> u32 {
            0
        }
        fn lsp_candidates(&self) -> &[LspCandidate] {
            &[]
        }
        fn install_hint(&self) -> &'static str {
            ""
        }
    }
    let plugins: &[&dyn LanguagePlugin] = &[&RustPlugin, &MockPlugin];
    check_extension_overlap(plugins);
}

// ── JavaPlugin ──────────────────────────────────────────────────────

#[test]
fn test_java_plugin_language_id() {
    assert_eq!(JavaPlugin.language_id(), "java");
}

#[test]
fn test_java_plugin_file_extensions() {
    let exts = JavaPlugin.file_extensions();
    assert_eq!(exts, &["java"]);
}

#[test]
fn test_java_plugin_marker_files() {
    let markers = JavaPlugin.marker_files();
    assert!(markers.contains(&"pom.xml"));
    assert!(markers.contains(&"build.gradle"));
    assert!(markers.contains(&"build.gradle.kts"));
    assert!(markers.contains(&"settings.gradle"));
    assert!(markers.contains(&"settings.gradle.kts"));
}

#[test]
fn test_java_plugin_marker_search_depth() {
    assert_eq!(JavaPlugin.marker_search_depth(), 2);
}

#[test]
fn test_java_plugin_lsp_candidates() {
    let candidates = JavaPlugin.lsp_candidates();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].binary, "jdtls");
    assert!(candidates[0].default_args.is_empty());
}

#[test]
fn test_java_plugin_install_hint() {
    let hint = JavaPlugin.install_hint();
    assert!(hint.contains("jdtls"));
    assert!(hint.contains("JDK 21"));
}

#[test]
fn test_plugin_for_language_returns_correct_type() {
    let rust_plugin = plugin_for_language("rust").unwrap();
    assert!(rust_plugin.file_extensions().contains(&"rs"));
    assert!(!rust_plugin.file_extensions().contains(&"go"));
}

#[test]
fn test_plugin_for_extension_returns_correct_type() {
    let py_plugin = plugin_for_extension("py").unwrap();
    assert_eq!(py_plugin.language_id(), "python");

    let stub_plugin = plugin_for_extension("pyi").unwrap();
    assert_eq!(stub_plugin.language_id(), "python");
}

#[test]
fn test_all_plugins_marker_files_non_empty() {
    for plugin in all_plugins() {
        assert!(
            !plugin.marker_files().is_empty(),
            "{} should have at least one marker file",
            plugin.language_id()
        );
    }
}

#[test]
fn test_all_plugins_file_extensions_non_empty() {
    for plugin in all_plugins() {
        assert!(
            !plugin.file_extensions().is_empty(),
            "{} should have at least one file extension",
            plugin.language_id()
        );
    }
}

#[test]
fn test_all_plugins_lsp_candidates_non_empty() {
    for plugin in all_plugins() {
        assert!(
            !plugin.lsp_candidates().is_empty(),
            "{} should have at least one LSP candidate",
            plugin.language_id()
        );
    }
}

#[test]
fn test_all_plugins_install_hints_non_empty() {
    for plugin in all_plugins() {
        assert!(
            !plugin.install_hint().is_empty(),
            "{} should have non-empty install hint",
            plugin.language_id()
        );
    }
}

#[test]
fn test_lsp_candidate_binary_names_non_empty() {
    for plugin in all_plugins() {
        for candidate in plugin.lsp_candidates() {
            assert!(
                !candidate.binary.is_empty(),
                "{} should have non-empty binary name",
                plugin.language_id()
            );
        }
    }
}

#[test]
fn test_typescript_plugin_includes_vue_extension() {
    let exts = TypeScriptPlugin.file_extensions();
    assert!(
        exts.contains(&"vue"),
        "TypeScript plugin should handle .vue files"
    );
}

#[test]
fn test_python_plugin_multiple_candidates() {
    let candidates = PythonPlugin.lsp_candidates();
    assert!(
        candidates.len() >= 5,
        "Python should have at least 5 LSP candidates"
    );
}

#[test]
fn test_java_marker_search_depth_supports_monorepo() {
    assert!(
        JavaPlugin.marker_search_depth() >= 2,
        "Java should search at least depth 2 for monorepo support"
    );
}
