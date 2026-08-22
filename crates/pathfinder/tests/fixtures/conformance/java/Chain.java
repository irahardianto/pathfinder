package com.example;

public class Chain {
    public static int chainLeaf() {
        return 42;
    }

    public static int chainMiddle() {
        return chainLeaf() + 1;
    }

    public static int chainRoot() {
        return chainMiddle() * 2;
    }
}
