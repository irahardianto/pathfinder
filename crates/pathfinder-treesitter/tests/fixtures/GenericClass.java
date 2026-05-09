package com.example;

import java.util.function.Function;

/**
 * GenericClass — AC-1.3 fixture.
 *
 * Verifies:
 *  - Generic class `Container<T>` extracts correctly (class name is "Container",
 *    not "Container<T extends ...>").
 *  - Generic method `<R> R transform(...)` extracts as a Function child with
 *    name "transform".
 */
public class Container<T extends Comparable<T>> {

    private T value;

    public Container(T value) {
        this.value = value;
    }

    public T getValue() {
        return value;
    }

    /** Generic method — verifies type params on method don't break extraction. */
    public <R> R transform(Function<T, R> fn) {
        return fn.apply(value);
    }

    @Override
    public String toString() {
        return "Container[" + value + "]";
    }
}
