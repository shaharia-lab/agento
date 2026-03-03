# Monitoring

Agento emits **traces**, **metrics**, and **logs** via OpenTelemetry. You can configure exporters through the UI or environment variables and ship signals to any compatible backend.

---

## Configuration

### Option 1 — Settings UI

Go to **Settings → Monitoring**, toggle **Enable monitoring**, fill in your exporter details, and click **Save**. The configuration takes effect immediately — no restart required.

| Field | Description |
|-------|-------------|
| Metrics exporter | `otlp` (push to a collector) or `prometheus` (pull via `/metrics`) |
| Logs exporter | `otlp` |
| OTLP endpoint | Host and port of your collector, e.g. `localhost:4317` |
| OTLP headers | Optional key=value pairs for authentication (e.g. `x-api-key=secret`) |
| Insecure | Enable when the collector does not use TLS (typical for local setups) |
| Metric export interval | How often to push metrics, in milliseconds (default: 60 000) |

> **Note:** If any `OTEL_*` environment variable is set, the UI shows an amber lock banner and the fields become read-only. Unset the env vars to use the UI instead.

### Option 2 — Environment variables

Set these before starting agento. They take priority over the Settings UI.

```bash
# Enable telemetry and point to a collector
export OTEL_EXPORTER_OTLP_ENDPOINT=localhost:4317
export OTEL_EXPORTER_OTLP_INSECURE=true

# Choose exporters
export OTEL_METRICS_EXPORTER=otlp   # or: prometheus
export OTEL_LOGS_EXPORTER=otlp

# Optional
export OTEL_EXPORTER_OTLP_HEADERS="x-api-key=secret"
export OTEL_METRIC_EXPORT_INTERVAL=10000   # milliseconds

agento web
```

To use the Prometheus pull model instead of OTLP push:

```bash
export OTEL_METRICS_EXPORTER=prometheus
agento web
# Metrics available at http://localhost:8990/metrics
```

---

## What is instrumented

| Signal | What is captured |
|--------|-----------------|
| **Traces** | Every HTTP request (method, path, status, duration) |
| **Metrics** | HTTP requests & duration · Agent runs, duration, input/output tokens · Chat sessions created/deleted · Storage operation counts & duration |
| **Logs** | All structured application logs (startup, requests, errors, agent events) |

---

## Test locally with Grafana LGTM

[`grafana/otel-lgtm`](https://github.com/grafana/otel-lgtm) bundles Grafana, Tempo (traces), Prometheus (metrics), and Loki (logs) in a single Docker image — no configuration needed.

**1. Start the stack**

```bash
docker run -d --name otel-lgtm \
  -p 3000:3000 \
  -p 4317:4317 \
  -p 4318:4318 \
  grafana/otel-lgtm
```

Wait ~10 seconds for the stack to be ready (check `docker logs otel-lgtm`).

**2. Start agento with OTLP pointing at the stack**

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=localhost:4317 \
OTEL_EXPORTER_OTLP_INSECURE=true \
OTEL_METRICS_EXPORTER=otlp \
OTEL_LOGS_EXPORTER=otlp \
OTEL_METRIC_EXPORT_INTERVAL=10000 \
agento web
```

Or configure the same values in **Settings → Monitoring** after starting normally.

**3. Open Grafana**

Navigate to [http://localhost:3000](http://localhost:3000) (default credentials: `admin` / `admin`).

| Signal | Where to look |
|--------|--------------|
| Traces | Explore → datasource **Tempo** → search for service `agento` |
| Metrics | Explore → datasource **Prometheus** → query `agento_*` |
| Logs | Explore → datasource **Loki** → label filter `service_name = agento` |

Use agento normally (open chats, run agents) to generate activity, then refresh the Explore view.

---

## Connecting to a production backend

Replace `localhost:4317` with your collector endpoint and add any required auth headers.

**Grafana Cloud example**

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=otlp-gateway-prod-us-central-0.grafana.net:443
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic <base64-encoded-instance-id:api-token>"
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
```

**Honeycomb example**

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=api.honeycomb.io:443
export OTEL_EXPORTER_OTLP_HEADERS="x-honeycomb-team=your-api-key"
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
```

Any OpenTelemetry-compatible backend (Jaeger, Zipkin via a collector, Datadog, New Relic, etc.) works the same way — point `OTEL_EXPORTER_OTLP_ENDPOINT` at your collector's gRPC endpoint.
