// Package a is the first package.
package a

// Store holds values.
type Store struct {
	items map[string]string
}

// NewStore builds a Store.
func NewStore() *Store {
	return &Store{items: map[string]string{}}
}

// Get returns an item.
func (s *Store) Get(k string) string {
	return s.lookup(k)
}

func (s *Store) lookup(k string) string {
	return s.items[k]
}

// Helper is a package-level function named like b.Helper.
func Helper() string { return "a" }
