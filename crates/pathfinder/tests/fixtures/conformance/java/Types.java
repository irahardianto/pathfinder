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

    // 🦀 café 🌍 greeting
    public static String nonAsciiHello() {
        return "hello";
    }
}
