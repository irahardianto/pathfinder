package com.example;

/**
 * BasicClass — AC-1.3 / AC-1.4 / AC-1.5 fixture.
 *
 * Verifies:
 *  - Class extracted with correct kind and access level.
 *  - Constructor extracted as a Function child.
 *  - All four Java access levels detected (public / protected / private / package).
 *  - Fields (name, count) are NOT extracted as symbols.
 */
public class BasicClass {

    private String name;
    protected int count;

    public BasicClass(String name) {
        this.name = name;
        this.count = 0;
    }

    public String getName() {
        return name;
    }

    protected void increment() {
        count++;
    }

    private void helper() {
        // private helper — must appear as AccessLevel::Private
    }

    void packageMethod() {
        // package-private (no modifier) — must appear as AccessLevel::Package
    }
}
