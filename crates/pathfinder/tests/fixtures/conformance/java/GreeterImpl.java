package com.example;

/**
 * GreeterImpl implements Greeter in a separate file.
 */
public class GreeterImpl implements Greeter {
    private final String greetingPrefix;

    public GreeterImpl(String prefix) {
        this.greetingPrefix = prefix;
    }

    @Override
    public String greet(String name) {
        return this.greetingPrefix + ", " + name + "!";
    }
}
