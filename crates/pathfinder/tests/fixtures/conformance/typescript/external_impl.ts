import { AppConfig } from "./types";

/**
 * ConfigManager consumes AppConfig declared in types.ts.
 */
export class ConfigManager {
  config: AppConfig;

  constructor(cfg: AppConfig) {
    this.config = cfg;
  }

  getTimeout(): number {
    return this.config.timeoutSeconds;
  }
}
