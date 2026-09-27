package store

import "io"

// Getter reads values.
type Getter interface {
	Get(k string) (string, error)
}

// ReadGetter embeds a stdlib interface.
type ReadGetter interface {
	io.Reader
	Getter
}

type Store struct {
	data map[string]string
}

func New() *Store { return &Store{data: map[string]string{}} }

func (s *Store) Get(k string) (string, error) { return s.data[k], nil }

func (s *Store) Read(p []byte) (int, error) { return 0, nil }

// Cache embeds Store: promoted methods.
type Cache struct {
	*Store
}

// Wrong has Get with another shape: not a Getter.
type Wrong struct{}

func (Wrong) Get() string { return "" }

// Parse is a generic function called with explicit type arguments.
func Parse[T any](s string) *Store { return New() }
