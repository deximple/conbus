//! The wire: one type per OTLP proto message the pipe sends, each field named
//! as the proto names it, so `rename_all` makes the lowerCamelCase OTLP/JSON
//! wants. A proto `oneof` is an enum, flattened into the message around it.
//! Nothing here decides what is sent; `metrics` does.

use serde::Serialize;

/// `opentelemetry.proto.collector.metrics.v1.ExportMetricsServiceRequest`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExportMetricsServiceRequest {
    pub(super) resource_metrics: Vec<ResourceMetrics>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResourceMetrics {
    pub(super) resource: Resource,
    pub(super) scope_metrics: Vec<ScopeMetrics>,
}

#[derive(Serialize)]
pub(super) struct Resource {
    pub(super) attributes: Vec<KeyValue>,
}

/// With no `scope`: which program sent the points is the resource's to say.
#[derive(Serialize)]
pub(super) struct ScopeMetrics {
    pub(super) metrics: Vec<Metric>,
}

#[derive(Serialize)]
pub(super) struct Metric {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    pub(super) unit: &'static str,
    /// The proto's `oneof data`: `"sum": {…}` or `"gauge": {…}`.
    #[serde(flatten)]
    pub(super) data: Data,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Data {
    Sum(Sum),
    Gauge(Gauge),
}

impl Data {
    pub(super) fn points(&mut self) -> &mut Vec<NumberDataPoint> {
        match self {
            Data::Sum(sum) => &mut sum.data_points,
            Data::Gauge(gauge) => &mut gauge.data_points,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Sum {
    pub(super) aggregation_temporality: u8,
    pub(super) is_monotonic: bool,
    pub(super) data_points: Vec<NumberDataPoint>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Gauge {
    pub(super) data_points: Vec<NumberDataPoint>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NumberDataPoint {
    pub(super) attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) start_time_unix_nano: Option<String>,
    pub(super) time_unix_nano: String,
    pub(super) as_int: String,
}

#[derive(Serialize)]
pub(super) struct KeyValue {
    pub(super) key: &'static str,
    pub(super) value: AnyValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AnyValue {
    pub(super) string_value: String,
}
