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

// MigrateFromFS reads existing filesystem-based data and imports it into the
// SQLite database. It runs inside a single transaction for atomicity.
// The migration is idempotent — it skips records that already exist by primary key.
func MigrateFromFS(db *sql.DB, dataDir string, logger *slog.Logger) error {
	agentsDir := filepath.Join(dataDir, "agents")
	chatsDir := filepath.Join(dataDir, "chats")
	integrationsDir := filepath.Join(dataDir, "integrations")
	settingsFile := filepath.Join(dataDir, "settings.json")

	ctx := context.Background()
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin migration transaction: %w", err)
	}
	defer func() { _ = tx.Rollback() }()

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
	if _, err := os.Stat(filepath.Join(dataDir, "settings.json")); err == nil {
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

		data, err := os.ReadFile(filepath.Join(dir, entry.Name())) //nolint:gosec
		if err != nil {
			logger.Warn("skipping agent file", "file", entry.Name(), "error", err)
			continue
		}

		var cfg config.AgentConfig
		if err := yaml.Unmarshal(data, &cfg); err != nil {
			logger.Warn("skipping malformed agent", "file", entry.Name(), "error", err)
			continue
		}
		if cfg.Slug == "" {
			cfg.Slug = strings.TrimSuffix(entry.Name(), ".yaml")
		}
		if cfg.Model == "" {
			cfg.Model = "claude-sonnet-4-6"
		}
		if cfg.Thinking == "" {
			cfg.Thinking = "adaptive"
		}

		capsJSON, err := json.Marshal(cfg.Capabilities)
		if err != nil {
			logger.Warn("skipping agent with bad capabilities", "slug", cfg.Slug, "error", err)
			continue
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
			return fmt.Errorf("inserting agent %q: %w", cfg.Slug, err)
		}
		count++
	}
	logger.Info("migrated agents", "count", count)
	return nil
}

func migrateChatSessions(ctx context.Context, tx *sql.Tx, dir string, logger *slog.Logger) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}

	sessionCount := 0
	messageCount := 0
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".jsonl") {
			continue
		}

		f, err := os.Open(filepath.Join(dir, entry.Name())) //nolint:gosec
		if err != nil {
			logger.Warn("skipping chat file", "file", entry.Name(), "error", err)
			continue
		}

		scanner := bufio.NewScanner(f)
		scanner.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)
		first := true
		var sessionID string

		for scanner.Scan() {
			var rec struct {
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
			if err := json.Unmarshal(scanner.Bytes(), &rec); err != nil {
				continue
			}

			if first {
				first = false
				if rec.Type != "session" {
					break
				}
				sessionID = rec.ID

				_, err = tx.ExecContext(ctx, `
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
					_ = f.Close()
					return fmt.Errorf("inserting session %q: %w", rec.ID, err)
				}
				sessionCount++
				continue
			}

			if rec.Type == "message" && sessionID != "" {
				blocksJSON := "[]"
				if len(rec.Blocks) > 0 {
					blocksJSON = string(rec.Blocks)
				}

				_, err = tx.ExecContext(ctx, `
					INSERT INTO chat_messages (session_id, role, content, blocks, timestamp)
					VALUES (?, ?, ?, ?, ?)`,
					sessionID, rec.Role, rec.Content, blocksJSON, rec.Timestamp,
				)
				if err != nil {
					_ = f.Close()
					return fmt.Errorf("inserting message for session %q: %w", sessionID, err)
				}
				messageCount++
			}
		}
		_ = f.Close()
	}
	logger.Info("migrated chat sessions", "sessions", sessionCount, "messages", messageCount)
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

		data, err := os.ReadFile(filepath.Join(dir, entry.Name())) //nolint:gosec
		if err != nil {
			logger.Warn("skipping integration file", "file", entry.Name(), "error", err)
			continue
		}

		var cfg config.IntegrationConfig
		if err := json.Unmarshal(data, &cfg); err != nil {
			logger.Warn("skipping malformed integration", "file", entry.Name(), "error", err)
			continue
		}

		credJSON, _ := json.Marshal(cfg.Credentials)
		servJSON, _ := json.Marshal(cfg.Services)

		var authJSON *string
		if cfg.Auth != nil {
			b, err := json.Marshal(cfg.Auth)
			if err == nil {
				s := string(b)
				authJSON = &s
			}
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
			return fmt.Errorf("inserting integration %q: %w", cfg.ID, err)
		}
		count++
	}
	logger.Info("migrated integrations", "count", count)
	return nil
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
	if err := json.Unmarshal(data, &settings); err != nil {
		logger.Warn("skipping malformed settings file", "error", err)
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
