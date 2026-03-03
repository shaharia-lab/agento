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

// Providers holds the OTel SDK providers.
type Providers struct {
	TracerProvider *trace.TracerProvider
	MeterProvider  *metric.MeterProvider
	LoggerProvider *log.LoggerProvider
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
		trace.WithSampler(trace.AlwaysSample()),
		trace.WithResource(res),
	)
	mp := metric.NewMeterProvider(metric.WithResource(res))
	lp := log.NewLoggerProvider(log.WithResource(res))

	otel.SetTracerProvider(tp)
	otel.SetMeterProvider(mp)
	global.SetLoggerProvider(lp)

	return &Providers{
		TracerProvider: tp,
		MeterProvider:  mp,
		LoggerProvider: lp,
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
