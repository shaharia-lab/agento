package telemetry_test

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/telemetry"
)

func TestConfigFromEnv_Disabled(t *testing.T) {
	t.Setenv("OTEL_SDK_DISABLED", "true")
	cfg := telemetry.ConfigFromEnv()
	assert.False(t, cfg.Enabled)
}

func TestConfigFromEnv_NothingSet(t *testing.T) {
	// Ensure no OTEL vars are set
	cfg := telemetry.ConfigFromEnv()
	assert.False(t, cfg.Enabled)
}

func TestConfigFromEnv_PrometheusMetrics(t *testing.T) {
	t.Setenv("OTEL_METRICS_EXPORTER", "prometheus")
	cfg := telemetry.ConfigFromEnv()
	assert.True(t, cfg.Enabled)
	assert.Equal(t, telemetry.MetricsExporterPrometheus, cfg.MetricsExporter)
}

func TestConfigFromEnv_OTLPEndpoint(t *testing.T) {
	t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "localhost:4317")
	t.Setenv("OTEL_METRICS_EXPORTER", "otlp")
	cfg := telemetry.ConfigFromEnv()
	assert.True(t, cfg.Enabled)
	assert.Equal(t, "localhost:4317", cfg.OTLPEndpoint)
	assert.Equal(t, telemetry.MetricsExporterOTLP, cfg.MetricsExporter)
}

func TestConfigFromEnv_Headers(t *testing.T) {
	t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "localhost:4317")
	t.Setenv("OTEL_EXPORTER_OTLP_HEADERS", "x-api-key=secret,x-tenant=acme")
	cfg := telemetry.ConfigFromEnv()
	require.Equal(t, "secret", cfg.OTLPHeaders["x-api-key"])
	require.Equal(t, "acme", cfg.OTLPHeaders["x-tenant"])
}

func TestConfigFromEnv_MetricInterval(t *testing.T) {
	t.Setenv("OTEL_METRICS_EXPORTER", "otlp")
	t.Setenv("OTEL_METRIC_EXPORT_INTERVAL", "5000")
	cfg := telemetry.ConfigFromEnv()
	assert.Equal(t, 5*time.Second, cfg.MetricExportInterval)
}
