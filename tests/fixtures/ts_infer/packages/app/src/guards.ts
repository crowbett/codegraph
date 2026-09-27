import { Value, Computed } from "./factories"
import { Holder } from "./maps"

export function isValue(x: any): x is Value<number> {
    return x !== null
}

export const isComputed = null as any as (x: any) => x is Computed<number>

export function read(x: any, y: unknown): number {
    if (isValue(x)) {
        x.get()
    }
    if (isComputed(x)) {
        [1].map(() => x.recompute())
    }
    if (y instanceof Holder) {
        return y.read("k")
    }
    return 0
}
