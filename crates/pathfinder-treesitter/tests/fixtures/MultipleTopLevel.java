/**
 * MultipleTopLevel — AC-1.3 edge-case fixture.
 *
 * Java allows multiple top-level class/interface/enum declarations in a
 * single source file. This is unusual but valid (not private, not public
 * — typically package-private). Only one public top-level class is allowed
 * per file (matching filename), but multiple package-private classes/enums
 * are valid.
 *
 * Expected: ALL top-level types are extracted.
 *
 * The extractor iterates `named_children()` of the root `program` node.
 * Each `class_declaration`/`enum_declaration`/`interface_declaration`
 * at the root should be processed independently.
 */

// This file has NO public class — all are package-private.
// Multiple top-level classes is unusual but valid Java.

class FirstClass {
    void firstMethod() {}
}

interface SecondInterface {
    void secondMethod();
}

enum ThirdEnum {
    VALUE_A, VALUE_B;
    
    public String thirdMethod() { return "enum"; }
}

class FourthClass {
    void fourthMethod() {}
    
    // Inner class — child of FourthClass, not top-level
    class NestedInside {
        void nestedMethod() {}
    }
}
