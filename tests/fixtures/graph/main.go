package main

func main() { a() }

func a() {
	b()
	c()
}

func b() { c() }

func c() { d() }

// d closes the cycle b -> c -> d -> b.
func d() { b() }

func orphan() {}
