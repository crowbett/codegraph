package api

import "example.com/app/store"

type wrapped struct {
	Name string
	*store.Store
}

func (w *wrapped) lookup() {
	w.Store.Get("a")
}

func named() (st *store.Store, err error) {
	st = store.New()
	st.Get("b")
	return
}
