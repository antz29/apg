package main

import (
	"encoding/json"
	"fmt"
)

// --- Unified schema messages (SPEC §2) ---

type moduleMsg struct {
	Type string `json:"type"` // module
	Fqn  string `json:"fqn"`
}

type structMsg struct {
	Type      string `json:"type"` // struct
	ID        string `json:"id"`
	Parent    string `json:"parent"`
	Name      string `json:"name"`
	Path      string `json:"path"`
	Start     int    `json:"start"`
	End       int    `json:"end"`
	StartLine int    `json:"start_line"`
	EndLine   int    `json:"end_line"`
}

type funcMsg struct {
	Type      string   `json:"type"` // function
	ID        string   `json:"id"`
	Parent    string   `json:"parent"`
	Name      string   `json:"name"`
	Params    []string `json:"params"`
	File      string   `json:"file"`
	Path      string   `json:"path"`
	Start     int      `json:"start"`
	End       int      `json:"end"`
	StartLine int      `json:"start_line"`
	EndLine   int      `json:"end_line"`
}

type fileMsg struct {
	Type      string `json:"type"` // file
	Path      string `json:"path"`
	Parent    string `json:"parent"`
	StartLine int    `json:"start_line"`
	EndLine   int    `json:"end_line"`
}

type unresolvedMsg struct {
	Type     string `json:"type"` // unresolved
	Fqn      string `json:"fqn"`
	Category string `json:"category"`
}

type edgeMsg struct {
	Type       string `json:"type"` // contains | calls | uses | unresolved_call | unresolved_use
	From       string `json:"from"`
	To         string `json:"to"`
	TargetType string `json:"target_type,omitempty"`
}

var enc *json.Encoder

// nextNodeID is the monotonic opaque-id counter (SPEC §3). idPrefix
// (`--id-prefix`, default "n") keeps ids unique across frontends when a scan
// merges multiple languages, so `n1` from Go and `n1` from another frontend
// never collide in the shared stream.
var idPrefix = "n"
var nextNodeID int

func newNodeID() string {
	nextNodeID++
	return fmt.Sprintf("%s%d", idPrefix, nextNodeID)
}

// structID / funcID map canonical FQNs (parent.name, or parent.init#file for
// init) to the opaque ids assigned in pass 1, so edge records can reference
// declarations by id in pass 2. They are built over the FULL loaded package
// set (the full resolution context), never over just the emitted packages: a
// resolved cross-package reference must still find its target.
var structID map[string]string
var funcID map[string]string

// emittedID holds the opaque ids whose declaration node records are actually
// part of the emitted stream (the target set, or every package when no filter
// is in force). An edge to a declaration whose id is NOT in this set carries
// the target's canonical FQN instead of the opaque id, so a reference into a
// non-emitted (cached) package survives the ingestor's fact splice; see
// edgeEndpoint.
var emittedID map[string]bool

// edgeEndpoint resolves the `to` endpoint of a calls/uses edge for a target
// declaration with canonical FQN `fqn` and opaque `id`: the opaque id when the
// declaration is part of the emitted stream, else `fqn` itself. The ingestor's
// cached-fact splice resolves a bare FQN against the reused unit, so a
// reference authored by an emitted package to a declaration in a non-emitted
// package is preserved instead of being silently dropped. With no filter in
// force every declaration is emitted, so this is always `id` — byte-identical
// to a full scan.
func edgeEndpoint(fqn, id string) string {
	if emittedID[id] {
		return id
	}
	return fqn
}

// unresolvedSeen deduplicates unresolved node records by fqn.
var unresolvedSeen map[string]bool
