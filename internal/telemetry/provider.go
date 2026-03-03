package telemetry

import (
	"context"
	"fmt"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/log/global"
	"go.opentelemetry.io/otel/sdk/log"
	"go.opentelemetry.io/otel/sdk/metric"
	"go.opentelemetry.io/otel/sdk/resource"
	"go.opentelemetry.io/otel/sdk/trace"
	semconv "go.opentelemetry.io/otel/semconv/v1.26.0"

	"github.com/shaharia-lab/agento/internal/build"
)

// Providers holds the OTel SDK providers and pre-built metric instruments.
type Providers struct {
	TracerProvider *trace.TracerProvider
	MeterProvider  *metric.MeterProvider
	LoggerProvider *log.LoggerProvider
	Instruments    *Instruments
}

// InitNoOp sets up no-op (SDK with no exporters) providers and registers them
// as globals. Used during Phase 1 so instrumentation compiles and runs without
// a real OTel backend.
func InitNoOp(ctx context.Context) (*Providers, error) {
	res, err := resource.New(ctx,
		resource.WithAttributes(
			semconv.ServiceName("agento"),
			semconv.ServiceVersion(build.Version),
		),
	)
	if err != nil {
		res = resource.Default()
	}

	tp := trace.NewTracerProvider(
		// NeverSample keeps spans free (non-recording) while no exporter is wired.
		// Phase 2 will switch to AlwaysSample() when real exporters are connected.
		trace.WithSampler(trace.NeverSample()),
		trace.WithResource(res),
	)
	mp := metric.NewMeterProvider(metric.WithResource(res))
	lp := log.NewLoggerProvider(log.WithResource(res))

	otel.SetTracerProvider(tp)
	otel.SetMeterProvider(mp)
	global.SetLoggerProvider(lp)

	instr, err := NewInstruments()
	if err != nil {
		return nil, fmt.Errorf("creating metric instruments: %w", err)
	}
	setGlobalInstruments(instr)

	return &Providers{
		TracerProvider: tp,
		MeterProvider:  mp,
		LoggerProvider: lp,
		Instruments:    instr,
	}, nil
}

// Shutdown flushes and shuts down all providers.
func (p *Providers) Shutdown(ctx context.Context) error {
	var errs []error
	if err := p.TracerProvider.Shutdown(ctx); err != nil {
		errs = append(errs, err)
	}
	if err := p.MeterProvider.Shutdown(ctx); err != nil {
		errs = append(errs, err)
	}
	if err := p.LoggerProvider.Shutdown(ctx); err != nil {
		errs = append(errs, err)
	}
	if len(errs) > 0 {
		return fmt.Errorf("telemetry shutdown errors: %v", errs)
	}
	return nil
}
