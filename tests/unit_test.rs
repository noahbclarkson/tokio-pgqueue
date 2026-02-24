use tokio_pgqueue::{BackoffStrategy, EnqueueOptions, QueueConfig};
use chrono::{Duration, Utc};
use std::time::Duration as StdDuration;

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
    let before = Utc::now();
    let opts = EnqueueOptions::new().run_after(std::time::Duration::from_secs(60));
    let run_at = opts.run_at.unwrap();
    assert!(run_at >= before + Duration::seconds(60));
    assert!(run_at <= before + Duration::seconds(61));
}

#[test]
fn test_enqueue_options_default() {
    let opts = EnqueueOptions::default();
    assert_eq!(opts.priority, 100);
    assert_eq!(opts.max_attempts, None);
    assert!(opts.run_at.is_none());
}

use tokio_pgqueue::worker::WorkerConfig;

#[test]
fn test_worker_config_builder() {
    let config = WorkerConfig::default();
    assert_eq!(config.poll_interval, StdDuration::from_millis(500));
    assert_eq!(config.heartbeat_interval, StdDuration::from_secs(30));
    assert_eq!(config.auto_reclaim, true);
    assert_eq!(config.reclaim_timeout, StdDuration::from_secs(300));
}

#[test]
fn test_backoff_strategy_none() {
    let strategy = BackoffStrategy::None;
    assert_eq!(strategy.duration(0), StdDuration::ZERO);
    assert_eq!(strategy.duration(1), StdDuration::ZERO);
    assert_eq!(strategy.duration(10), StdDuration::ZERO);
}

#[test]
fn test_backoff_strategy_fixed() {
    let strategy = BackoffStrategy::Fixed(StdDuration::from_secs(30));
    assert_eq!(strategy.duration(0), StdDuration::from_secs(30));
    assert_eq!(strategy.duration(1), StdDuration::from_secs(30));
    assert_eq!(strategy.duration(5), StdDuration::from_secs(30));
}

#[test]
fn test_backoff_strategy_exponential() {
    let strategy = BackoffStrategy::Exponential {
        base_secs: 2,
        max_secs: 1024,
    };
    // 2 * 2^0 = 2
    assert_eq!(strategy.duration(0), StdDuration::from_secs(2));
    // 2 * 2^1 = 4
    assert_eq!(strategy.duration(1), StdDuration::from_secs(4));
    // 2 * 2^2 = 8
    assert_eq!(strategy.duration(2), StdDuration::from_secs(8));
    // 2 * 2^5 = 64
    assert_eq!(strategy.duration(5), StdDuration::from_secs(64));
    // 2 * 2^9 = 1024 (hits max)
    assert_eq!(strategy.duration(9), StdDuration::from_secs(1024));
    // 2 * 2^10 = 2048 but capped at 1024
    assert_eq!(strategy.duration(10), StdDuration::from_secs(1024));
}

#[test]
fn test_queue_config_default_backoff() {
    let config = QueueConfig::default();
    // Default should be exponential backoff
    match config.backoff_strategy {
        BackoffStrategy::Exponential { base_secs, max_secs } => {
            assert_eq!(base_secs, 2);
            assert_eq!(max_secs, 1024);
        }
        _ => panic!("Default backoff strategy should be Exponential"),
    }
}
