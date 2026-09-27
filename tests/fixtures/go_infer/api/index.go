package api

import "example.com/app/store"

type registry struct {
	byName map[string]*store.Store
	groups [][]*store.Store
}

func (r *registry) find(k string, all []*store.Store) {
	all[0].Get("a")
	st, ok := r.byName[k]
	if ok {
		st.Get("b")
	}
	for _, s := range r.groups[0] {
		s.Get("c")
	}
	r.groups[1][2].Get("d")
}
