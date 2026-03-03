package api

import (
	"encoding/json"
	"errors"
	"net/http"
	"time"

	"github.com/shaharia-lab/agento/internal/telemetry"
)

// MonitoringConfigDTO is the JSON-serialisable representation of MonitoringConfig
// used in API requests and responses. It stores the metric interval as milliseconds
// to avoid the ambiguity of Go's time.Duration nanosecond default.
type MonitoringConfigDTO struct {
	Enabled                bool              `json:"enabled"`
	MetricsExporter        string            `json:"metrics_exporter"`
	LogsExporter           string            `json:"logs_exporter"`
	OTLPEndpoint           string            `json:"otlp_endpoint"`
	OTLPHeaders            map[string]string `json:"otlp_headers,omitempty"`
	OTLPInsecure           bool              `json:"otlp_insecure"`
	MetricExportIntervalMs int64             `json:"metric_export_interval_ms"`
}

// MonitoringResponse is the response envelope for GET/PUT /api/monitoring.
type MonitoringResponse struct {
	Settings  MonitoringConfigDTO `json:"settings"`
	Locked    map[string]string   `json:"locked"`
	EnvLocked bool                `json:"env_locked"`
}

func monitoringConfigToDTO(cfg telemetry.MonitoringConfig) MonitoringConfigDTO {
	return MonitoringConfigDTO{
		Enabled:                cfg.Enabled,
		MetricsExporter:        string(cfg.MetricsExporter),
		LogsExporter:           string(cfg.LogsExporter),
		OTLPEndpoint:           cfg.OTLPEndpoint,
		OTLPHeaders:            cfg.OTLPHeaders,
		OTLPInsecure:           cfg.OTLPInsecure,
		MetricExportIntervalMs: cfg.MetricExportInterval.Milliseconds(),
	}
}

func dtoToMonitoringConfig(dto MonitoringConfigDTO) telemetry.MonitoringConfig {
	interval := time.Duration(dto.MetricExportIntervalMs) * time.Millisecond
	if interval <= 0 {
		interval = 60 * time.Second
	}
	return telemetry.MonitoringConfig{
		Enabled:              dto.Enabled,
		MetricsExporter:      telemetry.MetricsExporter(dto.MetricsExporter),
		LogsExporter:         telemetry.LogsExporter(dto.LogsExporter),
		OTLPEndpoint:         dto.OTLPEndpoint,
		OTLPHeaders:          dto.OTLPHeaders,
		OTLPInsecure:         dto.OTLPInsecure,
		MetricExportInterval: interval,
	}
}

// getMonitoring handles GET /api/monitoring.
func (s *Server) getMonitoring(w http.ResponseWriter, _ *http.Request) {
	if s.monitoringMgr == nil {
		s.writeError(w, http.StatusServiceUnavailable, "monitoring manager not configured")
		return
	}
	cfg := s.monitoringMgr.Get()
	s.writeJSON(w, http.StatusOK, MonitoringResponse{
		Settings:  monitoringConfigToDTO(cfg),
		Locked:    s.monitoringMgr.LockedFields(),
		EnvLocked: s.monitoringMgr.IsEnvLocked(),
	})
}

// putMonitoring handles PUT /api/monitoring.
func (s *Server) putMonitoring(w http.ResponseWriter, r *http.Request) {
	if s.monitoringMgr == nil {
		s.writeError(w, http.StatusServiceUnavailable, "monitoring manager not configured")
		return
	}

	var dto MonitoringConfigDTO
	if err := json.NewDecoder(r.Body).Decode(&dto); err != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	cfg := dtoToMonitoringConfig(dto)
	if err := s.monitoringMgr.Update(r.Context(), cfg); err != nil {
		var envLocked *telemetry.EnvLockedError
		if errors.As(err, &envLocked) {
			s.writeError(w, http.StatusConflict, err.Error())
			return
		}
		s.writeError(w, http.StatusInternalServerError, "updating monitoring config: "+err.Error())
		return
	}

	updated := s.monitoringMgr.Get()
	s.writeJSON(w, http.StatusOK, MonitoringResponse{
		Settings:  monitoringConfigToDTO(updated),
		Locked:    s.monitoringMgr.LockedFields(),
		EnvLocked: s.monitoringMgr.IsEnvLocked(),
	})
}
