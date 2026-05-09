package com.example;

/**
 * SealedHierarchy — AC-1.3 fixture (Java 17+).
 *
 * Verifies:
 *  - Sealed class extracted as SymbolKind::Class.
 *  - Inner record types (Circle, Rectangle) appear as SymbolKind::Struct children.
 */
public sealed class Shape permits Shape.Circle, Shape.Rectangle {

    /** Sealed sub-type: circle. */
    public record Circle(double radius) implements Shape {
        public double area() {
            return Math.PI * radius * radius;
        }
    }

    /** Sealed sub-type: rectangle. */
    public record Rectangle(double w, double h) implements Shape {
        public double area() {
            return w * h;
        }
    }

    /** Abstract area method to be implemented by permitted sub-types. */
    public abstract double area();
}
