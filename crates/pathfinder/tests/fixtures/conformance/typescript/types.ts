export interface AppConfig {
  timeoutSeconds: number;
  maxRetries: number;
}

/**
 * Non-call reference: type annotation parameter.
 */
export function useConfig(cfg: AppConfig): number {
  return cfg.timeoutSeconds;
}

/**
 * Symbol with genuinely zero callers.
 */
export function zeroCallerSymbol(): boolean {
  return true;
}

/**
 * Decorated / annotated class.
 */
export class DecoratedService {
  timeout: number = 50;

  getTimeout(): number {
    return this.timeout;
  }
}

// 🦀 café 🌍 greeting
export function nonAsciiHello(): string {
  return "hello";
}
