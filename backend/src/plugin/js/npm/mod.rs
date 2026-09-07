//! npm Dependency Manager
//!
//! Handles npm dependency resolution and installation for JavaScript plugins.
//! - Parse npm dependencies from the plugin manifest
//! - Generate package.json files
//! - Execute npm install commands
//! - Manage node_modules paths
//! - Cache dependencies across plugins

mod cache;
mod package_json;
mod security;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use crate::core::error::TingError;

pub use cache::{CacheEntry, CacheStatistics};
pub use package_json::PackageJson;
pub use security::{NpmAuditResult, NpmDependency, NpmSecurityConfig, VulnerabilitySeverity};

const TRUSTED_NPM_REGISTRY: &str = "https://registry.npmjs.org/";
const NPM_VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const NPM_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_NPM_COMMAND_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const NPM_OUTPUT_TRUNCATED_MARKER: &[u8] = b"\n...[npm output truncated]\n";

pub struct PackageJsonSpec<'a> {
    pub plugin_dir: &'a Path,
    pub plugin_name: &'a str,
    pub plugin_version: &'a str,
    pub description: Option<&'a str>,
    pub author: Option<&'a str>,
    pub license: Option<&'a str>,
    pub npm_dependencies: &'a [NpmDependency],
}

fn read_bounded_command_output<R: Read>(mut reader: R) -> Vec<u8> {
    let mut retained = Vec::with_capacity(MAX_NPM_COMMAND_OUTPUT_BYTES);
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let remaining = MAX_NPM_COMMAND_OUTPUT_BYTES.saturating_sub(retained.len());
        if remaining > 0 {
            retained.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        truncated |= read > remaining;
    }
    if truncated {
        let marker_start =
            MAX_NPM_COMMAND_OUTPUT_BYTES.saturating_sub(NPM_OUTPUT_TRUNCATED_MARKER.len());
        retained.truncate(marker_start);
        retained.extend_from_slice(NPM_OUTPUT_TRUNCATED_MARKER);
    }
    retained
}

fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    operation: &str,
) -> Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(target_family = "unix")]
    {
        use std::os::unix::process::CommandExt;

        // SAFETY: This callback only creates a new process group in the child
        // between fork and exec; it does not access shared application state.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to start {operation}"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let stdout_reader = thread::spawn(move || {
        stdout
            .take()
            .map(read_bounded_command_output)
            .unwrap_or_default()
    });
    let stderr_reader = thread::spawn(move || {
        stderr
            .take()
            .map(read_bounded_command_output)
            .unwrap_or_default()
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("Failed to wait for {operation}"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            terminate_process_tree(&mut child);
            break child
                .wait()
                .with_context(|| format!("Failed to reap timed out {operation}"))?;
        }
        thread::sleep(Duration::from_millis(50));
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    if timed_out {
        anyhow::bail!("{operation} timed out after {} seconds", timeout.as_secs());
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn terminate_process_tree(child: &mut std::process::Child) {
    #[cfg(target_family = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(target_family = "unix")]
    {
        // SAFETY: npm is spawned into its own process group above, so the
        // negative PID targets only that command tree.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Dependency installation log entry
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DependencyInstallLog {
    pub timestamp: String,
    pub plugin_name: String,
    pub dependencies: Vec<NpmDependency>,
    pub success: bool,
    pub error: Option<String>,
    pub audit_result: Option<NpmAuditResult>,
}

/// npm dependency manager
pub struct NpmManager {
    npm_path: PathBuf,
    cache_dir: Option<PathBuf>,
    security_config: NpmSecurityConfig,
    log_dir: Option<PathBuf>,
    cache_registry: cache::CacheRegistry,
    cache_stats: cache::CacheStatsLock,
}

impl NpmManager {
    pub fn new(npm_path: Option<PathBuf>, cache_dir: Option<PathBuf>) -> Self {
        let npm_path = npm_path.unwrap_or_else(|| PathBuf::from("npm"));
        Self {
            npm_path,
            cache_dir,
            security_config: NpmSecurityConfig::default(),
            log_dir: None,
            cache_registry: Arc::new(RwLock::new(std::collections::HashMap::new())),
            cache_stats: Arc::new(RwLock::new(CacheStatistics::default())),
        }
    }

    pub fn with_security(
        npm_path: Option<PathBuf>,
        cache_dir: Option<PathBuf>,
        security_config: NpmSecurityConfig,
        log_dir: Option<PathBuf>,
    ) -> Self {
        let npm_path = npm_path.unwrap_or_else(|| PathBuf::from("npm"));
        Self {
            npm_path,
            cache_dir,
            security_config,
            log_dir,
            cache_registry: Arc::new(RwLock::new(std::collections::HashMap::new())),
            cache_stats: Arc::new(RwLock::new(CacheStatistics::default())),
        }
    }

    pub fn set_security_config(&mut self, config: NpmSecurityConfig) {
        self.security_config = config;
    }

    pub fn set_log_dir(&mut self, log_dir: PathBuf) {
        self.log_dir = Some(log_dir);
    }

    fn build_install_command(&self, plugin_dir: &Path) -> Command {
        let mut command = Command::new(&self.npm_path);
        command
            .arg("install")
            .arg("--omit=dev")
            .arg("--ignore-scripts=true")
            .arg("--package-lock=false")
            .arg("--fund=false")
            .arg(format!("--registry={TRUSTED_NPM_REGISTRY}"))
            .current_dir(plugin_dir);
        if !self.security_config.enable_audit {
            command.arg("--audit=false");
        }
        command
    }

    /// Parse npm dependencies from plugin manifest metadata (static method)
    pub fn parse_dependencies(plugin_json: &Value) -> Vec<NpmDependency> {
        let mut dependencies = Vec::new();

        if let Some(npm_deps) = plugin_json.get("npm_dependencies") {
            if let Some(deps_obj) = npm_deps.as_object() {
                for (name, version) in deps_obj {
                    if let Some(version_str) = version.as_str() {
                        dependencies
                            .push(NpmDependency::new(name.clone(), version_str.to_string()));
                    } else {
                        warn!(
                            "npm dependency version format invalid {}: {:?}",
                            name, version
                        );
                    }
                }
            } else if let Some(deps_array) = npm_deps.as_array() {
                for dep in deps_array {
                    if let Some(dep_obj) = dep.as_object() {
                        if let (Some(name), Some(version)) = (
                            dep_obj.get("name").and_then(|v| v.as_str()),
                            dep_obj.get("version").and_then(|v| v.as_str()),
                        ) {
                            dependencies
                                .push(NpmDependency::new(name.to_string(), version.to_string()));
                        } else {
                            warn!("npm dependency missing name or version: {:?}", dep);
                        }
                    } else {
                        warn!("npm dependency array element is not an object: {:?}", dep);
                    }
                }
            } else {
                warn!("npm_dependencies field has invalid format, expected object or array");
            }
        }

        debug!("Parsed {} npm dependencies", dependencies.len());
        dependencies
    }

    /// Generate package.json for a plugin
    pub fn generate_package_json(&self, spec: PackageJsonSpec<'_>) -> Result<PathBuf> {
        let PackageJsonSpec {
            plugin_dir,
            plugin_name,
            plugin_version,
            description,
            author,
            license,
            npm_dependencies,
        } = spec;
        package_json::generate_package_json(
            plugin_dir,
            plugin_name,
            plugin_version,
            description,
            author,
            license,
            npm_dependencies,
        )
    }

    /// Install npm dependencies for a plugin
    pub fn install_dependencies(&self, plugin_dir: &Path) -> Result<()> {
        self.install_dependencies_with_name(plugin_dir, "unknown-plugin")
    }

    /// Install npm dependencies for a plugin with logging
    pub fn install_dependencies_with_name(
        &self,
        plugin_dir: &Path,
        plugin_name: &str,
    ) -> Result<()> {
        info!(
            "Installing npm dependencies for plugin '{}' in: {}",
            plugin_name,
            plugin_dir.display()
        );
        let start_time = std::time::Instant::now();

        let package_json_path = plugin_dir.join("package.json");
        if !package_json_path.exists() {
            let error_msg = format!("package.json not found in {}", plugin_dir.display());
            self.log_installation(plugin_name, &[], false, Some(&error_msg), None)?;
            return Err(TingError::PluginLoadError(error_msg).into());
        }

        let package_json_metadata = std::fs::symlink_metadata(&package_json_path)
            .context("Failed to inspect plugin package.json")?;
        if package_json_metadata.file_type().is_symlink() || !package_json_metadata.is_file() {
            return Err(TingError::PluginLoadError(
                "Plugin package.json must be a real file".to_string(),
            )
            .into());
        }
        let package_json = PackageJson::read_from_file(&package_json_path)?;
        let dependencies: Vec<NpmDependency> = package_json
            .dependencies
            .iter()
            .map(|(name, version)| NpmDependency::new(name.clone(), version.clone()))
            .collect();

        if let Err(e) = self.validate_dependencies(&dependencies) {
            let error_msg = format!("Dependency validation failed: {}", e);
            error!("{}", error_msg);
            self.log_installation(plugin_name, &dependencies, false, Some(&error_msg), None)?;
            return Err(e);
        }

        self.prepare_trusted_install_files(plugin_dir, &package_json)?;

        self.check_npm_available()?;

        debug!("Executing: npm install in {}", plugin_dir.display());
        let mut cmd = self.build_install_command(plugin_dir);

        let output = run_command_with_timeout(
            &mut cmd,
            NPM_OPERATION_TIMEOUT,
            &format!("npm install in {}", plugin_dir.display()),
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let error_msg = format!("npm install failed: {}", stderr);
            error!("{}", error_msg);
            self.log_installation(plugin_name, &dependencies, false, Some(&error_msg), None)?;
            return Err(TingError::PluginLoadError(error_msg).into());
        }

        debug!(
            "npm install output: {}",
            String::from_utf8_lossy(&output.stdout)
        );

        let audit_result = if self.security_config.enable_audit {
            match self.run_npm_audit(plugin_dir) {
                Ok(result) => {
                    info!(
                        "npm audit completed: {} total vulnerabilities",
                        result.total
                    );
                    if self.security_config.fail_on_audit_vulnerabilities && !result.passed {
                        let error_msg = format!(
                            "npm audit found vulnerabilities above threshold ({}): {} total",
                            self.security_config.max_vulnerability_severity.as_str(),
                            result.total
                        );
                        error!("{}", error_msg);
                        self.log_installation(
                            plugin_name,
                            &dependencies,
                            false,
                            Some(&error_msg),
                            Some(result),
                        )?;
                        return Err(TingError::PluginLoadError(error_msg).into());
                    }
                    Some(result)
                }
                Err(e) => {
                    let error_msg = format!("npm audit could not be completed: {e}");
                    if self.security_config.fail_on_audit_vulnerabilities {
                        error!("{}", error_msg);
                        self.log_installation(
                            plugin_name,
                            &dependencies,
                            false,
                            Some(&error_msg),
                            None,
                        )?;
                        return Err(TingError::PluginLoadError(error_msg).into());
                    }
                    warn!("{}", error_msg);
                    None
                }
            }
        } else {
            None
        };

        let elapsed = start_time.elapsed();
        info!("npm dependencies installed successfully in {:?}", elapsed);
        self.log_installation(plugin_name, &dependencies, true, None, audit_result)?;
        Ok(())
    }

    /// Install dependencies with caching support
    pub fn install_dependencies_with_cache(
        &self,
        plugin_dir: &Path,
        plugin_name: &str,
        dependencies: &[NpmDependency],
    ) -> Result<()> {
        if dependencies.is_empty() {
            debug!("No dependencies to install for plugin: {}", plugin_name);
            return Ok(());
        }

        self.validate_dependencies(dependencies)?;

        info!(
            "Installing complete graph for {} dependencies in plugin '{}'",
            dependencies.len(),
            plugin_name
        );

        if self.cache_dir.is_some() {
            debug!(
                "Custom top-level package cache restore is disabled; npm will resolve the complete transitive dependency graph"
            );
        }

        // A top-level package directory is not a complete npm dependency graph: npm may
        // hoist transitive packages beside it. Always let npm rebuild node_modules so a
        // cache hit cannot produce runtime `module not found` failures.
        self.install_dependencies_with_name(plugin_dir, plugin_name)
    }

    // ── node_modules helpers ──

    pub fn get_node_modules_path(&self, plugin_dir: &Path) -> PathBuf {
        plugin_dir.join("node_modules")
    }

    pub fn has_node_modules(&self, plugin_dir: &Path) -> bool {
        self.get_node_modules_path(plugin_dir).exists()
    }

    pub fn clean_node_modules(&self, plugin_dir: &Path) -> Result<()> {
        let node_modules_path = self.get_node_modules_path(plugin_dir);
        if node_modules_path.exists() {
            info!("Cleaning node_modules in: {}", plugin_dir.display());
            std::fs::remove_dir_all(&node_modules_path).with_context(|| {
                format!(
                    "Failed to remove node_modules at {}",
                    node_modules_path.display()
                )
            })?;
        }
        Ok(())
    }

    // ── Cache delegation ──

    pub fn is_cached(&self, package_name: &str, version: &str) -> bool {
        cache::is_cached(&self.cache_dir, &self.cache_registry, package_name, version)
    }

    pub fn cleanup_cache_for_plugin(&self, plugin_name: &str) -> Result<usize> {
        cache::cleanup_cache_for_plugin(
            &self.cache_dir,
            &self.cache_registry,
            &self.cache_stats,
            plugin_name,
        )
    }

    pub fn cleanup_all_unused(&self) -> Result<usize> {
        cache::cleanup_all_unused(&self.cache_dir, &self.cache_registry, &self.cache_stats)
    }

    pub fn get_cache_statistics(&self) -> CacheStatistics {
        cache::get_cache_statistics(&self.cache_registry, &self.cache_stats)
    }

    pub fn clear_cache(&self) -> Result<()> {
        cache::clear_cache(&self.cache_dir, &self.cache_registry, &self.cache_stats)
    }

    // ── Private helpers ──

    fn check_npm_available(&self) -> Result<()> {
        let mut command = Command::new(&self.npm_path);
        command.arg("--version");
        let output = run_command_with_timeout(&mut command, NPM_VERSION_TIMEOUT, "npm --version")?;
        if !output.status.success() {
            return Err(TingError::PluginLoadError(
                "npm is not available or not in PATH".to_string(),
            )
            .into());
        }
        info!(
            "npm version: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
        Ok(())
    }

    fn validate_dependencies(&self, dependencies: &[NpmDependency]) -> Result<()> {
        for dependency in dependencies {
            dependency.validate().map_err(|error| {
                TingError::PluginLoadError(format!(
                    "Invalid npm dependency {}@{}: {}",
                    dependency.name, dependency.version, error
                ))
            })?;
        }

        if !self.security_config.whitelist.is_empty() {
            let mut blocked = Vec::new();
            for dep in dependencies {
                if !self.security_config.whitelist.contains(&dep.name) {
                    warn!("Dependency '{}' is not in whitelist", dep.name);
                    blocked.push(dep.name.clone());
                }
            }

            if !blocked.is_empty() {
                return Err(TingError::PluginLoadError(format!(
                    "The following dependencies are not whitelisted: {}",
                    blocked.join(", ")
                ))
                .into());
            }
        }

        Ok(())
    }

    fn prepare_trusted_install_files(
        &self,
        plugin_dir: &Path,
        package_json: &PackageJson,
    ) -> Result<()> {
        for filename in [".npmrc", "package-lock.json", "npm-shrinkwrap.json"] {
            let path = plugin_dir.join(filename);
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(TingError::PluginLoadError(format!(
                    "Plugin npm install rejected untrusted file: {filename}"
                ))
                .into());
            }
        }

        let node_modules = plugin_dir.join("node_modules");
        if let Ok(metadata) = std::fs::symlink_metadata(&node_modules)
            && (metadata.file_type().is_symlink() || !metadata.is_dir()) {
                return Err(TingError::PluginLoadError(
                    "Plugin node_modules must be a real directory".to_string(),
                )
                .into());
            }

        // Rewriting through the strict model removes scripts and other untrusted npm fields.
        package_json.write_to_file(&plugin_dir.join("package.json"))?;
        Ok(())
    }

    fn run_npm_audit(&self, plugin_dir: &Path) -> Result<NpmAuditResult> {
        info!("Running npm audit in: {}", plugin_dir.display());
        let mut command = Command::new(&self.npm_path);
        command
            .arg("audit")
            .arg("--json")
            .arg("--ignore-scripts=true")
            .arg(format!("--registry={TRUSTED_NPM_REGISTRY}"))
            .current_dir(plugin_dir);
        let output = run_command_with_timeout(
            &mut command,
            NPM_OPERATION_TIMEOUT,
            &format!("npm audit in {}", plugin_dir.display()),
        )?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let audit_json: Value =
            serde_json::from_str(&stdout).context("Failed to parse npm audit output")?;

        let mut vulnerabilities = std::collections::HashMap::new();
        let mut total = 0;

        if let Some(metadata) = audit_json.get("metadata")
            && let Some(vulns) = metadata.get("vulnerabilities") {
                for severity in &["low", "moderate", "high", "critical"] {
                    if let Some(count) = vulns.get(*severity).and_then(|v| v.as_u64()) {
                        let sev = VulnerabilitySeverity::parse(severity).unwrap();
                        vulnerabilities.insert(sev, count as usize);
                        total += count as usize;
                    }
                }
            }

        let passed = vulnerabilities
            .iter()
            .filter(|(sev, count)| {
                **sev > self.security_config.max_vulnerability_severity && **count > 0
            })
            .count()
            == 0;

        Ok(NpmAuditResult {
            vulnerabilities,
            total,
            passed,
            raw_output: stdout.to_string(),
        })
    }

    fn log_installation(
        &self,
        plugin_name: &str,
        dependencies: &[NpmDependency],
        success: bool,
        error: Option<&str>,
        audit_result: Option<NpmAuditResult>,
    ) -> Result<()> {
        let log_entry = DependencyInstallLog {
            timestamp: chrono::Utc::now().to_rfc3339(),
            plugin_name: plugin_name.to_string(),
            dependencies: dependencies.to_vec(),
            success,
            error: error.map(|s| s.to_string()),
            audit_result,
        };

        if success {
            info!(
                plugin = plugin_name,
                dep_count = dependencies.len(),
                "Dependency installation succeeded"
            );
        } else {
            error!(
                plugin = plugin_name,
                dep_count = dependencies.len(),
                error = error.unwrap_or("unknown"),
                "Dependency installation failed"
            );
        }

        if let Some(log_dir) = &self.log_dir {
            if !log_dir.exists() {
                std::fs::create_dir_all(log_dir).context("Failed to create log directory")?;
            }
            let log_file = log_dir.join(format!(
                "npm_install_{}_{}.json",
                plugin_name,
                chrono::Utc::now().format("%Y%m%d_%H%M%S")
            ));
            let log_json = serde_json::to_string_pretty(&log_entry)
                .context("Failed to serialize log entry")?;
            std::fs::write(&log_file, log_json)
                .with_context(|| format!("Failed to write log file: {}", log_file.display()))?;
            debug!("Installation log written to: {}", log_file.display());
        }

        Ok(())
    }
}

impl Default for NpmManager {
    fn default() -> Self {
        Self::new(None, None)
    }
}
