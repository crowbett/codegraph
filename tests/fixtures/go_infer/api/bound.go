package api

type Creator[T any] interface {
	Create(v T) string
}

type intCreator struct{}

func (intCreator) Create(v int) string { return "" }

type Box[T any, C Creator[T]] struct {
	v T
}

func (b *Box[U, VC]) Render() string {
	var vc VC
	return vc.Create(b.v)
}

func Make[T any, C Creator[T]](v T) string {
	var c C
	return c.Create(v)
}
