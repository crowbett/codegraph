/** Adds two numbers. */
export default function add(a: number, b: number): number {
  return a + b;
}

export function helper(): string {
  return "math";
}

function subtract(a: number, b: number): number {
  return a - b;
}

export { subtract as minus };
