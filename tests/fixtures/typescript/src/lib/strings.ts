export function helper(): string {
  return "strings";
}

export interface Named {
  name: string;
}

/** Greets people. */
export class Greeter {
  greet(name: string): string {
    return this.format(name);
  }

  private format(n: string): string {
    return `hi ${n}`;
  }
}

/** A counter with an accessor. */
export class Counter {
  count = 0;

  get isEmpty(): boolean {
    return this.count === 0;
  }

  describe(): string {
    return this.isEmpty ? "empty" : `${this.count}`;
  }
}
