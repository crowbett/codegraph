package api

import "example.com/app/store"

func table() {
	tests := []struct {
		name string
		st   *store.Store
	}{
		{name: "a", st: store.New()},
	}
	for _, tt := range tests {
		tt.st.Get(tt.name)
	}
	one := struct{ st *store.Store }{st: store.New()}
	one.st.Get("b")
	byKey := map[string]struct{ get store.Getter }{}
	for _, v := range byKey {
		v.get.Get("c")
	}
}
