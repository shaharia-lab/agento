package telemetry_test

import (
	"context"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/telemetry"
)

func TestInitNoOp(t *testing.T) {
	p, err := telemetry.InitNoOp(context.Background())
	require.NoError(t, err)
	require.NotNil(t, p)
	require.NotNil(t, p.TracerProvider)
	require.NotNil(t, p.MeterProvider)
	require.NotNil(t, p.LoggerProvider)
	require.NotNil(t, p.Instruments)
	require.NotNil(t, telemetry.GetGlobalInstruments())

	require.NoError(t, p.Shutdown(context.Background()))
}

func TestInit_Prometheus(t *testing.T) {
	cfg := telemetry.DefaultMonitoringConfig()
	cfg.Enabled = true
	cfg.MetricsExporter = telemetry.MetricsExporterPrometheus

	p, err := telemetry.Init(context.Background(), cfg)
	require.NoError(t, err)
	require.NotNil(t, p)
	require.NotNil(t, p.Instruments)
	require.NoError(t, p.Shutdown(context.Background()))
}
