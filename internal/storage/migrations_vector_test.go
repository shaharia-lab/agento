package storage

import (
	"encoding/json"
	"flag"
	"os"
	"path/filepath"
	"testing"
)

// The schema, exported for the Rust port — and the reason it is a file rather
// than a transcription.
//
// The desktop app (desktop/, `desktop` branch) is porting this server to Rust,
// and phase 3 gives Rust the write path. Writing means agreeing about the
// schema, and 27 migrations of hand-copied DDL is exactly the kind of thing
// that agrees on every table anyone happens to check and differs on one column
// default nobody does. So Rust does not transcribe them: it embeds this file
// (`include_str!` in desktop/src-tauri/src/native/migrate.rs), which is
// generated from the slice below and asserted against it here.
//
// That makes drift a **test failure in Go**, at the moment a migration is
// added, rather than a divergence discovered later from the Rust side. Adding
// migration 28 and forgetting to regenerate fails this test; the existing
// convention of also bumping the hardcoded version in sqlite_test.go is
// unchanged.
//
// Regenerate with:
//
//	go test ./internal/storage/ -run TestMigrationVectors -update-migration-vectors
//
// The file outlives this package. When the Go server is deleted it is the only
// record of what the schema was, which is the same reason desktop/parity/ keeps
// its other vectors.
const migrationVectorFile = "../../desktop/parity/migrations_vectors.json"

var updateMigrationVectors = flag.Bool("update-migration-vectors", false,
	"rewrite "+migrationVectorFile+" from the migrations slice")

// migrationVector is one migration as both languages see it.
type migrationVector struct {
	Version int    `json:"version"`
	SQL     string `json:"sql"`
}

type migrationVectorFileContents struct {
	Comment    []string          `json:"_comment"`
	Migrations []migrationVector `json:"migrations"`
}

func TestMigrationVectors(t *testing.T) {
	want := migrationVectorFileContents{
		Comment: []string{
			"The Agento SQLite schema, migration by migration, exactly as",
			"internal/storage applies it.",
			"",
			"Generated from Go (go test ./internal/storage/ -update-migration-vectors)",
			"and asserted by both languages: internal/storage/migrations_vector_test.go",
			"regenerates and checks it, and the Rust port embeds it verbatim at",
			"desktop/src-tauri/src/native/migrate.rs rather than transcribing 27",
			"migrations by hand.",
			"",
			"'sql' is the migration body byte for byte, including the leading",
			"newline several of them carry. Do not reformat it: Rust compares the",
			"bytes, and prettifying this file is a divergence.",
		},
		Migrations: make([]migrationVector, 0, len(migrations)),
	}
	for _, m := range migrations {
		want.Migrations = append(want.Migrations, migrationVector{Version: m.version, SQL: m.sql})
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding migration vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateMigrationVectors {
		if mkErr := os.MkdirAll(filepath.Dir(migrationVectorFile), 0o750); mkErr != nil {
			t.Fatalf("creating vector directory: %v", mkErr)
		}
		if writeErr := os.WriteFile(migrationVectorFile, encoded, 0o600); writeErr != nil {
			t.Fatalf("writing %s: %v", migrationVectorFile, writeErr)
		}
		t.Logf("wrote %s (%d migrations)", migrationVectorFile, len(want.Migrations))
		return
	}

	onDisk, err := os.ReadFile(migrationVectorFile) //nolint:gosec // fixed test path
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-migration-vectors): %v", migrationVectorFile, err)
	}

	if string(onDisk) != string(encoded) {
		t.Errorf("%s is stale — regenerate with:\n"+
			"\tgo test ./internal/storage/ -run TestMigrationVectors -update-migration-vectors\n"+
			"The Rust port embeds this file, so a stale copy means the two "+
			"implementations disagree about the schema.", migrationVectorFile)
	}
}

// The versions must be contiguous and start at 1, because both languages
// decide what to apply by comparing against MAX(version) in schema_migrations.
// A gap would make everything after it unreachable on a database that stopped
// in the gap, and a duplicate would make the second one silently never run.
func TestMigrationVersionsAreContiguousFromOne(t *testing.T) {
	for i, m := range migrations {
		if want := i + 1; m.version != want {
			t.Fatalf("migrations[%d] has version %d, want %d — versions must be contiguous from 1", i, m.version, want)
		}
		if m.sql == "" {
			t.Errorf("migration %d has empty SQL", m.version)
		}
	}
}
