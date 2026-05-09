/**
 * PackageInfo — AC-1.3 edge-case fixture (package-info.java).
 *
 * `package-info.java` has no class declarations (unlike module-info.java
 * which has no symbols of any kind). It typically contains:
 * - A `package` declaration
 * - Package-level annotations
 * - Documentation comments
 *
 * Expected: no symbols extracted, no panic. The extractor handles files
 * with no matching symbol nodes gracefully via recursive iteration that
 * never finds a named `class_kinds` or `function_kinds` node.
 */

@Deprecated
@NonNullByDefault
package fixtures;

/**
 * These annotations are not defined anywhere (compile would fail), but
 * that doesn't matter for tree-sitter parsing — the AST is still valid.
 * The extractor simply won't find any class/function nodes to extract.
 */
