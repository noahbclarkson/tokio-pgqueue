//! Optional metrics telemetry hooks for `tokio-pgqueue`.
//!
//! When the `metrics` feature is enabled, this module records observability data
//! using the [`metrics`](https://docs.rs/metrics) facade crate. Any compatible
//! recorder (e.g. `metrics-exporter-prometheus`) can be installed to collect the data.
//!
//! When the `metrics` feature is **disabled**, all functions in this module are
//! zero-cost no-ops that will be optimised away entirely by the compiler.
//!
//! # Metrics emitted
//!
//! | Name | Kind | Labels |
//! |------|------|--------|
//! | `tokio_pgqueue_jobs_enqueued_total` | Counter | `queue_name` |
//! | `tokio_pgqueue_jobs_completed_total` | Counter | `queue_name`, `status` |
//! | `tokio_pgqueue_job_processing_seconds` | Histogram | `queue_name` |
//! | `tokio_pgqueue_heartbeats_total` | Counter | `queue_name`, `status` |
//! | `tokio_pgqueue_dlq_entries_total` | Counter | `queue_name` |
//! | `tokio_pgqueue_queue_depth` | Gauge | `queue_name` |

/// Increment the jobs-enqueued counter.
#[inline(always)]
pub(crate) fn record_job_enqueued(queue_name: &str) {
    #[cfg(feature = "metrics")]
    {
        metrics::counter!(
            "tokio_pgqueue_jobs_enqueued_total",
            "queue_name" => queue_name.to_owned()
        )
        .increment(1);
    }
    #[cfg(not(feature = "metrics"))]
    let _ = queue_name;
}

/// Increment the jobs-completed counter with the given outcome status.
///
/// `status` should be `"success"` or `"failed"`.
#[inline(always)]
pub(crate) fn record_job_completed(queue_name: &str, status: &'static str) {
    #[cfg(feature = "metrics")]
    {
        metrics::counter!(
            "tokio_pgqueue_jobs_completed_total",
            "queue_name" => queue_name.to_owned(),
            "status" => status
        )
        .increment(1);
    }
    #[cfg(not(feature = "metrics"))]
    {
        let _ = queue_name;
        let _ = status;
    }
}

/// Record the wall-clock processing time (in seconds) for a completed job.
#[inline(always)]
pub(crate) fn record_job_processing_time(queue_name: &str, duration_secs: f64) {
    #[cfg(feature = "metrics")]
    {
        metrics::histogram!(
            "tokio_pgqueue_job_processing_seconds",
            "queue_name" => queue_name.to_owned()
        )
        .record(duration_secs);
    }
    #[cfg(not(feature = "metrics"))]
    {
        let _ = queue_name;
        let _ = duration_secs;
    }
}

/// Increment the heartbeat counter.
///
/// `status` should be `"success"` or `"failure"`.
#[inline(always)]
pub(crate) fn record_heartbeat(queue_name: &str, status: &'static str) {
    #[cfg(feature = "metrics")]
    {
        metrics::counter!(
            "tokio_pgqueue_heartbeats_total",
            "queue_name" => queue_name.to_owned(),
            "status" => status
        )
        .increment(1);
    }
    #[cfg(not(feature = "metrics"))]
    {
        let _ = queue_name;
        let _ = status;
    }
}

/// Increment the dead-letter-queue entries counter.
#[inline(always)]
pub(crate) fn record_dlq_entry(queue_name: &str) {
    #[cfg(feature = "metrics")]
    {
        metrics::counter!(
            "tokio_pgqueue_dlq_entries_total",
            "queue_name" => queue_name.to_owned()
        )
        .increment(1);
    }
    #[cfg(not(feature = "metrics"))]
    let _ = queue_name;
}

/// Set the queue-depth gauge.
#[inline(always)]
pub(crate) fn record_queue_depth(queue_name: &str, depth: f64) {
    #[cfg(feature = "metrics")]
    {
        metrics::gauge!(
            "tokio_pgqueue_queue_depth",
            "queue_name" => queue_name.to_owned()
        )
        .set(depth);
    }
    #[cfg(not(feature = "metrics"))]
    {
        let _ = queue_name;
        let _ = depth;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test: all recording functions are callable and must not panic, regardless
    /// of whether the `metrics` feature is enabled.
    #[test]
    fn test_all_record_fns_do_not_panic() {
        record_job_enqueued("smoke_queue");
        record_job_completed("smoke_queue", "success");
        record_job_completed("smoke_queue", "failed");
        record_job_processing_time("smoke_queue", 0.123);
        record_heartbeat("smoke_queue", "success");
        record_heartbeat("smoke_queue", "failure");
        record_dlq_entry("smoke_queue");
        record_queue_depth("smoke_queue", 5.0);
    }

    /// When the `metrics` feature is enabled, verify that the counters are actually
    /// incremented by installing a `DebuggingRecorder` from `metrics-util`.
    #[cfg(feature = "metrics")]
    #[test]
    fn test_counters_are_incremented() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        metrics::with_local_recorder(&recorder, || {
            record_job_enqueued("counter_test_queue");
            record_job_enqueued("counter_test_queue");
            record_job_completed("counter_test_queue", "success");
            record_dlq_entry("counter_test_queue");
            record_heartbeat("counter_test_queue", "success");
        });

        let snapshot = snapshotter.snapshot();
        let data = snapshot.into_vec();

        // Build a map of metric_name -> value for easy assertion
        let mut counters: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for (key, _unit, _desc, value) in &data {
            if let DebugValue::Counter(v) = value {
                counters.insert(key.key().name().to_string(), *v);
            }
        }

        assert_eq!(
            counters.get("tokio_pgqueue_jobs_enqueued_total").copied(),
            Some(2),
            "enqueued counter should be 2"
        );
        assert_eq!(
            counters.get("tokio_pgqueue_jobs_completed_total").copied(),
            Some(1),
            "completed counter should be 1"
        );
        assert_eq!(
            counters.get("tokio_pgqueue_dlq_entries_total").copied(),
            Some(1),
            "dlq counter should be 1"
        );
        assert_eq!(
            counters.get("tokio_pgqueue_heartbeats_total").copied(),
            Some(1),
            "heartbeat counter should be 1"
        );
    }

    /// Verify that the histogram records a value.
    #[cfg(feature = "metrics")]
    #[test]
    fn test_histogram_records_value() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        metrics::with_local_recorder(&recorder, || {
            record_job_processing_time("hist_test_queue", 1.5);
        });

        let snapshot = snapshotter.snapshot();
        let data = snapshot.into_vec();

        let hist = data.iter().find(|(key, _, _, _)| {
            key.key().name() == "tokio_pgqueue_job_processing_seconds"
        });
        assert!(hist.is_some(), "histogram metric should be recorded");
        if let Some((_, _, _, DebugValue::Histogram(vals))) = hist {
            assert!(!vals.is_empty(), "histogram should contain at least one value");
            assert!(
                (vals[0] - 1.5).abs() < 1e-9,
                "histogram value should be 1.5"
            );
        }
    }

    /// Verify that the gauge records a value.
    #[cfg(feature = "metrics")]
    #[test]
    fn test_gauge_records_queue_depth() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        metrics::with_local_recorder(&recorder, || {
            record_queue_depth("depth_test_queue", 42.0);
        });

        let snapshot = snapshotter.snapshot();
        let data = snapshot.into_vec();

        let gauge = data
            .iter()
            .find(|(key, _, _, _)| key.key().name() == "tokio_pgqueue_queue_depth");
        assert!(gauge.is_some(), "gauge metric should be recorded");
        if let Some((_, _, _, DebugValue::Gauge(v))) = gauge {
            assert!(
                (v - 42.0).abs() < 1e-9,
                "gauge value should be 42.0"
            );
        }
    }
}
