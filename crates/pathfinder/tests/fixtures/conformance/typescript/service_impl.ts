import { Greeter } from "./service";

/**
 * GreeterService implements Greeter in a separate file.
 */
export class GreeterService implements Greeter {
  greetingPrefix: string;

  constructor(prefix: string) {
    this.greetingPrefix = prefix;
  }

  greet(name: string): string {
    return `${this.greetingPrefix}, ${name}!`;
  }
}
