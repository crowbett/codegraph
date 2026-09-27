import { Value } from "./factories"

export class Holder {
    private data: Map<string, Value<number>> = new Map()

    read(k: string): number {
        return this.data.get(k)!.get()
    }
}
