package storage

import (
	"context"
	"database/sql"
	"fmt"
	"log/slog"
)

// ApplyMigrationsUpTo opens dbPath and applies every migration with
// version <= v, recording them in schema_migrations exactly as the runner
// does. It exists so upgrade-path tests can build a genuine old database
// instead of mutilating a current one back into shape: undoing migrations by
// hand needs an inverse statement for every migration above the target, and a
// forgotten one leaves MAX(version) high, which makes the runner skip the
// migration under test and the test pass while asserting nothing.
//
// It shares openSQLiteDB and applyMigrations with NewSQLiteDB rather than
// re-implementing them, so the database it builds is the one the real open
// path would have built at that version. Callers own the returned handle and
// must close it before re-opening the file through NewSQLiteDB.
//
// A v at or above the newest declared migration is rejected rather than
// silently returning a head database. That is the same failure the inverse-SQL
// approach had — the fixture is not actually old, the migration under test has
// already run, and the test passes while asserting nothing — and moving the
// bound into one place is what stops it coming back through a stale literal in
// a caller. Head is derived here rather than in production code; this file is
// compiled only into the test binary.
func ApplyMigrationsUpTo(dbPath string, v int) (*sql.DB, error) {
	ctx := context.Background()
	logger := slog.Default()

	if head := latestMigrationVersion(); v >= head {
		return nil, fmt.Errorf("ApplyMigrationsUpTo(%d): newest migration is %d, so this builds a head database, not an old one", v, head)
	}

	db, err := openSQLiteDB(ctx, dbPath, logger)
	if err != nil {
		return nil, err
	}

	if _, err := applyMigrations(ctx, db, logger, v); err != nil {
		if cerr := db.Close(); cerr != nil {
			logger.Warn("failed to close database after migration error", "error", cerr)
		}
		return nil, fmt.Errorf("applying migrations up to %d: %w", v, err)
	}

	return db, nil
}

// latestMigrationVersion is the highest version in the migrations slice. It
// takes the maximum rather than the last element so it does not depend on the
// slice being sorted, matching the bound check in applyMigrations.
func latestMigrationVersion() int {
	head := 0
	for _, m := range migrations {
		if m.version > head {
			head = m.version
		}
	}
	return head
}
