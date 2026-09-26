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
