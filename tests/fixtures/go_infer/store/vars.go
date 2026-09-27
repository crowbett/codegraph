package store

// Default is a package-level var typed by its initializer.
var Default = &Store{}

// Shared is a package-level var with a written interface type.
var Shared Getter = &Store{}

// NewLocal aliases an instantiated generic constructor.
var NewLocal = Parse[int]

// NewPair aliases a constructor instantiated with two type arguments.
var NewPair = MakePair[int, string]

func MakePair[K any, V any]() *Cache { return nil }

type holder struct {
	st *Store
}

// Box is a var whose field is used from another file.
var Box = holder{st: New()}
