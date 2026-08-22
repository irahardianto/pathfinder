package com.example;

public class ExternalImpl {
    public Types.AppConfig config;

    public ExternalImpl(Types.AppConfig config) {
        this.config = config;
    }

    public int getTimeout() {
        return this.config.timeoutSeconds;
    }
}
