use super::*;
use std::path::PathBuf;

#[test]
fn test_parse_log_level() {
    assert!(matches!(parse_log_level("debug"), Ok(Level::DEBUG)));
    assert!(matches!(parse_log_level("info"), Ok(Level::INFO)));
    assert!(matches!(parse_log_level("warn"), Ok(Level::WARN)));
    assert!(matches!(parse_log_level("error"), Ok(Level::ERROR)));
    assert!(parse_log_level("invalid").is_err());
}

#[test]
fn test_rolling_appender_paths() {
    let appender =
        RollingFileAppender::new(PathBuf::from("/tmp/logs"), "test.log".to_string(), 1024, 5);

    assert_eq!(appender.current_path(), PathBuf::from("/tmp/logs/test.log"));
    assert_eq!(
        appender.backup_path(1),
        PathBuf::from("/tmp/logs/test.log.1")
    );
    assert_eq!(
        appender.backup_path(2),
        PathBuf::from("/tmp/logs/test.log.2")
    );
}
