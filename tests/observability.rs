//! ADR-028 observability tests.
//!
//! Each step in the rollout adds tests here. Step 1 covers the
//! `ObservabilityConfig` defaults and config deserialisation back-compat.

use smriti::config::ObservabilityConfig;

#[test]
fn observability_config_default_log_format_is_none() {
    let cfg = ObservabilityConfig::default();
    assert!(cfg.log_format.is_none());
}

#[test]
fn app_config_deserialises_without_observability_section() {
    let toml = r#"
[database]
path = "data/"

[pruning]
working_scratch_ttl_hours = 48
working_priority_ttl_days = 7
working_open_question_ttl_days = 30
access_decay_interval_days = 7
consolidation_interval_hours = 24
importance_archive_threshold = 1

[queue]
drain_interval_secs = 300
max_queue_size = 1000
"#;
    let parsed: smriti::config::AppConfig =
        toml::from_str(toml).expect("AppConfig must deserialise without [observability]");
    assert!(parsed.observability.log_format.is_none());
}
