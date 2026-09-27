package api

import (
	"net/url"

	"example.com/app/store"
)

func closures() {
	do := func() *store.Store { return store.New() }
	do().Get("a")
	f := make(url.Values)
	f.Get("b")
}
