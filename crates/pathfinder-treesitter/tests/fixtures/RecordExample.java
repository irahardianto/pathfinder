package com.example;

/**
 * RecordExample — AC-1.4 fixture (Java 16+).
 *
 * Verifies:
 *  - Record extracted as SymbolKind::Struct.
 *  - Public access level detected.
 *  - Explicit method `distance()` extracted as a Function child.
 *  - Record components (x, y) are NOT extracted as field symbols.
 */
public record Point(int x, int y) {

    /** Custom compact constructor. */
    public Point {
        if (x < 0 || y < 0) {
            throw new IllegalArgumentException("coordinates must be non-negative");
        }
    }

    /** Derived method — should be extracted as a child. */
    public double distance() {
        return Math.sqrt((double) x * x + (double) y * y);
    }

    /** Static factory. */
    public static Point origin() {
        return new Point(0, 0);
    }
}
