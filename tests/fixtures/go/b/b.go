package b

import "example.com/app/a"

// Helper shares its name with a.Helper.
func Helper() string { return "b" }

// Use calls into package a.
func Use(s *a.Store) string {
	return s.Get("k") + a.Helper()
}
