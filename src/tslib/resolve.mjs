// apg unified JS/TS scanner frontend — symbol/type resolution.
//
// Plain ESM sidecar (the `identity.mjs` precedent). Resolves a syntax node to
// the project declaration it denotes (via the injected `ctx.checker`), maps
// that declaration back to its opaque id / struct FQN, and classifies foreign
// targets. Anything not a project symbol is classified for an unresolved edge —
// never a fabricated FQN.

import ts from "typescript";
import path from "node:path";
import { ctx, declIdByKey, structFqnByKey, config } from "./state.mjs";
import { docStart } from "./declarations.mjs";

export function idForDecl(decl) {
  if (!decl) return "";
  const sf = decl.getSourceFile();
  return declIdByKey.get(sf.fileName + "@" + docStart(decl, sf)) || "";
}
export function structFqnOfDecl(decl) {
  if (!decl) return "";
  const sf = decl.getSourceFile();
  return structFqnByKey.get(sf.fileName + "@" + docStart(decl, sf)) || "";
}
export function categoryOfDecl(decl) {
  if (!decl) return "unknown";
  const f = decl.getSourceFile().fileName;
  if (f.includes("/typescript/lib/")) return "stdlib";
  if (f.includes("/node_modules/")) return "external";
  return "unknown";
}
export function unwrapAlias(sym) {
  let s = sym;
  for (let i = 0; i < 8 && s && s.flags & ts.SymbolFlags.Alias; i++) {
    s = ctx.checker.getAliasedSymbol(s);
  }
  return s;
}

// The name node to resolve for a call/new expression's callee.
export function nameLocation(expr) {
  if (ts.isIdentifier(expr)) return expr;
  if (ts.isPropertyAccessExpression(expr)) return expr.name;
  if (ts.isParenthesizedExpression(expr)) return nameLocation(expr.expression);
  if (ts.isCallExpression(expr) || ts.isNewExpression(expr)) return nameLocation(expr.expression);
  if (ts.isNonNullExpression(expr)) return nameLocation(expr.expression);
  return null;
}

// An unresolved callee's identity: its symbol name at full length, or — for a
// nameless callee expression — a sentinel keyed to the call site's repo-relative
// file and position, never a span of source text.
export function calleeIdentity(expr, sf) {
  const loc = nameLocation(expr);
  if (loc) return loc.getText(sf);
  const rel = path.relative(config.root, sf.fileName);
  const { line, character } = sf.getLineAndCharacterOfPosition(expr.getStart(sf));
  return `<nameless@${rel}:${line + 1}:${character + 1}>`;
}

// A declaration node that is callable (emits a Calls edge target).
export function isCallableDecl(decl) {
  return (
    ts.isFunctionDeclaration(decl) ||
    ts.isMethodDeclaration(decl) ||
    ts.isMethodSignature(decl) ||
    ts.isConstructorDeclaration(decl) ||
    ts.isGetAccessor(decl) ||
    ts.isSetAccessor(decl) ||
    ts.isVariableDeclaration(decl) // function-valued variable (`const f = () => …`)
  );
}

// The base name node of a type reference (unwraps generics/qualified names).
export function typeNameLocation(node) {
  if (ts.isTypeReferenceNode(node)) return typeNameLocation(node.typeName);
  if (ts.isArrayTypeNode(node)) return typeNameLocation(node.elementType);
  if (ts.isTypeQueryNode(node)) return typeNameLocation(node.exprName);
  if (ts.isExpressionWithTypeArguments(node)) return typeNameLocation(node.expression);
  if (ts.isQualifiedName(node)) return typeNameLocation(node.right);
  if (ts.isIdentifier(node)) return node;
  return null;
}

// A declaration node's own name identifier (skipped as a value-use: `class
// Button {}` uses Button, not the other way around).
export function isDeclName(node) {
  return !!node.parent && node.parent.name === node;
}

export function isTypeRefNode(node) {
  return (
    ts.isTypeReferenceNode(node) ||
    ts.isTypeQueryNode(node) ||
    ts.isExpressionWithTypeArguments(node) ||
    ts.isArrayTypeNode(node) ||
    ts.isTupleTypeNode(node) ||
    ts.isUnionTypeNode(node) ||
    ts.isIntersectionTypeNode(node) ||
    ts.isParenthesizedTypeNode(node)
  );
}
