//! Process lifecycle management for mission execution.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::config::OrchestratorConfig;
use crate::error::{PilotError, Result};

fn get_hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "localhost".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub hostname: String,
}

impl Default for LockInfo {
    fn default() -> Self {
        Self {
            pid: std::process::id(),
            started_at: Utc::now(),
            last_heartbeat: Utc::now(),
            hostname: get_hostname(),
        }
    }
}

impl LockInfo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_process_alive(&self) -> bool {
        if self.hostname != get_hostname() {
            return false;
        }
        is_process_running(self.pid)
    }

    pub fn is_stale(&self, threshold: Duration) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.last_heartbeat);
        // Handle clock skew: if elapsed is negative, treat as potentially stale
        // (better to allow recovery than to block indefinitely)
        elapsed.to_std().map(|d| d > threshold).unwrap_or(true)
    }

    pub fn is_valid(&self, threshold: Duration) -> bool {
        self.is_process_alive() && !self.is_stale(threshold)
    }
}

#[cfg(unix)]
fn is_process_running(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    match kill(Pid::from_raw(pid as i32), None) {
        Ok(_) => true,
        Err(nix::errno::Errno::ESRCH) => false,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            o.status.success() && !out.contains("INFO:") && out.contains(&pid.to_string())
        })
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn is_process_running(_pid: u32) -> bool {
    false
}

pub struct LifecycleManager {
    locks_dir: PathBuf,
    stale_threshold: Duration,
    retry_attempts: u32,
    retry_delay_ms: u64,
    heartbeat_interval: Duration,
}

impl LifecycleManager {
    pub fn new(pilot_dir: &Path) -> Self {
        Self::with_config(pilot_dir, &OrchestratorConfig::default())
    }

    pub fn with_config(pilot_dir: &Path, config: &OrchestratorConfig) -> Self {
        Self {
            locks_dir: pilot_dir.join("locks"),
            stale_threshold: Duration::from_secs(config.lock_stale_threshold_secs),
            retry_attempts: config.lock_retry_attempts,
            retry_delay_ms: config.lock_retry_delay_ms,
            heartbeat_interval: Duration::from_secs(config.heartbeat_interval_secs),
        }
    }

    fn lock_path(&self, mission_id: &str) -> PathBuf {
        self.locks_dir.join(format!("{}.lock", mission_id))
    }

    fn temp_lock_path(&self, mission_id: &str) -> PathBuf {
        self.locks_dir
            .join(format!("{}.{}.lock.tmp", mission_id, std::process::id()))
    }

    pub async fn acquire_lock(&self, mission_id: &str) -> Result<LockGuard> {
        fs::create_dir_all(&self.locks_dir).await?;

        for attempt in 0..self.retry_attempts {
            match self.try_acquire_lock(mission_id).await {
                Ok(guard) => return Ok(guard),
                Err(PilotError::MissionAlreadyRunning { .. }) => {
                    return Err(PilotError::MissionAlreadyRunning {
                        mission_id: mission_id.to_string(),
                        pid: self
                            .read_lock(mission_id)
                            .await?
                            .map(|l| l.pid)
                            .unwrap_or(0),
                    });
                }
                Err(_) if attempt < self.retry_attempts - 1 => {
                    tokio::time::sleep(Duration::from_millis(
                        self.retry_delay_ms * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(e) => return Err(e),
            }
        }

        Err(PilotError::LockAcquisitionFailed {
            mission_id: mission_id.to_string(),
        })
    }

    async fn try_acquire_lock(&self, mission_id: &str) -> Result<LockGuard> {
        let lock_path = self.lock_path(mission_id);
        let temp_path = self.temp_lock_path(mission_id);

        if let Some(existing) = self.read_lock(mission_id).await? {
            if existing.is_valid(self.stale_threshold) {
                return Err(PilotError::MissionAlreadyRunning {
                    mission_id: mission_id.to_string(),
                    pid: existing.pid,
                });
            }
            info!(mission_id, old_pid = existing.pid, "Removing stale lock");
        }

        let lock_info = LockInfo::new();
        let content = serde_yaml_bw::to_string(&lock_info)?;
        fs::write(&temp_path, &content).await?;

        match fs::rename(&temp_path, &lock_path).await {
            Ok(_) => {
                debug!(mission_id, pid = lock_info.pid, "Lock acquired");
                Ok(LockGuard::new(
                    lock_path,
                    mission_id.to_string(),
                    self.heartbeat_interval,
                ))
            }
            Err(e) => {
                let _ = fs::remove_file(&temp_path).await;
                Err(e.into())
            }
        }
    }

    pub async fn read_lock(&self, mission_id: &str) -> Result<Option<LockInfo>> {
        let lock_path = self.lock_path(mission_id);
        match fs::read_to_string(&lock_path).await {
            Ok(content) => Ok(Some(serde_yaml_bw::from_str(&content)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn is_actively_running(&self, mission_id: &str) -> Result<bool> {
        Ok(self
            .read_lock(mission_id)
            .await?
            .map(|l| l.is_valid(self.stale_threshold))
            .unwrap_or(false))
    }

    pub async fn update_heartbeat(&self, mission_id: &str) -> Result<()> {
        let lock_path = self.lock_path(mission_id);
        if let Some(mut info) = self.read_lock(mission_id).await? {
            info.last_heartbeat = Utc::now();
            fs::write(&lock_path, serde_yaml_bw::to_string(&info)?).await?;
        }
        Ok(())
    }

    pub async fn is_orphan_state(&self, mission_id: &str) -> Result<bool> {
        Ok(self
            .read_lock(mission_id)
            .await?
            .map(|l| !l.is_valid(self.stale_threshold))
            .unwrap_or(true))
    }

    pub async fn release_lock(&self, mission_id: &str) -> Result<()> {
        let lock_path = self.lock_path(mission_id);
        match fs::remove_file(&lock_path).await {
            Ok(_) => {
                debug!(mission_id, "Lock released");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

pub struct LockGuard {
    path: PathBuf,
    mission_id: String,
    shutdown_tx: Option<watch::Sender<bool>>,
    heartbeat_handle: Option<tokio::task::JoinHandle<()>>,
}

impl LockGuard {
    /// Default max consecutive heartbeat failures before warning.
    const DEFAULT_MAX_FAILURES: u32 = 3;

    fn new(path: PathBuf, mission_id: String, heartbeat_interval: Duration) -> Self {
        Self::with_max_failures(
            path,
            mission_id,
            heartbeat_interval,
            Self::DEFAULT_MAX_FAILURES,
        )
    }

    fn with_max_failures(
        path: PathBuf,
        mission_id: String,
        heartbeat_interval: Duration,
        max_failures: u32,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let lock_path = path.clone();

        let handle = tokio::spawn(async move {
            Self::heartbeat_loop(lock_path, heartbeat_interval, shutdown_rx, max_failures).await;
        });

        Self {
            path,
            mission_id,
            shutdown_tx: Some(shutdown_tx),
            heartbeat_handle: Some(handle),
        }
    }

    async fn heartbeat_loop(
        lock_path: PathBuf,
        interval: Duration,
        mut shutdown: watch::Receiver<bool>,
        max_consecutive_failures: u32,
    ) {
        let mut consecutive_failures: u32 = 0;
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match Self::update_heartbeat_file(&lock_path).await {
                        Ok(_) => consecutive_failures = 0,
                        Err(e) => {
                            consecutive_failures += 1;
                            warn!(
                                error = %e,
                                consecutive_failures,
                                "Heartbeat update failed"
                            );
                            if consecutive_failures >= max_consecutive_failures {
                                tracing::error!(
                                    "Heartbeat failed {} times consecutively, lock may become stale",
                                    consecutive_failures
                                );
                            }
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        debug!("Heartbeat loop shutdown");
                        break;
                    }
                }
            }
        }
    }

    async fn update_heartbeat_file(lock_path: &Path) -> std::io::Result<()> {
        let content = tokio::fs::read_to_string(lock_path).await?;
        let mut lock: LockInfo = serde_yaml_bw::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        lock.last_heartbeat = Utc::now();
        let yaml = serde_yaml_bw::to_string(&lock)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Heartbeat update strategy:
        // - Unix: atomic temp+rename (safe, target replaced atomically)
        // - Windows: direct overwrite (ensures lock file always exists)
        //
        // On Windows, remove+rename leaves a gap where lock is absent, risking
        // concurrent execution if crash occurs between operations. Direct overwrite
        // is non-atomic but guarantees lock file presence for mutual exclusion.
        #[cfg(windows)]
        {
            tokio::fs::write(lock_path, &yaml).await
        }

        #[cfg(not(windows))]
        {
            let temp_path = lock_path.with_extension("lock.tmp");
            tokio::fs::write(&temp_path, &yaml).await?;
            tokio::fs::rename(&temp_path, lock_path).await
        }
    }

    pub fn mission_id(&self) -> &str {
        &self.mission_id
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }

        if let Some(handle) = self.heartbeat_handle.take() {
            handle.abort();
        }

        if let Err(e) = std::fs::remove_file(&self.path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            if !std::thread::panicking() {
                warn!(mission_id = %self.mission_id, error = %e, "Failed to release lock");
            } else {
                eprintln!(
                    "[claude-pilot] Failed to release lock for {}: {}",
                    self.mission_id, e
                );
            }
        }
    }
}
