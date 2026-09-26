package api

import (
	"net/http"

	"example.com/app/store"
)

type Server struct {
	st  *store.Store
	get store.Getter
}

func (s *Server) handle(w http.ResponseWriter, r *http.Request) {
	st := store.New()
	st.Get("a")
	s.st.Get("b")
	s.get.Get("c")
	w.Header().Set("k", "v")
	for _, c := range []*store.Cache{} {
		c.Get("d")
	}
}
