package telemetry

import (
	"os"
	"strconv"
	"strings"
	"time"
)

// MetricsExporter selects the metrics export backend.
type MetricsExporter string

const (
	// MetricsExporterOTLP exports metrics via OTLP gRPC.
	MetricsExporterOTLP MetricsExporter = "otlp"
	// MetricsExporterPrometheus exports metrics via the Prometheus pull model.
	MetricsExporterPrometheus MetricsExporter = "prometheus"
	// MetricsExporterNone disables metrics export.
	MetricsExporterNone MetricsExporter = "none"
)

// LogsExporter selects the log export backend.
type LogsExporter string

const (
	// LogsExporterOTLP exports logs via OTLP gRPC.
	LogsExporterOTLP LogsExporter = "otlp"
	// LogsExporterNone disables log export.
	LogsExporterNone LogsExporter = "none"
)

// MonitoringConfig holds all telemetry configuration.
type MonitoringConfig struct {
	// Enabled controls whether telemetry is active. Maps to OTEL_SDK_DISABLED (inverted).
	Enabled bool

	// MetricsExporter selects the metrics backend. Maps to OTEL_METRICS_EXPORTER.
	MetricsExporter MetricsExporter

	// LogsExporter selects the log backend. Maps to OTEL_LOGS_EXPORTER.
	LogsExporter LogsExporter

	// OTLPEndpoint is the gRPC endpoint for the OTLP collector.
	// Maps to OTEL_EXPORTER_OTLP_ENDPOINT.
	OTLPEndpoint string

	// OTLPHeaders are key=value pairs sent with every OTLP request.
	// Maps to OTEL_EXPORTER_OTLP_HEADERS (comma-separated key=value).
	OTLPHeaders map[string]string

	// MetricExportInterval is how often metrics are pushed to OTLP.
	// Maps to OTEL_METRIC_EXPORT_INTERVAL (milliseconds). Default: 60000ms.
	MetricExportInterval time.Duration
}

// DefaultMonitoringConfig returns the default (disabled, no exporters) config.
func DefaultMonitoringConfig() MonitoringConfig {
	return MonitoringConfig{
		Enabled:              false,
		MetricsExporter:      MetricsExporterNone,
		LogsExporter:         LogsExporterNone,
		OTLPEndpoint:         "",
		OTLPHeaders:          map[string]string{},
		MetricExportInterval: 60 * time.Second,
	}
}

// ConfigFromEnv reads MonitoringConfig from standard OTEL_* environment variables.
// If OTEL_SDK_DISABLED=true the config is returned with Enabled=false.
func ConfigFromEnv() MonitoringConfig {
	cfg := DefaultMonitoringConfig()

	if v := os.Getenv("OTEL_SDK_DISABLED"); v == "true" || v == "1" {
		return cfg
	}

	// Any meaningful OTEL config implies enabled=true.
	metricsExp := strings.ToLower(strings.TrimSpace(os.Getenv("OTEL_METRICS_EXPORTER")))
	logsExp := strings.ToLower(strings.TrimSpace(os.Getenv("OTEL_LOGS_EXPORTER")))
	endpoint := strings.TrimSpace(os.Getenv("OTEL_EXPORTER_OTLP_ENDPOINT"))

	if metricsExp == "" && logsExp == "" && endpoint == "" {
		// Nothing configured — stay disabled (no-op), same as Phase 1.
		return cfg
	}

	cfg.Enabled = true
	cfg.MetricsExporter = parseMetricsExporter(metricsExp)
	cfg.LogsExporter = parseLogsExporter(logsExp)
	cfg.OTLPEndpoint = endpoint
	cfg.OTLPHeaders = parseOTLPHeaders(os.Getenv("OTEL_EXPORTER_OTLP_HEADERS"))
	cfg.MetricExportInterval = parseMetricExportInterval(
		os.Getenv("OTEL_METRIC_EXPORT_INTERVAL"),
		cfg.MetricExportInterval,
	)

	return cfg
}

// parseMetricsExporter maps the OTEL_METRICS_EXPORTER env value to a MetricsExporter.
func parseMetricsExporter(v string) MetricsExporter {
	switch v {
	case "otlp":
		return MetricsExporterOTLP
	case "prometheus":
		return MetricsExporterPrometheus
	default:
		return MetricsExporterNone
	}
}

// parseLogsExporter maps the OTEL_LOGS_EXPORTER env value to a LogsExporter.
func parseLogsExporter(v string) LogsExporter {
	if v == "otlp" {
		return LogsExporterOTLP
	}
	return LogsExporterNone
}

// parseMetricExportInterval parses the OTEL_METRIC_EXPORT_INTERVAL env value (milliseconds).
// Returns defaultVal if the value is missing or invalid.
func parseMetricExportInterval(v string, defaultVal time.Duration) time.Duration {
	if v == "" {
		return defaultVal
	}
	ms, err := strconv.Atoi(v)
	if err != nil || ms <= 0 {
		return defaultVal
	}
	return time.Duration(ms) * time.Millisecond
}

// parseOTLPHeaders parses a comma-separated "key=value,key2=value2" string.
func parseOTLPHeaders(raw string) map[string]string {
	headers := make(map[string]string)
	if raw == "" {
		return headers
	}
	for _, pair := range strings.Split(raw, ",") {
		pair = strings.TrimSpace(pair)
		if pair == "" {
			continue
		}
		idx := strings.IndexByte(pair, '=')
		if idx <= 0 {
			continue
		}
		headers[strings.TrimSpace(pair[:idx])] = strings.TrimSpace(pair[idx+1:])
	}
	return headers
}
