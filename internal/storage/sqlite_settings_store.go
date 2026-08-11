package storage

import (
	"context"
	"database/sql"
	"encoding/json"
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

// Load returns the persisted user settings. If no row exists yet, it returns
// zero-value settings so the SettingsManager can fill in defaults.
func (s *SQLiteSettingsStore) Load() (config.UserSettings, error) {
	var us config.UserSettings
	var darkMode, onboarding int
	var hiddenProjects string

	ctx := context.Background()
	err := s.db.QueryRowContext(ctx, `
		SELECT default_working_dir, default_model, onboarding_complete,
		       appearance_dark_mode, appearance_font_size, appearance_font_family,
		       notification_settings, event_bus_worker_pool_size, public_url,
		       hidden_projects, idle_gap_threshold_minutes
		FROM user_settings WHERE id = 1`).Scan(
		&us.DefaultWorkingDir, &us.DefaultModel, &onboarding,
		&darkMode, &us.AppearanceFontSize, &us.AppearanceFontFamily,
		&us.NotificationSettings, &us.EventBusWorkerPoolSize,
		&us.PublicURL, &hiddenProjects, &us.IdleGapThresholdMinutes,
	)
	if err == sql.ErrNoRows {
		// Return zero-value settings; SettingsManager fills defaults.
		return config.UserSettings{}, nil
	}
	if err != nil {
		return us, fmt.Errorf("loading settings: %w", err)
	}
	us.OnboardingComplete = onboarding != 0
	us.AppearanceDarkMode = darkMode != 0
	us.HiddenProjects = decodeHiddenProjects(hiddenProjects)
	return us, nil
}

// decodeHiddenProjects reads the stored JSON array. Unparseable content yields
// no hidden projects rather than an error: the failure mode of showing a
// project the user hid is a visible one they can fix, while failing the whole
// settings load over it would take the app down with it.
func decodeHiddenProjects(raw string) []string {
	if raw == "" {
		return nil
	}
	var projects []string
	if err := json.Unmarshal([]byte(raw), &projects); err != nil {
		return nil
	}
	return projects
}

// encodeHiddenProjects serializes the list for storage, degrading to an empty
// array so the NOT NULL column always holds valid JSON.
func encodeHiddenProjects(projects []string) string {
	if len(projects) == 0 {
		return "[]"
	}
	encoded, err := json.Marshal(projects)
	if err != nil {
		return "[]"
	}
	return string(encoded)
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

	notificationSettings := settings.NotificationSettings
	if notificationSettings == "" {
		notificationSettings = "{}"
	}

	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO user_settings
			(id, default_working_dir, default_model, onboarding_complete,
			 appearance_dark_mode, appearance_font_size, appearance_font_family,
			 notification_settings, event_bus_worker_pool_size, public_url,
			 hidden_projects, idle_gap_threshold_minutes)
		VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET
			default_working_dir = excluded.default_working_dir,
			default_model = excluded.default_model,
			onboarding_complete = excluded.onboarding_complete,
			appearance_dark_mode = excluded.appearance_dark_mode,
			appearance_font_size = excluded.appearance_font_size,
			appearance_font_family = excluded.appearance_font_family,
			notification_settings = excluded.notification_settings,
			event_bus_worker_pool_size = excluded.event_bus_worker_pool_size,
			public_url = excluded.public_url,
			hidden_projects = excluded.hidden_projects,
			idle_gap_threshold_minutes = excluded.idle_gap_threshold_minutes`,
		settings.DefaultWorkingDir, settings.DefaultModel, onboarding,
		darkMode, settings.AppearanceFontSize, settings.AppearanceFontFamily,
		notificationSettings, settings.EventBusWorkerPoolSize,
		settings.PublicURL, encodeHiddenProjects(settings.HiddenProjects),
		settings.IdleGapThresholdMinutes,
	)
	if err != nil {
		return fmt.Errorf("saving settings: %w", err)
	}
	return nil
}
