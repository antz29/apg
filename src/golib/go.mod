module apg/gofrontend

go 1.25.0

// Pin the toolchain that compiles this frontend: its compiler IS its
// type-checker, so the Go language ceiling is fixed by this toolchain. The
// exact CI pin lives in .github/workflows/release.yml (setup-go + GOTOOLCHAIN).
toolchain go1.27.1

require golang.org/x/tools v0.48.0

require (
	golang.org/x/mod v0.38.0 // indirect
	golang.org/x/sync v0.22.0 // indirect
)
