package api

import "example.com/app/store"

func local[T any](v T) *store.Store { return nil }

func generic() {
	store.Parse[int]("a")
	local[string]("b")
	store.Parse[int]("c").Get("d")
	x := local[int](1)
	x.Get("e")
	fns := map[string]func(int){}
	fns["k"](1)
}
