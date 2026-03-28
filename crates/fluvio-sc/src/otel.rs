//! OpenTelemetry + Fastrace initialization for SC observability.
//!
//! Exports metrics via OTLP to the OTel collector.
//! Tracing spans via fastrace -> fastrace-opentelemetry -> OTLP.
//!
//! Configure via env vars:
//! - `OTEL_EXPORTER_OTLP_ENDPOINT` - collector address (default: http://localhost:4317)
//! - `OTEL_SERVICE_NAME` - service name (default: fluvio-sc)

use tracing::{info, warn};

use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::Resource;

/// Initialize OpenTelemetry metrics and fastrace tracing.
/// Call once during SC startup.
pub fn init_otel() {
    let service_name = std::env::var("OTEL_SERVICE_NAME")
        .unwrap_or_else(|_| "fluvio-sc".to_string());

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4317".to_string());

    // Initialize OTLP metrics exporter
    match init_metrics(&service_name, &endpoint) {
        Ok(provider) => {
            let _ = opentelemetry::global::set_meter_provider(provider);
            info!(%service_name, %endpoint, "OTel metrics initialized");
        }
        Err(err) => {
            warn!(%err, "Failed to initialize OTel metrics — continuing without observability");
        }
    }

    // Initialize fastrace with OpenTelemetry reporter
    match init_tracing(&service_name, &endpoint) {
        Ok(()) => {
            info!("Fastrace tracing initialized");
        }
        Err(err) => {
            warn!(%err, "Failed to initialize fastrace — continuing without tracing");
        }
    }
}

fn init_metrics(
    service_name: &str,
    _endpoint: &str,
) -> Result<SdkMeterProvider, Box<dyn std::error::Error>> {
    let resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", service_name.to_string()),
        ])
        .build();

    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .build()?;

    let provider = SdkMeterProvider::builder()
        .with_resource(resource)
        .with_periodic_exporter(exporter)
        .build();

    Ok(provider)
}

fn init_tracing(
    service_name: &str,
    _endpoint: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::borrow::Cow;
    use fastrace_opentelemetry::OpenTelemetryReporter;
    use opentelemetry::InstrumentationScope;

    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()?;

    let resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", service_name.to_string()),
        ])
        .build();

    let scope = InstrumentationScope::builder("fluvio-sc")
        .with_version(env!("CARGO_PKG_VERSION"))
        .build();

    let reporter = OpenTelemetryReporter::new(
        span_exporter,
        Cow::Owned(resource),
        scope,
    );

    fastrace::set_reporter(reporter, fastrace::collector::Config::default());

    Ok(())
}

/// Shutdown OTel providers gracefully.
pub fn shutdown_otel() {
    fastrace::flush();
}
