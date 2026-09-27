export interface Value<T> {
    get(): T
}

export interface Computed<T> {
    get(): T
    recompute(): void
}

export interface Options {
    name?: string
}

export interface ComputedFactory {
    (options: Options): Options
    <T>(fn: () => T, options?: Options): Computed<T>
    describe(): string
}

export interface BoxFactory {
    <T>(value: T): Value<T>
}

export interface Factory {
    <T>(value: T[]): Value<T[]>
    box: BoxFactory
    list: <T>(items: T[]) => Value<T[]>
}

function make(): any {
    return null
}

export const computed: ComputedFactory = function (a: any) {
    return a
} as any

export var observable: Factory = make()
