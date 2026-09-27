package store

import (
	"encoding"
	"flag"
	"fmt"
)

// Value embeds stdlib interfaces whose method sets are known.
type Value interface {
	flag.Getter
	fmt.GoStringer
	encoding.TextMarshaler
	encoding.TextUnmarshaler
	IsSet() bool
}

type boolValue struct{ v bool }

func (b *boolValue) String() string                   { return "" }
func (b *boolValue) Set(s string) error               { return nil }
func (b *boolValue) Get() any                         { return b.v }
func (b *boolValue) GoString() string                 { return "" }
func (b *boolValue) MarshalText() ([]byte, error)     { return nil, nil }
func (b *boolValue) UnmarshalText(text []byte) error { return nil }
func (b *boolValue) IsSet() bool                      { return b.v }

// noGet lacks Get: not a Value.
type noGet struct{}

func (noGet) String() string                   { return "" }
func (noGet) Set(s string) error               { return nil }
func (noGet) GoString() string                 { return "" }
func (noGet) MarshalText() ([]byte, error)     { return nil, nil }
func (noGet) UnmarshalText(text []byte) error { return nil }
func (noGet) IsSet() bool                      { return false }
