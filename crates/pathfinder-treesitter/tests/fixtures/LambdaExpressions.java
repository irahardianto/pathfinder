package com.example;

import java.util.Arrays;
import java.util.Comparator;
import java.util.List;
import java.util.function.Consumer;
import java.util.function.Predicate;

/**
 * LambdaExpressions — edge-case fixture.
 *
 * Verifies:
 *  - Class `LambdaExample` is extracted correctly.
 *  - Lambda expressions assigned to fields do NOT produce empty-name symbols.
 *  - Methods using lambdas internally are still extracted correctly.
 *  - No panic on complex lambda syntax (method references, multi-line, capturing).
 */
public class LambdaExample {

    // Field lambdas — must NOT produce symbol noise
    private final Comparator<String> byLength = (a, b) -> Integer.compare(a.length(), b.length());
    private final Predicate<Integer> isPositive = n -> n > 0;
    private final Consumer<String> printer = System.out::println;

    public void sortStrings(List<String> items) {
        items.sort(byLength);
    }

    public List<String> filterAndSort(List<String> items) {
        return items.stream()
                .filter(s -> !s.isEmpty())
                .sorted(Comparator.comparingInt(String::length))
                .toList();
    }

    public void processNumbers(int[] nums) {
        Arrays.stream(nums)
                .filter(n -> isPositive.test(n))
                .mapToObj(Integer::toString)
                .forEach(printer);
    }
}
