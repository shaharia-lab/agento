// The reflector divergence map: what `google/jsonschema-go` produces for each
// Go shape a ported integration can contain, so the Rust side can say — per
// shape — whether `schemars` agrees and, where it does not, what a port must
// write instead.
//
// # Why this file exists at all
//
// A tool's input schema is not internal. `tools/list` hands it to the CLI and
// the CLI hands it to the model, so it is part of the same wire surface as the
// tool's name and its result text. #310 verified that surface for **a flat
// struct of required scalars** — which is all `current_time` is — and left a
// note on `new_tool`'s schema normalization naming the shapes the six ports
// (#312–#317) would hit first. This is that note, generated rather than
// written: every value below comes from `jsonschema.For` invoked the way
// `mcp.AddTool` invokes it, on one reference struct covering every shape class.
//
// The Rust half lives in `desktop/src-tauri/src/claude/schema_vectors.rs`. It
// declares the corresponding Rust shapes, runs them through
// `claude::new_tool`'s normalization, and asserts per property either equality
// or the *exact* divergence — so a `schemars` upgrade that changes one fails
// there, and a `jsonschema-go` upgrade that changes one fails here.
//
// # Two findings that are load-bearing for every port
//
//   - **`jsonschema:"required,…"` is not a directive.** `jsonschema-go` reads
//     the whole tag as the property's *description*, verbatim, `required,`
//     prefix included — and marks a field optional only on `omitempty` /
//     `omitzero`. Every params struct in `internal/integrations/` writes
//     `jsonschema:"required,Repository owner"` and none writes `omitempty`, so
//     **every field of every one of the 62 tools is required**, and the
//     description the model reads begins with `required,`. A port that "fixed"
//     either would change what the model sees.
//   - **An optional Go field is not an `Option<T>`.** `omitempty` leaves a
//     field out of `required` and does nothing to its `type`; `Option<T>` in
//     Rust renders `"type": ["string","null"]`. The Rust shape that matches is
//     `#[serde(default)] String`.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestJSONSchemaReflectVectors -update-jsonschema-reflect-vectors
package parity

import (
	"encoding/json"
	"flag"
	"os"
	"slices"
	"testing"

	"github.com/google/jsonschema-go/jsonschema"
)

const jsonschemaReflectVectorsFile = "jsonschema_reflect_vectors.json"

var updateJSONSchemaReflectVectors = flag.Bool("update-jsonschema-reflect-vectors", false,
	"rewrite jsonschema_reflect_vectors.json from this Go toolchain")

// reflectNested is the nested struct case. Go **inlines** it at every use;
// `schemars` lifts it into `$defs` and points a `$ref` at it.
type reflectNested struct {
	Name  string `json:"name" jsonschema:"The nested name"`
	Count int    `json:"count,omitempty" jsonschema:"The nested count"`
}

// reflectReference covers every shape class a ported params struct can contain.
//
// It is one struct rather than one per case on purpose: `$defs`/`$ref` and
// property ordering are properties of the *document*, not of a field, and a
// per-field generator would not show them.
type reflectReference struct {
	// The shape #310 already verified: a required scalar carrying the
	// `required,` tag every integration writes.
	RequiredString string `json:"required_string" jsonschema:"required,A required string"`
	// The same field made optional the only way Go has: `omitempty`. Note what
	// does *not* change — the type.
	OptionalString string `json:"optional_string,omitempty" jsonschema:"An optional string"`
	// `omitzero` is the newer spelling and `jsonschema-go` honors both.
	OmitZeroString string `json:"omitzero_string,omitzero" jsonschema:"An omitzero string"`
	// Untagged: the property has no description at all, which is a distinct
	// state from an empty one.
	Untagged string `json:"untagged"`

	Int   int     `json:"int" jsonschema:"A plain int"`
	Int64 int64   `json:"int64" jsonschema:"An int64"`
	Int32 int32   `json:"int32" jsonschema:"An int32"`
	Uint  uint    `json:"uint" jsonschema:"A uint"`
	Float float64 `json:"float" jsonschema:"A float64"`
	Bool  bool    `json:"bool" jsonschema:"A bool"`

	Strings     []string `json:"strings" jsonschema:"A string slice"`
	StringsOmit []string `json:"strings_omitempty,omitempty" jsonschema:"An omitempty string slice"`

	Nested    reflectNested  `json:"nested" jsonschema:"A nested struct"`
	NestedPtr *reflectNested `json:"nested_ptr,omitempty" jsonschema:"A pointer to a nested struct"`
	PtrString *string        `json:"ptr_string" jsonschema:"A pointer to a string"`

	Map map[string]string `json:"map" jsonschema:"A map of strings"`

	// `json:"-"` omits the property entirely.
	Ignored string `json:"-"`
	// An unexported field is omitted too — where a private Rust field is not,
	// since `schemars` reflects the type rather than its visibility.
	unexported string //nolint:unused // present so its absence is pinned
}

type reflectPropertyVector struct {
	Name string `json:"name"`
	// Whether the property is in the document's `required` list. Kept beside
	// the property because "optional" is a property of the parent, not of the
	// field's own schema — which is the whole point of the `Option<T>` note.
	Required bool            `json:"required"`
	Schema   json.RawMessage `json:"schema"`
}

type jsonschemaReflectVectors struct {
	Comment []string `json:"_comment"`
	// The whole document, exactly as `tools/list` would carry it.
	Schema json.RawMessage `json:"schema"`
	// The document's `required` list, in Go's order (declaration order).
	Required []string `json:"required"`
	// One entry per property, so a Rust assertion can name the shape it is
	// about rather than indexing into the document.
	Properties []reflectPropertyVector `json:"properties"`
}

func TestJSONSchemaReflectVectors(t *testing.T) {
	// Exactly what `mcp.AddTool` does with the `In` type parameter: no options,
	// no post-processing. `Resolve` is not applied, because it only compiles
	// the schema for validation and does not change the bytes `tools/list`
	// carries — `github_vectors.json` holds the resolved-and-served form for
	// twenty real tools and agrees with this one.
	schema, err := jsonschema.For[reflectReference](nil)
	if err != nil {
		t.Fatalf("reflecting the reference struct: %v", err)
	}

	document, err := json.Marshal(schema)
	if err != nil {
		t.Fatalf("encoding the reference schema: %v", err)
	}

	want := jsonschemaReflectVectors{
		Comment: []string{
			"The reflector divergence map: google/jsonschema-go's output for every Go shape",
			"a ported integration params struct can contain. Generated from Go, then frozen.",
			"Read by desktop/parity/jsonschema_reflect_parity_test.go (Go) and by",
			"desktop/src-tauri/src/claude/schema_vectors.rs (Rust), which declares the",
			"corresponding Rust shapes and asserts per property either equality or the exact",
			"divergence — with the guidance a port needs written at its site.",
			"Two rules every port depends on: a jsonschema tag is the description verbatim",
			"(the 'required,' prefix included, and it is not a directive), and a field is",
			"optional only when it carries omitempty or omitzero.",
			"Regenerate with: go test ./desktop/parity/ -run TestJSONSchemaReflectVectors -update-jsonschema-reflect-vectors",
		},
		Schema:   document,
		Required: slices.Clone(schema.Required),
	}

	// `PropertyOrder` is declaration order, which is also the order the custom
	// marshaler renders `properties` in — so iterating it keeps the vectors in
	// the order a reader of the Go struct expects.
	for _, name := range schema.PropertyOrder {
		encoded, err := json.Marshal(schema.Properties[name])
		if err != nil {
			t.Fatalf("encoding property %s: %v", name, err)
		}
		want.Properties = append(want.Properties, reflectPropertyVector{
			Name:     name,
			Required: slices.Contains(schema.Required, name),
			Schema:   encoded,
		})
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateJSONSchemaReflectVectors {
		if err := os.WriteFile(jsonschemaReflectVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", jsonschemaReflectVectorsFile, err)
		}
		t.Logf("wrote %s", jsonschemaReflectVectorsFile)
		return
	}

	frozen, err := os.ReadFile(jsonschemaReflectVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-jsonschema-reflect-vectors): %v",
			jsonschemaReflectVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this google/jsonschema-go produces different results.\n"+
			"Regenerate with -update-jsonschema-reflect-vectors and read what moved — "+
			"the Rust half in claude/schema_vectors.rs reads the same file, and a shape "+
			"that starts or stops diverging changes what every ported tool must be "+
			"written as.",
			jsonschemaReflectVectorsFile)
	}
}
