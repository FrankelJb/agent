use super::excludes::sync_excludes;
use super::stats::{SyncStats, parse_log, progress};
use crate::domain::files::matcher::{exclude_patterns, one_file_system, source_root};
use crate::services::backup::logger::JobLogger;
use crate::services::config::DatabaseConfig;
use crate::services::storage::providers::rclone::helpers::{remote_target, write_config};
use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub fn replica_target(remote_name: &str, base_path: &str, folder: &str, generated_id: &str) -> String {
    remote_target(remote_name, base_path, &format!("{folder}/sync/{generated_id}/current"))
}

pub async fn sync_dir(config_text: &str, dest: &str, cfg: &DatabaseConfig, logger: &JobLogger) -> Result<SyncStats> {
    let root = source_root(cfg)?;
    if std::fs::read_dir(&root)
        .with_context(|| format!("cannot read source directory {}", root.display()))?
        .next()
        .is_none()
    {
        bail!(
            "source directory {} is empty; refusing to sync so an unmounted volume cannot wipe the replica",
            root.display()
        );
    }
    let config = write_config(config_text)?;

    let mut cmd = Command::new("rclone");
    cmd.arg("--config")
        .arg(config.path())
        .arg("sync")
        .arg(&root)
        .arg(dest)
        .args([
            "--links", "--delete-excluded", "--use-json-log", "--stats-log-level", "NOTICE", "--retries", "1",
            "--contimeout", "30s", "--low-level-retries", "3",
        ]);
    if one_file_system(cfg) {
        cmd.arg("--one-file-system");
    }
    for pattern in sync_excludes(&exclude_patterns(cfg)) {
        cmd.arg("--exclude").arg(pattern);
    }

    let debug = tracing::enabled!(tracing::Level::DEBUG);
    cmd.args(["--stats", if debug { "5s" } else { "1h" }]);

    logger.log("info", format!("Syncing {} to {dest}", root.display()));
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => anyhow!("rclone binary not found"),
            _ => anyhow!(e).context("failed to start rclone"),
        })?;
    let mut lines = BufReader::new(child.stderr.take().context("rclone stderr unavailable")?).lines();
    let mut stderr = String::new();
    while let Some(line) = lines.next_line().await.context("failed to read rclone output")? {
        if debug {
            if let Some(progress) = progress(&line) {
                logger.log("debug", format!("progress: {progress}"));
            }
        }
        stderr.push_str(&line);
        stderr.push('\n');
    }
    let status = child.wait().await.context("failed to wait for rclone")?;

    let log = parse_log(&stderr);
    for error in &log.errors {
        logger.log("warn", format!("rclone: {error}"));
    }
    if !status.success() {
        let reason = log
            .stats
            .as_ref()
            .and_then(|s| s.last_error.clone())
            .or_else(|| log.errors.last().cloned())
            .unwrap_or_else(|| "no error message".to_string());
        bail!("rclone sync failed ({status}): {reason}; the replica may be partially updated");
    }
    let mut stats = log.stats.unwrap_or_default();
    stats.replica_bytes = match remote_size(config.path(), dest).await {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            logger.log("warn", format!("Could not measure the replica size: {e:#}"));
            None
        }
    };
    let replica = stats.replica_bytes.map(|b| format!("; replica holds {b} bytes")).unwrap_or_default();
    logger.log(
        "info",
        format!("Synced: {} file(s) transferred ({} bytes), {} deleted{replica}", stats.transfers, stats.bytes, stats.deletes),
    );
    Ok(stats)
}

async fn remote_size(config: &Path, target: &str) -> Result<u64> {
    let out = Command::new("rclone")
        .args(["--contimeout", "30s", "--low-level-retries", "3", "--config"])
        .arg(config)
        .arg("size")
        .arg(target)
        .arg("--json")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to start rclone")?;
    if !out.status.success() {
        bail!("rclone size failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    let size: serde_json::Value = serde_json::from_slice(&out.stdout).context("unexpected rclone size output")?;
    size["bytes"].as_u64().context("rclone size returned no byte count")
}
