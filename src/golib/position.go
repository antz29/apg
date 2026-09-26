package main

import (
	"go/ast"
	"go/printer"
	"go/token"
	"strings"

	"golang.org/x/tools/go/packages"
)

// spanStart is the 0-based start offset of a declaration, including any doc
// comment.
func spanStart(node ast.Node, p *packages.Package) int {
	start := p.Fset.Position(node.Pos()).Offset
	switch d := node.(type) {
	case *ast.GenDecl:
		if d.Doc != nil {
			start = p.Fset.Position(d.Doc.Pos()).Offset
		}
	case *ast.FuncDecl:
		if d.Doc != nil {
			start = p.Fset.Position(d.Doc.Pos()).Offset
		}
	}
	return start
}

// offsetLine returns the 1-based line number containing a 0-based byte offset.
// `end` offsets are exclusive, so callers pass end-1 for the last byte.
func offsetLine(f *token.File, offset int) int {
	if f == nil {
		return 1
	}
	if offset >= f.Size() {
		offset = f.Size() - 1
	}
	if offset < 0 {
		offset = 0
	}
	return f.Line(f.Pos(offset))
}

func exprString(e ast.Expr) string {
	if e == nil {
		return ""
	}
	var sb strings.Builder
	printer.Fprint(&sb, token.NewFileSet(), e)
	return strings.TrimSpace(sb.String())
}
