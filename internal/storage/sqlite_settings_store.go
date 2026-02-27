package storage

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/shaharia-lab/agento/internal/config"
)

// SQLiteSettingsStore implements config.SettingsStore backed by a SQLite database.
type SQLiteSettingsStore struct {
	db *sql.DB
}

// NewSQLiteSettingsStore returns a new SQLiteSettingsStore.
func NewSQLiteSettingsStore(db *sql.DB) *SQLiteSettingsStore {
	return &SQLiteSettingsStore{db: db}
}

// Load returns the persisted user settings. If no row exists yet, it inserts
// the default row and returns defaults.
func (s *SQLiteSettingsStore) Load() (config.UserSettings, error) {
	var us config.UserSettings
	var darkMode, onboarding int

	ctx := context.Background()
	err := s.db.QueryRowContext(ctx, `
		SELECT default_working_dir, default_model, onboarding_complete,
		       appearance_dark_mode, appearance_font_size, appearance_font_family
		FROM user_settings WHERE id = 1`).Scan(
		&us.DefaultWorkingDir, &us.DefaultModel, &onboarding,
		&darkMode, &us.AppearanceFontSize, &us.AppearanceFontFamily,
	)
	if err == sql.ErrNoRows {
		// Insert default row.
		us = config.UserSettings{
			DefaultWorkingDir: "/tmp/agento/work",
		}
		if err := s.Save(us); err != nil {
			return us, fmt.Errorf("initializing default settings: %w", err)
		}
		return us, nil
	}
	if err != nil {
		return us, fmt.Errorf("loading settings: %w", err)
	}
	us.OnboardingComplete = onboarding != 0
	us.AppearanceDarkMode = darkMode != 0
	return us, nil
}

// Save persists the user settings (single row, id=1).
func (s *SQLiteSettingsStore) Save(settings config.UserSettings) error {
	onboarding := 0
	if settings.OnboardingComplete {
		onboarding = 1
	}
	darkMode := 0
	if settings.AppearanceDarkMode {
		darkMode = 1
	}

	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO user_settings
			(id, default_working_dir, default_model, onboarding_complete,
			 appearance_dark_mode, appearance_font_size, appearance_font_family)
		VALUES (1, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET
			default_working_dir = excluded.default_working_dir,
			default_model = excluded.default_model,
			onboarding_complete = excluded.onboarding_complete,
			appearance_dark_mode = excluded.appearance_dark_mode,
			appearance_font_size = excluded.appearance_font_size,
			appearance_font_family = excluded.appearance_font_family`,
		settings.DefaultWorkingDir, settings.DefaultModel, onboarding,
		darkMode, settings.AppearanceFontSize, settings.AppearanceFontFamily,
	)
	if err != nil {
		return fmt.Errorf("saving settings: %w", err)
	}
	return nil
}
