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

// DefaultName is used when none is set.
const DefaultName = "a"

// Options configures a Store.
type Options struct {
	Name string
	Make func() string
}

// Defaults are the default options.
func Defaults() Options {
	o := Options{Name: DefaultName, Make: Helper}
	if o.Name == DefaultName {
		o.Make = Helper
	}
	return o
}

func maker() func() string {
	f := Helper
	return f
}

func shadow(ch chan string) string {
	const DefaultName = "local"
	select {
	case Helper := <-ch:
		return Helper
	default:
	}
	return DefaultName
}
