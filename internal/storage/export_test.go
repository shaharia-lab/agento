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
func ApplyMigrationsUpTo(dbPath string, v int) (*sql.DB, error) {
	ctx := context.Background()
	logger := slog.Default()

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
