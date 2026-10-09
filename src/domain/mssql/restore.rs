use super::format::MssqlFormat;
use crate::services::backup::logger::JobLogger;
use crate::services::config::DatabaseConfig;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

const DROP_DATABASE_SQL: &str = "DECLARE @q nvarchar(258) = QUOTENAME(@P1);
IF DB_ID(@P1) IS NOT NULL
BEGIN
    IF DB_ID(@P1) <= 4
        THROW 50000, N'Refusing to restore over a system database; nothing was dropped', 1;
    IF ISNULL(HAS_PERMS_BY_NAME(NULL, 'DATABASE', 'CREATE DATABASE'), 0) = 0
        THROW 50000, N'Restore needs CREATE DATABASE permission (dbcreator) to recreate the target database; nothing was dropped', 1;
    DECLARE @single bit = 0;
    IF CAST(SERVERPROPERTY('EngineEdition') AS int) <> 5
    BEGIN
        EXEC(N'ALTER DATABASE ' + @q + N' SET SINGLE_USER WITH ROLLBACK IMMEDIATE');
        SET @single = 1;
    END
    BEGIN TRY
        EXEC(N'DROP DATABASE ' + @q);
    END TRY
    BEGIN CATCH
        IF @single = 1 AND DB_ID(@P1) IS NOT NULL
            EXEC(N'ALTER DATABASE ' + @q + N' SET MULTI_USER');
        THROW;
    END CATCH
END";

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

    drop_database(&cfg, &logger).await?;

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

async fn drop_database(cfg: &DatabaseConfig, logger: &JobLogger) -> Result<()> {
    logger.log("info", format!("Replacing MSSQL database {} (drop if it exists, then recreate from the backup)", cfg.database));

    let start = Instant::now();
    let mut client = super::connection::build_client(cfg, Some("master"))
        .await
        .with_context(|| format!("Failed to connect to master for {}", cfg.name))?;
    client
        .execute(DROP_DATABASE_SQL, &[&cfg.database.as_str()])
        .await
        .with_context(|| format!("Failed to drop MSSQL database {}", cfg.database))?;

    let duration_ms = start.elapsed().as_millis() as f64;
    logger.log_command(format!("DROP DATABASE IF EXISTS [{}]", cfg.database), None, Some(0), Some(duration_ms));
    Ok(())
}
