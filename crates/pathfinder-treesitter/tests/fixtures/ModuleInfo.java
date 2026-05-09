/**
 * ModuleInfo — AC-1.3 edge-case fixture (module-info.java).
 *
 * A `module` declaration is not a class/interface/enum/record, so the
 * tree-sitter extractor must produce zero symbols without panicking.
 *
 * Note: module-info.java has no package statement and no class body.
 */
module com.example.app {
    requires java.base;
    requires java.logging;

    exports com.example.api;
    exports com.example.util to com.example.client;

    opens com.example.internal to com.fasterxml.jackson.databind;
}
