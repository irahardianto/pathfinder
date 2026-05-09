package com.example;

/**
 * EnumWithMethods — AC-1.4 fixture.
 *
 * Verifies:
 *  - Enum extracted as SymbolKind::Enum.
 *  - Public access level detected.
 *  - Method `isActive()` extracted as a Public Function child.
 *  - Enum constants (ACTIVE, INACTIVE) are NOT extracted as symbols.
 */
public enum Status {

    ACTIVE,
    INACTIVE,
    PENDING;

    public boolean isActive() {
        return this == ACTIVE;
    }

    public boolean isTerminal() {
        return this == INACTIVE;
    }

    @Override
    public String toString() {
        return name().toLowerCase();
    }
}
