package telemetry

import (
	"context"
	"fmt"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploggrpc"
	"go.opentelemetry.io/otel/exporters/otlp/otlpmetric/otlpmetricgrpc"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc"
	promexporter "go.opentelemetry.io/otel/exporters/prometheus"
	"go.opentelemetry.io/otel/log/global"
	sdklog "go.opentelemetry.io/otel/sdk/log"
	sdkmetric "go.opentelemetry.io/otel/sdk/metric"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	semconv "go.opentelemetry.io/otel/semconv/v1.26.0"

	"github.com/shaharia-lab/agento/internal/build"
)

// Providers holds the OTel SDK providers and pre-built metric instruments.
type Providers struct {
	TracerProvider *sdktrace.TracerProvider
	MeterProvider  *sdkmetric.MeterProvider
	LoggerProvider *sdklog.LoggerProvider
	Instruments    *Instruments
}

// Init initializes OTel providers according to cfg and sets them as globals.
// When cfg.Enabled is false it sets no-op providers (no exporters, NeverSample).
func Init(ctx context.Context, cfg MonitoringConfig) (*Providers, error) {
	res, err := newResource(ctx)
	if err != nil {
		res = resource.Default()
	}

	if !cfg.Enabled {
		return initNoOpProviders(res)
	}

	return initRealProviders(ctx, cfg, res)
}

// InitNoOp is a convenience wrapper that initializes no-op providers.
// Used by tests and Phase-1 startup before real config is available.
func InitNoOp(ctx context.Context) (*Providers, error) {
	return Init(ctx, DefaultMonitoringConfig())
}

func newResource(ctx context.Context) (*resource.Resource, error) {
	return resource.New(ctx,
		resource.WithAttributes(
			semconv.ServiceName("agento"),
			semconv.ServiceVersion(build.Version),
		),
	)
}

func initNoOpProviders(res *resource.Resource) (*Providers, error) {
	tp := sdktrace.NewTracerProvider(
		sdktrace.WithSampler(sdktrace.NeverSample()),
		sdktrace.WithResource(res),
	)
	mp := sdkmetric.NewMeterProvider(sdkmetric.WithResource(res))
	lp := sdklog.NewLoggerProvider(sdklog.WithResource(res))

	return registerProviders(tp, mp, lp)
}

func initRealProviders(ctx context.Context, cfg MonitoringConfig, res *resource.Resource) (*Providers, error) {
	tp, err := buildTracerProvider(ctx, cfg, res)
	if err != nil {
		return nil, fmt.Errorf("building tracer provider: %w", err)
	}

	mp, err := buildMeterProvider(ctx, cfg, res)
	if err != nil {
		return nil, fmt.Errorf("building meter provider: %w", err)
	}

	lp, err := buildLoggerProvider(ctx, cfg, res)
	if err != nil {
		return nil, fmt.Errorf("building logger provider: %w", err)
	}

	return registerProviders(tp, mp, lp)
}

func registerProviders(
	tp *sdktrace.TracerProvider,
	mp *sdkmetric.MeterProvider,
	lp *sdklog.LoggerProvider,
) (*Providers, error) {
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

// buildTracerProvider creates a TracerProvider with an OTLP gRPC exporter.
// AlwaysSample is used when a real backend is connected.
func buildTracerProvider(
	ctx context.Context, cfg MonitoringConfig, res *resource.Resource,
) (*sdktrace.TracerProvider, error) {
	if cfg.OTLPEndpoint == "" {
		// No endpoint configured — fall back to no-op for traces.
		return sdktrace.NewTracerProvider(
			sdktrace.WithSampler(sdktrace.NeverSample()),
			sdktrace.WithResource(res),
		), nil
	}

	opts := []otlptracegrpc.Option{
		otlptracegrpc.WithEndpoint(cfg.OTLPEndpoint),
	}
	if cfg.OTLPInsecure {
		opts = append(opts, otlptracegrpc.WithInsecure())
	}
	if len(cfg.OTLPHeaders) > 0 {
		opts = append(opts, otlptracegrpc.WithHeaders(cfg.OTLPHeaders))
	}

	exp, err := otlptracegrpc.New(ctx, opts...)
	if err != nil {
		return nil, fmt.Errorf("creating OTLP trace exporter: %w", err)
	}

	return sdktrace.NewTracerProvider(
		sdktrace.WithSampler(sdktrace.AlwaysSample()),
		sdktrace.WithResource(res),
		sdktrace.WithBatcher(exp),
	), nil
}

// buildMeterProvider creates a MeterProvider backed by either Prometheus or OTLP.
func buildMeterProvider(
	ctx context.Context, cfg MonitoringConfig, res *resource.Resource,
) (*sdkmetric.MeterProvider, error) {
	switch cfg.MetricsExporter {
	case MetricsExporterPrometheus:
		exp, err := promexporter.New()
		if err != nil {
			return nil, fmt.Errorf("creating Prometheus exporter: %w", err)
		}
		return sdkmetric.NewMeterProvider(
			sdkmetric.WithResource(res),
			sdkmetric.WithReader(exp),
		), nil

	case MetricsExporterOTLP:
		if cfg.OTLPEndpoint == "" {
			break
		}
		return buildOTLPMeterProvider(ctx, cfg, res)
	}

	// Default: no-op meter provider.
	return sdkmetric.NewMeterProvider(sdkmetric.WithResource(res)), nil
}

func buildOTLPMeterProvider(
	ctx context.Context, cfg MonitoringConfig, res *resource.Resource,
) (*sdkmetric.MeterProvider, error) {
	opts := []otlpmetricgrpc.Option{
		otlpmetricgrpc.WithEndpoint(cfg.OTLPEndpoint),
	}
	if cfg.OTLPInsecure {
		opts = append(opts, otlpmetricgrpc.WithInsecure())
	}
	if len(cfg.OTLPHeaders) > 0 {
		opts = append(opts, otlpmetricgrpc.WithHeaders(cfg.OTLPHeaders))
	}

	exp, err := otlpmetricgrpc.New(ctx, opts...)
	if err != nil {
		return nil, fmt.Errorf("creating OTLP metric exporter: %w", err)
	}

	interval := cfg.MetricExportInterval
	if interval <= 0 {
		interval = 60 * time.Second
	}

	return sdkmetric.NewMeterProvider(
		sdkmetric.WithResource(res),
		sdkmetric.WithReader(sdkmetric.NewPeriodicReader(exp,
			sdkmetric.WithInterval(interval),
		)),
	), nil
}

// buildLoggerProvider creates a LoggerProvider backed by OTLP or no-op.
func buildLoggerProvider(
	ctx context.Context, cfg MonitoringConfig, res *resource.Resource,
) (*sdklog.LoggerProvider, error) {
	if cfg.LogsExporter != LogsExporterOTLP || cfg.OTLPEndpoint == "" {
		return sdklog.NewLoggerProvider(sdklog.WithResource(res)), nil
	}

	opts := []otlploggrpc.Option{
		otlploggrpc.WithEndpoint(cfg.OTLPEndpoint),
	}
	if cfg.OTLPInsecure {
		opts = append(opts, otlploggrpc.WithInsecure())
	}
	if len(cfg.OTLPHeaders) > 0 {
		opts = append(opts, otlploggrpc.WithHeaders(cfg.OTLPHeaders))
	}

	exp, err := otlploggrpc.New(ctx, opts...)
	if err != nil {
		return nil, fmt.Errorf("creating OTLP log exporter: %w", err)
	}

	return sdklog.NewLoggerProvider(
		sdklog.WithResource(res),
		sdklog.WithProcessor(sdklog.NewBatchProcessor(exp)),
	), nil
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
