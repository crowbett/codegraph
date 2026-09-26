export interface Tool {
  run(x: number): number;
}

export class Store {
  get(k: string): string {
    return k;
  }
}

export function makeStore(): Store {
  return new Store();
}
