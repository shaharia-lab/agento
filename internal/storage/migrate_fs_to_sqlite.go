package storage

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"time"

	"gopkg.in/yaml.v3"

	"github.com/shaharia-lab/agento/internal/config"
)

const settingsFileName = "settings.json"

// MigrateFromFS reads existing filesystem-based data and imports it into the
// SQLite database. It runs inside a single transaction for atomicity.
// The migration is idempotent — it skips records that already exist by primary key.
func MigrateFromFS(db *sql.DB, dataDir string, logger *slog.Logger) error {
	agentsDir := filepath.Join(dataDir, "agents")
	chatsDir := filepath.Join(dataDir, "chats")
	integrationsDir := filepath.Join(dataDir, "integrations")
	settingsFile := filepath.Join(dataDir, settingsFileName)

	ctx := context.Background()
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin migration transaction: %w", err)
	}
	defer func() {
		alreadyDone := "sql: transaction has already been committed or rolled back"
		if rbErr := tx.Rollback(); rbErr != nil && rbErr.Error() != alreadyDone {
			logger.Error("failed to rollback migration transaction", "error", rbErr)
		}
	}()

	if err := migrateAgents(ctx, tx, agentsDir, logger); err != nil {
		return fmt.Errorf("migrating agents: %w", err)
	}

	if err := migrateChatSessions(ctx, tx, chatsDir, logger); err != nil {
		return fmt.Errorf("migrating chat sessions: %w", err)
	}

	if err := migrateIntegrations(ctx, tx, integrationsDir, logger); err != nil {
		return fmt.Errorf("migrating integrations: %w", err)
	}

	if err := migrateSettings(ctx, tx, settingsFile, logger); err != nil {
		return fmt.Errorf("migrating settings: %w", err)
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("commit migration transaction: %w", err)
	}

	logger.Info("filesystem-to-SQLite migration completed successfully")

	// Clean up old filesystem data after successful migration.
	cleanupFSData(dataDir, logger)

	return nil
}

// HasFSData returns true if there is any filesystem-based data to migrate.
func HasFSData(dataDir string) bool {
	for _, sub := range []string{"agents", "chats", "integrations"} {
		dir := filepath.Join(dataDir, sub)
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}
		if len(entries) > 0 {
			return true
		}
	}
	// Also check settings.json
	if _, err := os.Stat(filepath.Join(dataDir, settingsFileName)); err == nil {
		return true
	}
	return false
}

func migrateAgents(ctx context.Context, tx *sql.Tx, dir string, logger *slog.Logger) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}

	count := 0
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".yaml") {
			continue
		}

		inserted, err := migrateOneAgent(ctx, tx, dir, entry.Name(), logger)
		if err != nil {
			return err
		}
		if inserted {
			count++
		}
	}
	logger.Info("migrated agents", "count", count)
	return nil
}

func migrateOneAgent(ctx context.Context, tx *sql.Tx, dir, fileName string, logger *slog.Logger) (bool, error) {
	data, err := os.ReadFile(filepath.Join(dir, fileName)) //nolint:gosec
	if err != nil {
		logger.Warn("skipping agent file", "file", fileName, "error", err)
		return false, nil
	}

	var cfg config.AgentConfig
	if unmarshalErr := yaml.Unmarshal(data, &cfg); unmarshalErr != nil {
		logger.Warn("skipping malformed agent", "file", fileName, "error", unmarshalErr)
		return false, nil
	}

	applyAgentMigrationDefaults(&cfg, fileName)

	capsJSON, err := json.Marshal(cfg.Capabilities)
	if err != nil {
		logger.Warn("skipping agent with bad capabilities", "slug", cfg.Slug, "error", err)
		return false, nil
	}

	now := time.Now().UTC()
	_, err = tx.ExecContext(ctx, `
		INSERT OR IGNORE INTO agents
			(slug, name, description, model, thinking, permission_mode,
			 system_prompt, capabilities, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		cfg.Slug, cfg.Name, cfg.Description, cfg.Model,
		cfg.Thinking, cfg.PermissionMode, cfg.SystemPrompt,
		string(capsJSON), now, now,
	)
	if err != nil {
		return false, fmt.Errorf("inserting agent %q: %w", cfg.Slug, err)
	}
	return true, nil
}

func applyAgentMigrationDefaults(cfg *config.AgentConfig, fileName string) {
	if cfg.Slug == "" {
		cfg.Slug = strings.TrimSuffix(fileName, ".yaml")
	}
	if cfg.Model == "" {
		cfg.Model = "claude-sonnet-4-6"
	}
	if cfg.Thinking == "" {
		cfg.Thinking = "adaptive"
	}
}

// chatFileRecord is the unified struct for session and message records in JSONL chat files.
type chatFileRecord struct {
	Type                     string          `json:"type"`
	ID                       string          `json:"id"`
	Title                    string          `json:"title"`
	AgentSlug                string          `json:"agent_slug"`
	SDKSession               string          `json:"sdk_session_id"`
	WorkingDir               string          `json:"working_directory"`
	Model                    string          `json:"model"`
	SettingsProfileID        string          `json:"settings_profile_id"`
	CreatedAt                time.Time       `json:"created_at"`
	UpdatedAt                time.Time       `json:"updated_at"`
	TotalInputTokens         int             `json:"total_input_tokens"`
	TotalOutputTokens        int             `json:"total_output_tokens"`
	TotalCacheCreationTokens int             `json:"total_cache_creation_tokens"`
	TotalCacheReadTokens     int             `json:"total_cache_read_tokens"`
	Role                     string          `json:"role"`
	Content                  string          `json:"content"`
	Timestamp                time.Time       `json:"timestamp"`
	Blocks                   json.RawMessage `json:"blocks"`
}

type chatMigrationCounts struct {
	sessions int
	messages int
}

func migrateChatSessions(ctx context.Context, tx *sql.Tx, dir string, logger *slog.Logger) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}

	counts := chatMigrationCounts{}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".jsonl") {
			continue
		}

		if err := migrateOneChatFile(ctx, tx, dir, entry.Name(), &counts, logger); err != nil {
			return err
		}
	}
	logger.Info("migrated chat sessions", "sessions", counts.sessions, "messages", counts.messages)
	return nil
}

func migrateOneChatFile(
	ctx context.Context, tx *sql.Tx,
	dir, fileName string,
	counts *chatMigrationCounts, logger *slog.Logger,
) error {
	f, err := os.Open(filepath.Join(dir, fileName)) //nolint:gosec
	if err != nil {
		logger.Warn("skipping chat file", "file", fileName, "error", err)
		return nil
	}
	defer func() {
		if cerr := f.Close(); cerr != nil {
			logger.Warn("failed to close chat file", "file", fileName, "error", cerr)
		}
	}()

	records := parseChatRecords(f)
	if len(records) == 0 {
		return nil
	}

	return importChatRecords(ctx, tx, records, counts)
}

// parseChatRecords reads all valid JSONL records from the chat file.
// Returns nil if the first record is not a session header.
func parseChatRecords(f *os.File) []chatFileRecord {
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)

	var records []chatFileRecord
	for scanner.Scan() {
		var rec chatFileRecord
		if json.Unmarshal(scanner.Bytes(), &rec) != nil {
			continue
		}
		if len(records) == 0 && rec.Type != "session" {
			return nil
		}
		records = append(records, rec)
	}
	return records
}

// importChatRecords inserts the parsed session and message records into the database.
func importChatRecords(ctx context.Context, tx *sql.Tx, records []chatFileRecord, counts *chatMigrationCounts) error {
	header := &records[0]
	if err := insertChatSession(ctx, tx, header); err != nil {
		return err
	}
	counts.sessions++

	for i := 1; i < len(records); i++ {
		rec := &records[i]
		if rec.Type != "message" {
			continue
		}
		if err := insertChatMessage(ctx, tx, header.ID, rec); err != nil {
			return err
		}
		counts.messages++
	}
	return nil
}

func insertChatSession(ctx context.Context, tx *sql.Tx, rec *chatFileRecord) error {
	_, err := tx.ExecContext(ctx, `
		INSERT OR IGNORE INTO chat_sessions
			(id, title, agent_slug, sdk_session_id, working_directory, model,
			 settings_profile_id, total_input_tokens, total_output_tokens,
			 total_cache_creation_tokens, total_cache_read_tokens,
			 created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		rec.ID, rec.Title, rec.AgentSlug, rec.SDKSession, rec.WorkingDir,
		rec.Model, rec.SettingsProfileID,
		rec.TotalInputTokens, rec.TotalOutputTokens,
		rec.TotalCacheCreationTokens, rec.TotalCacheReadTokens,
		rec.CreatedAt, rec.UpdatedAt,
	)
	if err != nil {
		return fmt.Errorf("inserting session %q: %w", rec.ID, err)
	}
	return nil
}

func insertChatMessage(ctx context.Context, tx *sql.Tx, sessionID string, rec *chatFileRecord) error {
	blocksJSON := "[]"
	if len(rec.Blocks) > 0 {
		blocksJSON = string(rec.Blocks)
	}

	_, err := tx.ExecContext(ctx, `
		INSERT INTO chat_messages (session_id, role, content, blocks, timestamp)
		VALUES (?, ?, ?, ?, ?)`,
		sessionID, rec.Role, rec.Content, blocksJSON, rec.Timestamp,
	)
	if err != nil {
		return fmt.Errorf("inserting message for session %q: %w", sessionID, err)
	}
	return nil
}

func migrateIntegrations(ctx context.Context, tx *sql.Tx, dir string, logger *slog.Logger) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}

	count := 0
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}

		inserted, err := migrateOneIntegration(ctx, tx, dir, entry.Name(), logger)
		if err != nil {
			return err
		}
		if inserted {
			count++
		}
	}
	logger.Info("migrated integrations", "count", count)
	return nil
}

func migrateOneIntegration(ctx context.Context, tx *sql.Tx, dir, fileName string, logger *slog.Logger) (bool, error) {
	data, err := os.ReadFile(filepath.Join(dir, fileName)) //nolint:gosec
	if err != nil {
		logger.Warn("skipping integration file", "file", fileName, "error", err)
		return false, nil
	}

	var cfg config.IntegrationConfig
	if unmarshalErr := json.Unmarshal(data, &cfg); unmarshalErr != nil {
		logger.Warn("skipping malformed integration", "file", fileName, "error", unmarshalErr)
		return false, nil
	}

	credJSON, servJSON, authJSON, err := marshalIntegrationFields(&cfg, logger)
	if err != nil {
		return false, nil
	}

	enabled := 0
	if cfg.Enabled {
		enabled = 1
	}

	_, err = tx.ExecContext(ctx, `
		INSERT OR IGNORE INTO integrations
			(id, name, type, enabled, credentials, auth, services, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		cfg.ID, cfg.Name, cfg.Type, enabled,
		string(credJSON), authJSON, string(servJSON),
		cfg.CreatedAt, cfg.UpdatedAt,
	)
	if err != nil {
		return false, fmt.Errorf("inserting integration %q: %w", cfg.ID, err)
	}
	return true, nil
}

func marshalIntegrationFields(cfg *config.IntegrationConfig, logger *slog.Logger) ([]byte, []byte, *string, error) {
	credJSON := []byte(cfg.Credentials)
	if len(credJSON) == 0 {
		credJSON = []byte("{}")
	}
	servJSON, err := json.Marshal(cfg.Services)
	if err != nil {
		logger.Warn("skipping integration with bad services", "id", cfg.ID, "error", err)
		return nil, nil, nil, err
	}

	var authJSON *string
	if cfg.IsAuthenticated() {
		s := string(cfg.Auth)
		authJSON = &s
	}

	return credJSON, servJSON, authJSON, nil
}

func migrateSettings(ctx context.Context, tx *sql.Tx, settingsFile string, logger *slog.Logger) error {
	data, err := os.ReadFile(settingsFile) //nolint:gosec
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading settings file: %w", err)
	}

	var settings config.UserSettings
	if unmarshalErr := json.Unmarshal(data, &settings); unmarshalErr != nil {
		logger.Warn("skipping malformed settings file", "error", unmarshalErr)
		return nil
	}

	onboarding := 0
	if settings.OnboardingComplete {
		onboarding = 1
	}
	darkMode := 0
	if settings.AppearanceDarkMode {
		darkMode = 1
	}

	_, err = tx.ExecContext(ctx, `
		INSERT OR IGNORE INTO user_settings
			(id, default_working_dir, default_model, onboarding_complete,
			 appearance_dark_mode, appearance_font_size, appearance_font_family)
		VALUES (1, ?, ?, ?, ?, ?, ?)`,
		settings.DefaultWorkingDir, settings.DefaultModel, onboarding,
		darkMode, settings.AppearanceFontSize, settings.AppearanceFontFamily,
	)
	if err != nil {
		return fmt.Errorf("inserting settings: %w", err)
	}
	logger.Info("migrated user settings")
	return nil
}

// cleanupFSData removes the old filesystem-based data directories and settings
// file after a successful migration to SQLite. Each removal is logged
// individually; failures are warned but do not prevent the app from starting.
func cleanupFSData(dataDir string, logger *slog.Logger) {
	logger.Info("cleaning up old filesystem storage after successful migration")

	dirs := []string{
		filepath.Join(dataDir, "agents"),
		filepath.Join(dataDir, "chats"),
		filepath.Join(dataDir, "integrations"),
	}
	for _, dir := range dirs {
		if _, err := os.Stat(dir); os.IsNotExist(err) {
			continue
		}
		if err := os.RemoveAll(dir); err != nil {
			logger.Warn("failed to remove legacy directory", "path", dir, "error", err)
		} else {
			logger.Info("removed legacy directory", "path", dir)
		}
	}

	settingsFile := filepath.Join(dataDir, settingsFileName)
	if _, err := os.Stat(settingsFile); err == nil {
		if err := os.Remove(settingsFile); err != nil {
			logger.Warn("failed to remove legacy settings file", "path", settingsFile, "error", err)
		} else {
			logger.Info("removed legacy settings file", "path", settingsFile)
		}
	}

	logger.Info("filesystem cleanup complete — all data is now in agento.db")
}
