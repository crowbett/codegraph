package api

import (
	"net/http"

	"example.com/app/store"
)

func useStoreVars() {
	store.Default.Get("a")
	store.Shared.Get("b")
	http.DefaultClient.Get("u")
}
