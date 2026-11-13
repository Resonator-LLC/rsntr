//! Counters of the network front end, rendered in the Prometheus text
//! format by `/metrics`.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Front end counters. All updates are relaxed atomics.
#[derive(Debug, Default)]
pub struct Metrics {
    /// WebSocket connections currently open.
    pub websocket_connections: AtomicI64,
    pub websocket_connections_total: AtomicU64,
    pub websocket_messages_received_total: AtomicU64,
    pub websocket_messages_sent_total: AtomicU64,
    /// Connections closed because the client did not keep up.
    pub websocket_slow_consumers_total: AtomicU64,
    pub http_requests_total: AtomicU64,
    pub sign_ins_total: AtomicU64,
    pub sign_in_failures_total: AtomicU64,
    pub rate_limited_total: AtomicU64,
    pub csrf_rejections_total: AtomicU64,
    /// Renderers of other parts of the process, see
    /// [`Metrics::register_extension`].
    extensions: Extensions,
}

/// Appends metrics in the Prometheus text format to the output.
type Extension = Box<dyn Fn(&mut String) + Send + Sync>;

#[derive(Default)]
struct Extensions(std::sync::Mutex<Vec<Extension>>);

impl std::fmt::Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Extensions(..)")
    }
}

impl Metrics {
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds a renderer whose output `/metrics` appends to its own: the
    /// hook for parts of the process that are not in this crate (robots,
    /// agents). The renderer writes complete metrics in the Prometheus
    /// text format, with names that start with `resonator_`; see
    /// [`write_metric`].
    pub fn register_extension(&self, render: impl Fn(&mut String) + Send + Sync + 'static) {
        self.extensions
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(render));
    }

    /// What the registered extensions report.
    pub fn render_extensions(&self) -> String {
        let mut out = String::new();
        for render in self
            .extensions
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            render(&mut out);
        }
        out
    }
}

/// Writes one metric without labels in the Prometheus text format, as
/// `resonator_<name>`. `kind` is `counter` or `gauge`.
pub fn write_metric(out: &mut String, name: &str, kind: &str, help: &str, value: i128) {
    let _ = writeln!(out, "# HELP resonator_{name} {help}");
    let _ = writeln!(out, "# TYPE resonator_{name} {kind}");
    let _ = writeln!(out, "resonator_{name} {value}");
}

/// A point-in-time view of everything `/metrics` reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub websocket_connections: i64,
    pub websocket_connections_total: u64,
    pub websocket_messages_received_total: u64,
    pub websocket_messages_sent_total: u64,
    pub websocket_slow_consumers_total: u64,
    pub http_requests_total: u64,
    pub sign_ins_total: u64,
    pub sign_in_failures_total: u64,
    pub rate_limited_total: u64,
    pub csrf_rejections_total: u64,
    pub open_subscriptions: u64,
    pub submits_total: u64,
    pub submit_errors_total: u64,
    pub db_writer_queue_depth: u64,
    pub loaded_waves: u64,
}

impl Metrics {
    /// The counters kept here; the caller fills in the rest.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let get = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        MetricsSnapshot {
            websocket_connections: self.websocket_connections.load(Ordering::Relaxed),
            websocket_connections_total: get(&self.websocket_connections_total),
            websocket_messages_received_total: get(&self.websocket_messages_received_total),
            websocket_messages_sent_total: get(&self.websocket_messages_sent_total),
            websocket_slow_consumers_total: get(&self.websocket_slow_consumers_total),
            http_requests_total: get(&self.http_requests_total),
            sign_ins_total: get(&self.sign_ins_total),
            sign_in_failures_total: get(&self.sign_in_failures_total),
            rate_limited_total: get(&self.rate_limited_total),
            csrf_rejections_total: get(&self.csrf_rejections_total),
            ..MetricsSnapshot::default()
        }
    }
}

impl MetricsSnapshot {
    /// The Prometheus text exposition format, version 0.0.4.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut metric = |name: &str, kind: &str, help: &str, value: i128| {
            write_metric(&mut out, name, kind, help, value);
        };
        metric(
            "websocket_connections",
            "gauge",
            "WebSocket connections currently open.",
            self.websocket_connections.into(),
        );
        metric(
            "websocket_connections_total",
            "counter",
            "WebSocket connections accepted.",
            self.websocket_connections_total.into(),
        );
        metric(
            "websocket_messages_received_total",
            "counter",
            "Protocol messages received from clients.",
            self.websocket_messages_received_total.into(),
        );
        metric(
            "websocket_messages_sent_total",
            "counter",
            "Protocol messages queued for clients.",
            self.websocket_messages_sent_total.into(),
        );
        metric(
            "websocket_slow_consumers_total",
            "counter",
            "Connections closed because the client did not keep up.",
            self.websocket_slow_consumers_total.into(),
        );
        metric(
            "open_subscriptions",
            "gauge",
            "Wave views currently open.",
            self.open_subscriptions.into(),
        );
        metric(
            "submits_total",
            "counter",
            "Delta submissions handled by the front end.",
            self.submits_total.into(),
        );
        metric(
            "submit_errors_total",
            "counter",
            "Delta submissions that were rejected.",
            self.submit_errors_total.into(),
        );
        metric(
            "db_writer_queue_depth",
            "gauge",
            "Database writes queued or running.",
            self.db_writer_queue_depth.into(),
        );
        metric(
            "loaded_waves",
            "gauge",
            "Waves held in memory.",
            self.loaded_waves.into(),
        );
        metric(
            "http_requests_total",
            "counter",
            "HTTP requests received.",
            self.http_requests_total.into(),
        );
        metric(
            "sign_ins_total",
            "counter",
            "Successful sign-ins.",
            self.sign_ins_total.into(),
        );
        metric(
            "sign_in_failures_total",
            "counter",
            "Failed sign-ins.",
            self.sign_in_failures_total.into(),
        );
        metric(
            "rate_limited_total",
            "counter",
            "Requests refused by a rate limit.",
            self.rate_limited_total.into(),
        );
        metric(
            "csrf_rejections_total",
            "counter",
            "Requests refused by the cross-site request check.",
            self.csrf_rejections_total.into(),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_prometheus_text() {
        let metrics = Metrics::default();
        Metrics::inc(&metrics.sign_ins_total);
        metrics.websocket_connections.store(3, Ordering::Relaxed);
        let mut snapshot = metrics.snapshot();
        snapshot.open_subscriptions = 7;
        let text = snapshot.render();
        assert!(text.contains(
            "# HELP resonator_websocket_connections WebSocket connections currently open.\n\
             # TYPE resonator_websocket_connections gauge\n\
             resonator_websocket_connections 3\n"
        ));
        assert!(text.contains("\nresonator_sign_ins_total 1\n"));
        assert!(text.contains("\nresonator_open_subscriptions 7\n"));
        assert!(text.contains("# TYPE resonator_submits_total counter\n"));
        // Extensions are rendered after the built-in metrics.
        metrics.register_extension(|out| write_metric(out, "extra_total", "counter", "Extra.", 5));
        assert_eq!(
            metrics.render_extensions(),
            "# HELP resonator_extra_total Extra.\n# TYPE resonator_extra_total counter\n\
             resonator_extra_total 5\n"
        );
        // Every sample line is "name value".
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let mut parts = line.split(' ');
            assert!(parts.next().unwrap().starts_with("resonator_"));
            parts.next().unwrap().parse::<i64>().unwrap();
            assert_eq!(parts.next(), None);
        }
    }
}
