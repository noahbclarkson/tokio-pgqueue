use tokio_pgqueue::EnqueueOptions;
use chrono::{Utc, Duration};

#[test]
fn test_enqueue_options_builder() {
    let now = Utc::now();
    let opts = EnqueueOptions::new()
        .priority(50)
        .max_attempts(5)
        .run_at(now);
        
    assert_eq!(opts.priority, 50);
    assert_eq!(opts.max_attempts, Some(5));
    assert_eq!(opts.run_at, Some(now));
}

#[test]
fn test_enqueue_options_run_after() {
    let opts = EnqueueOptions::new().run_after(std::time::Duration::from_secs(60));
    let run_at = opts.run_at.unwrap();
    let now = Utc::now();
    assert!(run_at > now && run_at <= now + Duration::seconds(61));
}

#[test]
fn test_enqueue_options_default() {
    let opts = EnqueueOptions::default();
    assert_eq!(opts.priority, 100);
    assert_eq!(opts.max_attempts, None);
    assert!(opts.run_at.is_none());
}

use tokio_pgqueue::worker::WorkerConfig;
use std::time::Duration as StdDuration;

#[test]
fn test_worker_config_builder() {
    let config = WorkerConfig::default();
    assert_eq!(config.poll_interval, StdDuration::from_millis(500));
    assert_eq!(config.heartbeat_interval, StdDuration::from_secs(30));
    assert_eq!(config.auto_reclaim, true);
    assert_eq!(config.reclaim_timeout, StdDuration::from_secs(300));
}
