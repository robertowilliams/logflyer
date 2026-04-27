//! Prometheus metrics for the logflayer preprocessing pipeline.
//!
//! All metrics use the `metrics` façade crate so callers are decoupled from
//! the exporter implementation.  The exporter is installed once at startup via
//! [`install`], which also starts a small HTTP listener on `METRICS_PORT`
//! (default 9090) that serves the `/metrics` text exposition format consumed
//! by Prometheus scrapers.
//!
//! # Metric catalogue
//!
//! | Name | Type | Description |
//! |---|---|---|
//! | `logflayer_preprocessing_samples_processed_total` | Counter | Samples that completed the pipeline successfully |
//! | `logflayer_preprocessing_samples_skipped_total` | Counter | Samples marked worth_classifying=false |
//! | `logflayer_preprocessing_errors_total` | Counter | Pipeline panics / task failures |
//! | `logflayer_preprocessing_duration_seconds` | Histogram | Wall-clock time per pipeline run |
//! | `logflayer_preprocessing_agentic_signals_total` | Counter | Samples flagged as worth classifying |

use std::net::SocketAddr;

use metrics::{counter, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;
use tracing::{info, warn};

// ─── Metric name constants ────────────────────────────────────────────────────

pub const PROCESSED_TOTAL:      &str = "logflayer_preprocessing_samples_processed_total";
pub const SKIPPED_TOTAL:        &str = "logflayer_preprocessing_samples_skipped_total";
pub const ERRORS_TOTAL:         &str = "logflayer_preprocessing_errors_total";
pub const DURATION_SECONDS:     &str = "logflayer_preprocessing_duration_seconds";
pub const AGENTIC_SIGNALS_TOTAL:&str = "logflayer_preprocessing_agentic_signals_total";

// ─── Installer ───────────────────────────────────────────────────────────────

/// Install the Prometheus recorder and start the HTTP listener.
///
/// Should be called once near the top of `main`.  If the port is already in
/// use or binding fails, a warning is logged and the function returns without
/// crashing the process — metrics will still be recorded in-memory, just not
/// scraped.
///
/// The HTTP listener is served on a background thread managed by the exporter
/// crate; no additional Tokio tasks are needed.
pub fn install(port: u16) {
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();

    match PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
    {
        Ok(()) => {
            info!(port, "Prometheus metrics listening on :{}/metrics", port);
        }
        Err(e) => {
            warn!(
                port,
                error = %e,
                "Failed to start Prometheus HTTP listener — metrics will not be scraped"
            );
        }
    }
}

// ─── Recording helpers ────────────────────────────────────────────────────────

/// Record a successfully completed preprocessing run.
///
/// `worth_classifying` drives both the `processed` counter and, conditionally,
/// the `agentic_signals` counter.
#[inline]
pub fn record_processed(worth_classifying: bool) {
    counter!(PROCESSED_TOTAL).increment(1);
    if worth_classifying {
        counter!(AGENTIC_SIGNALS_TOTAL).increment(1);
    }
}

/// Record a sample that was skipped by the pipeline
/// (e.g. empty content, too-short lines).
#[inline]
pub fn record_skipped() {
    counter!(SKIPPED_TOTAL).increment(1);
}

/// Record a preprocessing task failure (spawn_blocking panic or other error).
#[inline]
pub fn record_error() {
    counter!(ERRORS_TOTAL).increment(1);
}

/// Record how long a single preprocessing run took.
#[inline]
pub fn record_duration(secs: f64) {
    histogram!(DURATION_SECONDS).record(secs);
}
