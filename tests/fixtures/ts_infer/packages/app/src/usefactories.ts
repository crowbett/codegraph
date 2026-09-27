import { computed, observable } from "./factories"
import * as f from "./factories"

export function use(): string {
    const c = computed(() => 1)
    c.recompute()
    const b = observable.box(1)
    b.get()
    const l = f.observable.list([1])
    l.get()
    return computed.describe()
}
