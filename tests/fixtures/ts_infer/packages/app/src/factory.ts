export interface MapFactory {
    <K = any, V = any>(): Map<K, V>
    <K, V>(initialValues?: Array<[K, V]>, options?: Options): Map<
        K,
        V
    >
    <K, V>(
        initialValues?: Record<string, V>,
        options?: Options
    ): Map<K, V>
    <K, V>(initialValues?: Map<K, V>, options?: Options): Map<K, V>
}

export interface Later extends Options {
    <T = any>(value: T[], options?: Options): Later
    run(): number
}

export function useLater(l: Later): number {
    return l.run()
}

export interface Options {
    name?: string
}
