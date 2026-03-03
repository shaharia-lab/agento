package telemetry

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sync"
	"time"
)

// monitoringConfigStore is the JSON-serialisable representation of MonitoringConfig.
// It is used only for persistence; the runtime type remains MonitoringConfig.
type monitoringConfigStore struct {
	Enabled                bool              `json:"enabled"`
	MetricsExporter        string            `json:"metrics_exporter"`
	LogsExporter           string            `json:"logs_exporter"`
	OTLPEndpoint           string            `json:"otlp_endpoint"`
	OTLPHeaders            map[string]string `json:"otlp_headers,omitempty"`
	OTLPInsecure           bool              `json:"otlp_insecure"`
	MetricExportIntervalMs int64             `json:"metric_export_interval_ms"`
}

func configToStore(cfg MonitoringConfig) monitoringConfigStore {
	return monitoringConfigStore{
		Enabled:                cfg.Enabled,
		MetricsExporter:        string(cfg.MetricsExporter),
		LogsExporter:           string(cfg.LogsExporter),
		OTLPEndpoint:           cfg.OTLPEndpoint,
		OTLPHeaders:            cfg.OTLPHeaders,
		OTLPInsecure:           cfg.OTLPInsecure,
		MetricExportIntervalMs: cfg.MetricExportInterval.Milliseconds(),
	}
}

func storeToConfig(s monitoringConfigStore) MonitoringConfig {
	interval := time.Duration(s.MetricExportIntervalMs) * time.Millisecond
	if interval <= 0 {
		interval = 60 * time.Second
	}
	return MonitoringConfig{
		Enabled:              s.Enabled,
		MetricsExporter:      MetricsExporter(s.MetricsExporter),
		LogsExporter:         LogsExporter(s.LogsExporter),
		OTLPEndpoint:         s.OTLPEndpoint,
		OTLPHeaders:          s.OTLPHeaders,
		OTLPInsecure:         s.OTLPInsecure,
		MetricExportInterval: interval,
	}
}

// envVarChecks maps monitoring config field names to the environment variable that pins them.
var envVarChecks = map[string]string{ //nolint:gochecknoglobals
	"enabled":                "OTEL_SDK_DISABLED",
	"otlp_endpoint":          "OTEL_EXPORTER_OTLP_ENDPOINT",
	"otlp_headers":           "OTEL_EXPORTER_OTLP_HEADERS",
	"otlp_insecure":          "OTEL_EXPORTER_OTLP_INSECURE",
	"metrics_exporter":       "OTEL_METRICS_EXPORTER",
	"logs_exporter":          "OTEL_LOGS_EXPORTER",
	"metric_export_interval": "OTEL_METRIC_EXPORT_INTERVAL",
}

// MonitoringManager manages the OTel providers lifecycle, persists config to
// disk, and supports hot-reload without a server restart.
type MonitoringManager struct {
	mu         sync.RWMutex
	configPath string
	current    MonitoringConfig
	providers  *Providers
	envCfg     MonitoringConfig // config snapshot from env vars at startup
}

// NewMonitoringManager creates a MonitoringManager.
//   - dataDir: application data directory (e.g. ~/.agento)
//   - providers: currently-running OTel providers (from startup Init)
//   - envCfg: config loaded from env vars at startup (used to detect locks)
func NewMonitoringManager(dataDir string, providers *Providers, envCfg MonitoringConfig) *MonitoringManager {
	return &MonitoringManager{
		configPath: filepath.Join(dataDir, "monitoring.json"),
		providers:  providers,
		envCfg:     envCfg,
		current:    envCfg,
	}
}

// Load reads persisted config from disk. If any OTEL_* env vars are set the
// env config takes precedence and the file is ignored.
func (m *MonitoringManager) Load() error {
	m.mu.Lock()
	defer m.mu.Unlock()

	// Env vars always win; do not apply stored config.
	if m.isEnvLockedUnsafe() {
		return nil
	}

	data, err := os.ReadFile(m.configPath)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("reading monitoring config: %w", err)
	}

	var stored monitoringConfigStore
	if err := json.Unmarshal(data, &stored); err != nil {
		return fmt.Errorf("parsing monitoring config: %w", err)
	}

	cfg := storeToConfig(stored)
	if err := m.reload(context.Background(), cfg); err != nil {
		return fmt.Errorf("applying persisted monitoring config: %w", err)
	}
	m.current = cfg
	return nil
}

// Get returns a copy of the current config.
func (m *MonitoringManager) Get() MonitoringConfig {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.current
}

// IsEnvLocked reports whether any OTEL_* env var is set (triggers read-only UI banner).
func (m *MonitoringManager) IsEnvLocked() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.isEnvLockedUnsafe()
}

// isEnvLockedUnsafe checks env vars without acquiring the mutex; callers must hold at least RLock.
func (m *MonitoringManager) isEnvLockedUnsafe() bool {
	for _, envVar := range envVarChecks {
		if os.Getenv(envVar) != "" {
			return true
		}
	}
	return false
}

// LockedFields returns a map of field names to the env var names that pin them.
func (m *MonitoringManager) LockedFields() map[string]string {
	locked := make(map[string]string)
	for field, envVar := range envVarChecks {
		if os.Getenv(envVar) != "" {
			locked[field] = envVar
		}
	}
	return locked
}

// Update persists cfg and hot-reloads the OTel providers.
// Returns an EnvLockedError when OTEL_* env vars are set.
func (m *MonitoringManager) Update(ctx context.Context, cfg MonitoringConfig) error {
	if m.IsEnvLocked() {
		return &EnvLockedError{}
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	if err := m.persist(cfg); err != nil {
		return err
	}
	if err := m.reload(ctx, cfg); err != nil {
		return err
	}
	m.current = cfg
	return nil
}

// Shutdown flushes and shuts down the current providers.
func (m *MonitoringManager) Shutdown(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.providers == nil {
		return nil
	}
	return m.providers.Shutdown(ctx)
}

// reload tears down existing providers and starts new ones from cfg.
// Caller must hold m.mu write lock.
func (m *MonitoringManager) reload(_ context.Context, cfg MonitoringConfig) error {
	if m.providers != nil {
		shutCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		// Best-effort: start fresh providers even if old shutdown fails.
		if err := m.providers.Shutdown(shutCtx); err != nil {
			slog.Warn("failed to shutdown old telemetry providers during reload", "error", err)
		}
	}

	p, err := Init(context.Background(), cfg)
	if err != nil {
		return fmt.Errorf("re-initializing telemetry providers: %w", err)
	}
	m.providers = p
	return nil
}

// persist writes cfg to monitoring.json.
func (m *MonitoringManager) persist(cfg MonitoringConfig) error {
	store := configToStore(cfg)
	data, err := json.MarshalIndent(store, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling monitoring config: %w", err)
	}
	if err := os.WriteFile(m.configPath, data, 0600); err != nil {
		return fmt.Errorf("writing monitoring config: %w", err)
	}
	return nil
}

// EnvLockedError is returned by Update when any OTEL_* env var is set.
type EnvLockedError struct{}

// Error implements the error interface.
func (e *EnvLockedError) Error() string {
	return "monitoring config is locked by environment variables; unset OTEL_* env vars to configure via UI"
}
