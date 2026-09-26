class Base {
  run(): number {
    return 1;
  }
}

class Item {
  go(): number {
    return 2;
  }
}

class Bag {
  values(): Item {
    return new Item();
  }
  go(): number {
    return 3;
  }
}

export function g<T extends Base>(x: T): number {
  return x.run();
}

export function v(b: Bag): number {
  return b.values().go();
}
