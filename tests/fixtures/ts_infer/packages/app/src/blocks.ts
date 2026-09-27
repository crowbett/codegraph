function test(name: string, f: () => void) { f() }

test("a", () => {
    class Store {
        add(a: number): number { return a }
    }
    const s = new Store()
    s.add(1)
})

test("b", () => {
    class Store {
        add(a: number): number { return a * 2 }
    }
    const s = new Store()
    s.add(1)
})

class Parent {
    run(): number { return 1 }
}

class Child extends Parent {
    run(): number {
        return super.run() + 1
    }
}

export function viaPrototype(): number {
    return Parent.prototype.run() + new Child().run()
}
