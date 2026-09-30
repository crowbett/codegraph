// Command gotruth lists which named types of a Go module implement which of its
// interfaces, according to go/types (the compiler's own type checker). The accuracy
// benchmark adds these pairs to scip-go's implementation relationships, which miss some
// (echo's cross-package value-receiver implementations of MiddlewareConfigurator).
//
// Usage: gotruth <module dir>. Output, one pair per line, tab-separated, paths relative to
// the module dir with forward slashes and 1-based lines:
//
//	<type file> <type line> <type name> <interface file> <interface line> <interface name>
package main

import (
	"fmt"
	"go/token"
	"go/types"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/tools/go/packages"
)

type named struct {
	obj  *types.TypeName
	file string
	line int
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: gotruth <module dir>")
		os.Exit(2)
	}
	root, err := filepath.Abs(os.Args[1])
	if err != nil {
		fatal(err)
	}
	fset := token.NewFileSet()
	cfg := &packages.Config{
		Mode:  packages.NeedName | packages.NeedTypes | packages.NeedSyntax | packages.NeedTypesInfo,
		Dir:   root,
		Fset:  fset,
		Tests: true,
	}
	pkgs, err := packages.Load(cfg, "./...")
	if err != nil {
		fatal(err)
	}
	// Types are compared inside one compilation: every loaded package (test variants
	// included) with the in-module packages it imports. Across variants the same
	// declaration is a different type, and `Implements` would fail on the signatures.
	lines := map[string]bool{}
	for _, p := range pkgs {
		if p.Types == nil {
			continue
		}
		var concrete, ifaces []named
		visited := map[*types.Package]bool{}
		var visit func(tp *types.Package)
		visit = func(tp *types.Package) {
			if visited[tp] {
				return
			}
			visited[tp] = true
			sc := tp.Scope()
			for _, n := range sc.Names() {
				tn, ok := sc.Lookup(n).(*types.TypeName)
				if !ok || tn.IsAlias() {
					continue
				}
				pos := fset.Position(tn.Pos())
				rel, err := filepath.Rel(root, pos.Filename)
				if err != nil || pos.Filename == "" || strings.HasPrefix(rel, "..") {
					continue
				}
				nd := named{tn, filepath.ToSlash(rel), pos.Line}
				if it, ok := tn.Type().Underlying().(*types.Interface); ok {
					// Every type satisfies an empty interface: no information.
					if it.NumMethods() > 0 {
						ifaces = append(ifaces, nd)
					}
				} else {
					concrete = append(concrete, nd)
				}
			}
			for _, imp := range tp.Imports() {
				visit(imp)
			}
		}
		visit(p.Types)
		for _, t := range concrete {
			nt, ok := t.obj.Type().(*types.Named)
			if !ok || nt.TypeParams().Len() > 0 {
				// Uninstantiated generic types cannot be checked against an interface.
				continue
			}
			for _, i := range ifaces {
				it := i.obj.Type().Underlying().(*types.Interface)
				if types.Implements(nt, it) || types.Implements(types.NewPointer(nt), it) {
					lines[fmt.Sprintf("%s\t%d\t%s\t%s\t%d\t%s",
						t.file, t.line, t.obj.Name(), i.file, i.line, i.obj.Name())] = true
				}
			}
		}
	}
	out := make([]string, 0, len(lines))
	for l := range lines {
		out = append(out, l)
	}
	sort.Strings(out)
	for _, l := range out {
		fmt.Println(l)
	}
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "gotruth:", err)
	os.Exit(1)
}
