package b

type OpenCloser interface {
	Open()
	Close()
}

type Server struct{}

func (Server) Open()  {}
func (Server) Close() {}

func Use(o OpenCloser) { o.Close() }
