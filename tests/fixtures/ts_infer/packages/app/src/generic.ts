export interface Box<T> {
    get(): T
}

export interface Named<T> extends Box<T> {
    name(): string
}

export class StrBox implements Named<string> {
    get(): string {
        return ""
    }
    name(): string {
        return ""
    }
}
