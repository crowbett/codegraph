import { Ann, annFn, aliasHelper } from "./barrel"
import * as b from "./barrel"
export function make(this: Ann) {
  this.ext()
}
export function run() {
  annFn()
  aliasHelper()
  b.annFn()
  b.hidden()
}
