package com.example;

/**
 * InnerClasses — AC-1.6 / AC-1.7 fixture.
 *
 * Verifies:
 *  - Outer → Inner class hierarchy (AC-1.6).
 *  - Static nested class is a direct child of Outer.
 *  - Anonymous class (new Runnable() { ... }) must NOT produce empty-name
 *    symbols (AC-1.7 — no garbage, no panic).
 */
public class Outer {

    /** Non-static inner class. */
    public class Inner {
        void innerMethod() {
            System.out.println("inner");
        }
    }

    /** Static nested class. */
    public static class StaticNested {
        void nestedMethod() {
            System.out.println("nested");
        }
    }

    /** Field with anonymous class initializer — AC-1.7 edge case. */
    Runnable r = new Runnable() {
        @Override
        public void run() {
            System.out.println("anonymous runnable");
        }
    };
}
