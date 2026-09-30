import add, { helper } from "./lib/math";
import * as strings from "@lib/strings";
import { Greeter as G, Named } from "./lib/strings";

export class App extends G implements Named {
  name = "app";

  run(): number {
    helper();
    strings.helper();
    return add(1, 2);
  }
}

export const main = (): void => {
  const app = new App();
  app.run();
  app.greet("x");
};

export const registry = { helper, sum: add };
export const isApp = (x: unknown): boolean => x instanceof App;
export const pick = (flag: boolean) => {
  const local = helper;
  return flag ? local : App;
};

import { minus } from "./lib/math";
export const noop = (): void => {};
export const isNoop = (f: () => void = noop): boolean => f === noop && minus(2, 1) === 1;
