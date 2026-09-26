import { Tool, makeStore, Store } from '@acme/core';

class Service {
  constructor(private readonly store: Store) {}

  lookup(k: string): string {
    return this.store.get(k);
  }
}

export async function go(t: Tool): Promise<number> {
  const s = makeStore();
  s.get('a');
  const m = new Map<string, string>();
  m.get('x');
  new Service(s).lookup('k');
  return t.run(1);
}
