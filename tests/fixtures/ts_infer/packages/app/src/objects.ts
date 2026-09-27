import { Value } from "./factories"

export function makeHasher() {
    return {
        update(n: number): void {},
        digest(): string {
            return ""
        },
    }
}

export function useHasher(v: { entries(): number }, w: { entries(): string }): string {
    const h = makeHasher()
    h.update(v.entries())
    w.entries()
    return h.digest()
}

export function either(a: Value<number> | boolean): number {
    return a.get()
}
