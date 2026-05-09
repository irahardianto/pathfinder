package com.example;

/**
 * InterfaceWithDefaults — AC-1.4 fixture.
 *
 * Verifies:
 *  - Interface extracted as SymbolKind::Interface.
 *  - Public access level detected.
 *  - Both abstract and default methods are children.
 */
public interface Sortable {

    /** Abstract method — no body. */
    void sort();

    /** Default method — has a body (Java 8+). */
    default void printSorted() {
        sort();
        System.out.println("sorted");
    }

    /** Static interface method (Java 8+). */
    static Sortable noop() {
        return () -> {};
    }
}
