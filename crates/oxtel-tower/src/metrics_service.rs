//! Prometheus `/metrics` as a tower service.

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::task::{Context, Poll};

use http::{Request, Response, StatusCode, header};
use oxtel_prometheus::PrometheusExporter;
use tower_service::Service;

/// Answers every request with a Prometheus scrape of the exporter's
/// metrics, in the format the `Accept` header negotiates: OpenMetrics with
/// exemplars for Prometheus, the classic text format otherwise. Mount it at
/// `/metrics`: with axum, `Router::route_service("/metrics", ...)`; with
/// hyper, through hyper-util's `TowerToHyperService`.
#[derive(Clone)]
pub struct MetricsService {
    exporter: PrometheusExporter,
}

impl MetricsService {
    /// A service rendering `exporter`.
    pub fn new(exporter: PrometheusExporter) -> Self {
        MetricsService { exporter }
    }
}

impl<B> Service<Request<B>> for MetricsService {
    type Response = Response<String>;
    type Error = Infallible;
    type Future = Ready<Result<Response<String>, Infallible>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let accept = req
            .headers()
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok());
        let (content_type, body) = self.exporter.render(accept);
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
            .unwrap_or_else(|_| Response::new(String::new()));
        ready(Ok(response))
    }
}
