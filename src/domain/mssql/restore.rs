use super::format::MssqlFormat;
use crate::services::backup::logger::JobLogger;
use crate::services::config::DatabaseConfig;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

pub async fn run(cfg: DatabaseConfig, restore_file: PathBuf, logger: Arc<JobLogger>) -> Result<()> {
    logger.log("debug", format!("Starting MSSQL restore for database {}", cfg.name));

    let Some(format) = MssqlFormat::from_path(&restore_file) else {
        let message = format!(
            "Unsupported MSSQL backup file {}: expected .bacpac or .dacpac",
            restore_file.display()
        );
        logger.log("error", message.clone());
        anyhow::bail!(message);
    };

    logger.log("info", format!("Replacing MSSQL database {} (drop if it exists, then recreate from the backup)", cfg.database));
    let start = Instant::now();
    super::connection::drop_database(&cfg).await?;
    logger.log_command(format!("DROP DATABASE IF EXISTS [{}]", cfg.database), None, Some(0), Some(start.elapsed().as_millis() as f64));

    tokio::task::spawn_blocking(move || -> Result<()> {
        let connection_string = format!(
            "Server=tcp:{},{};Database={};User Id={};Password={};TrustServerCertificate=True;Encrypt=False",
            cfg.host, cfg.port, cfg.database, cfg.username, cfg.password
        );

        logger.log("info", format!(
            "MSSQL restore: {} → {}:{}/{}",
            restore_file.display(),
            cfg.host,
            cfg.port,
            cfg.database
        ));

        let start = Instant::now();
        let mut command = Command::new("sqlpackage");
        match format {
            MssqlFormat::Dacpac => command.args(["/a:Publish", "/p:AllowIncompatiblePlatform=True"]),
            MssqlFormat::Bacpac => command.arg("/a:Import"),
        };
        let output = command
            .arg(format!("/tcs:{}", connection_string))
            .arg(format!("/sf:{}", restore_file.display()))
            .output()
            .with_context(|| format!("Failed to run sqlpackage restore for {}", cfg.name))?;

        let duration_ms = start.elapsed().as_millis() as f64;
        let exit_code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let combined = format!("{}{}", stdout, stderr);

        if !output.status.success() {
            logger.log_command("sqlpackage", if combined.is_empty() { None } else { Some(combined.clone()) }, Some(exit_code), Some(duration_ms));
            logger.log("error", format!(
                "MSSQL restore failed for {} — stderr: {} stdout: {}",
                cfg.name, stderr, stdout
            ));
            anyhow::bail!("MSSQL restore failed for {}: {}", cfg.name, stderr);
        }

        logger.log_command("sqlpackage", if combined.is_empty() { None } else { Some(combined) }, Some(0), Some(duration_ms));
        logger.log("info", format!("MSSQL restore completed for {}", cfg.name));
        Ok(())
    })
    .await?
}
