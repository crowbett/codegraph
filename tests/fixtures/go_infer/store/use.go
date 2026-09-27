package store

func useVars() {
	Default.Get("a")
	Shared.Get("b")
	NewLocal("x").Get("c")
	NewPair().Get("d")
	Box.st.Get("e")
}
