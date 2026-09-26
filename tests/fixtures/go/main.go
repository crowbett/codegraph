package main

import (
	"fmt"
	"net/http"

	"example.com/app/a"
	bb "example.com/app/b"
)

func main() {
	s := a.NewStore()
	fmt.Println(s.Get("x"), a.Helper(), bb.Helper(), bb.Use(s))
	http.HandleFunc("/", handle)
	run()
}

func run() { helper() }

func helper() {}

func handle(w http.ResponseWriter, r *http.Request) {}
