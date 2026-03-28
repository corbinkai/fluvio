use opentelemetry::metrics::{Counter, Meter};
use std::sync::OnceLock;

pub struct SpuOtelMetrics {
    pub records_received: Counter<u64>,
    pub records_sent: Counter<u64>,
    pub bytes_received: Counter<u64>,
    pub bytes_sent: Counter<u64>,
    pub smartmodule_invocations: Counter<u64>,
    pub smartmodule_errors: Counter<u64>,
}

static METRICS: OnceLock<SpuOtelMetrics> = OnceLock::new();

pub fn init_otel_metrics(meter: &Meter) {
    METRICS.get_or_init(|| SpuOtelMetrics {
        records_received: meter.u64_counter("fluvio.spu.records.received").build(),
        records_sent: meter.u64_counter("fluvio.spu.records.sent").build(),
        bytes_received: meter.u64_counter("fluvio.spu.bytes.received").build(),
        bytes_sent: meter.u64_counter("fluvio.spu.bytes.sent").build(),
        smartmodule_invocations: meter
            .u64_counter("fluvio.spu.smartmodule.invocations")
            .build(),
        smartmodule_errors: meter.u64_counter("fluvio.spu.smartmodule.errors").build(),
    });
}

pub fn otel_metrics() -> Option<&'static SpuOtelMetrics> {
    METRICS.get()
}
