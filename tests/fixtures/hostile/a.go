package x

type T struct{}

func (s /*]*/ T /*[*/) F() { s.G() }

func (s T) G() {}
