package a

type Getter interface {
	Get() int
}

func Use[T Getter](x T) int {
	return x.Get()
}

type Server struct{}

func (Server) Close() {}
