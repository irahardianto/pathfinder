package com.example;

public class Types {
    public static class AppConfig {
        public int timeoutSeconds = 30;
        public int maxRetries = 3;
    }

    public static int useConfig(AppConfig cfg) {
        return cfg.timeoutSeconds;
    }

    public static boolean zeroCallerSymbol() {
        return true;
    }

    // Non-ASCII characters before the symbol name, on its own declaration line (TS-A2).
    public static String /* 🦀 café 🌍 */ nonAsciiHello() {
        return "hello";
    }
}
