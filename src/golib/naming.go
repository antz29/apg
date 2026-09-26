package main

import (
	"go/ast"
	"go/types"
	"path/filepath"
	"strings"
)

// funcKey is the canonical FQN the ingestor will render for a function
// declaration (SPEC §4): parent.name, or parent.init#<basename> for Go init.
func funcKey(parent, name, file string) string {
	if name == "init" {
		return parent + ".init#" + filepath.Base(file)
	}
	return parent + "." + name
}

// funcFQNAny returns the FQN regardless of module membership. Universe-scope
// interface methods (e.g. error.Error) are returned as "error.Error".
func funcFQNAny(obj types.Object) string {
	if obj == nil {
		return ""
	}
	fn, ok := obj.(*types.Func)
	if !ok {
		return ""
	}
	sig := fn.Type().(*types.Signature)
	if sig.Recv() != nil {
		rn := recvTypeName(sig.Recv().Type())
		if rn == "" {
			return ""
		}
		if obj.Pkg() == nil {
			return rn + "." + fn.Name()
		}
		return obj.Pkg().Path() + "." + rn + "." + fn.Name()
	}
	if obj.Pkg() == nil {
		return ""
	}
	return obj.Pkg().Path() + "." + fn.Name()
}

func inProjectModules(pkgPath string, modSet map[string]bool) bool {
	for mod := range modSet {
		if pkgPath == mod || strings.HasPrefix(pkgPath, mod+"/") {
			return true
		}
	}
	return false
}

func recvTypeName(t types.Type) string {
	switch tt := t.(type) {
	case *types.Named:
		return tt.Obj().Name()
	case *types.Pointer:
		return recvTypeName(tt.Elem())
	default:
		return ""
	}
}

func recvTypeNameAST(t ast.Expr) string {
	switch tt := t.(type) {
	case *ast.Ident:
		return tt.Name
	case *ast.StarExpr:
		return recvTypeNameAST(tt.X)
	case *ast.IndexExpr:
		return recvTypeNameAST(tt.X)
	case *ast.IndexListExpr:
		return recvTypeNameAST(tt.X)
	default:
		return ""
	}
}

func typeFQN(t types.Type, modSet map[string]bool) string {
	switch tt := t.(type) {
	case *types.Named:
		obj := tt.Obj()
		if obj.Pkg() == nil {
			return ""
		}
		if !inProjectModules(obj.Pkg().Path(), modSet) {
			return ""
		}
		return obj.Pkg().Path() + "." + obj.Name()
	case *types.Pointer:
		return typeFQN(tt.Elem(), modSet)
	default:
		return ""
	}
}

// typeFQNAny returns the FQN of a named type regardless of module membership.
func typeFQNAny(t types.Type) string {
	switch tt := t.(type) {
	case *types.Named:
		obj := tt.Obj()
		if obj.Pkg() == nil {
			return ""
		}
		return obj.Pkg().Path() + "." + obj.Name()
	case *types.Pointer:
		return typeFQNAny(tt.Elem())
	default:
		return ""
	}
}
