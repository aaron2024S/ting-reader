use super::*;
use crate::core::app::config::{
    AudioConfig, Config, DatabaseConfig, LoggingConfig, PluginConfig, SecurityConfig, ServerConfig,
    StorageConfig, TaskQueueConfig,
};
#[cfg(not(windows))]
use std::sync::Mutex;
use tempfile::tempdir;

#[cfg(not(windows))]
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn test_config(storage_root: PathBuf, local_roots: Vec<PathBuf>) -> Config {
    Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 3000,
            max_connections: 100,
            request_timeout: 30,
            gateway_prefix: None,
            gateway_socket: None,
        },
        database: DatabaseConfig {
            path: PathBuf::from("test.db"),
            connection_pool_size: 1,
            busy_timeout: 1000,
        },
        plugins: PluginConfig {
            plugin_dir: PathBuf::from("plugins"),
            preinstalled_dir: PathBuf::from("preinstalled-plugins"),
            enable_hot_reload: false,
            max_memory_per_plugin: 1024,
            max_execution_time: 30,
        },
        task_queue: TaskQueueConfig {
            max_concurrent_tasks: 1,
            default_retry_count: 1,
            task_timeout: 30,
        },
        logging: LoggingConfig {
            level: "info".to_string(),
            format: "json".to_string(),
            output: "stdout".to_string(),
            log_file: None,
            max_file_size: 1024,
            max_backups: 1,
        },
        security: SecurityConfig {
            enable_auth: false,
            api_key: String::new(),
            jwt_secret: "secret".to_string(),
            allowed_origins: vec!["*".to_string()],
            rate_limit_requests: 10,
            rate_limit_window: 60,
            enable_hsts: false,
            hsts_max_age: 60,
        },
        storage: StorageConfig {
            data_dir: PathBuf::from("data"),
            temp_dir: PathBuf::from("temp"),
            max_disk_usage: 1024,
            local_storage_root: storage_root,
            local_library_roots: local_roots,
        },
        audio: AudioConfig {
            cache_enabled: false,
            cache_size: 1,
            buffer_size: 1,
        },
    }
}

#[test]
fn resolves_legacy_relative_library_path() {
    let temp = tempdir().unwrap();
    let storage = temp.path().join("storage");
    let library_dir = storage.join("audiobooks");
    std::fs::create_dir_all(&library_dir).unwrap();
    let config = test_config(storage, vec![]);

    let resolved = resolve_local_library_path("audiobooks", &config).unwrap();

    assert_eq!(resolved, std::fs::canonicalize(library_dir).unwrap());
}

#[test]
fn resolves_absolute_path_inside_configured_root() {
    let temp = tempdir().unwrap();
    let legacy = temp.path().join("storage");
    let authorized = temp.path().join("media");
    let library_dir = authorized.join("book");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::create_dir_all(&library_dir).unwrap();
    let config = test_config(legacy, vec![authorized]);

    let resolved = resolve_local_library_path(&library_dir.to_string_lossy(), &config).unwrap();

    assert_eq!(resolved, std::fs::canonicalize(library_dir).unwrap());
}

#[test]
fn rejects_unauthorized_absolute_path() {
    let temp = tempdir().unwrap();
    let legacy = temp.path().join("storage");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let config = test_config(legacy, vec![]);

    let err = resolve_local_library_path(&outside.to_string_lossy(), &config).unwrap_err();

    assert!(matches!(err, TingError::SecurityViolation(_)));
}

#[test]
fn rejects_folder_sub_path_escape() {
    let temp = tempdir().unwrap();
    let storage = temp.path().join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    let config = test_config(storage, vec![]);

    let err = resolve_storage_folder_target(None, "../outside", &config).unwrap_err();

    assert!(matches!(err, TingError::ValidationError(_)));
}

#[test]
fn strips_windows_verbatim_prefix_for_display() {
    assert_eq!(
        strip_windows_verbatim_prefix(r"\\?\D:\media\book"),
        r"D:\media\book"
    );
    assert_eq!(
        strip_windows_verbatim_prefix(r"\\?\UNC\nas\share\book"),
        r"//nas\share\book"
    );
}

#[cfg(not(windows))]
#[test]
fn discovers_fnos_colon_separated_roots() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let storage = temp.path().join("storage");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    let previous = std::env::var("TRIM_DATA_ACCESSIBLE_PATHS").ok();
    unsafe {
        std::env::set_var(
            "TRIM_DATA_ACCESSIBLE_PATHS",
            format!("{}:{}", first.display(), second.display()),
        );
    }

    let config = test_config(storage, vec![]);
    let roots = discover_authorized_roots(&config);

    if let Some(previous) = previous {
        unsafe {
            std::env::set_var("TRIM_DATA_ACCESSIBLE_PATHS", previous);
        }
    } else {
        unsafe {
            std::env::remove_var("TRIM_DATA_ACCESSIBLE_PATHS");
        }
    }

    let fnos_roots = roots.iter().filter(|root| root.source == "fnos").count();
    assert_eq!(fnos_roots, 2);
}
