use crate::domain::factory::DatabaseFactory;
use crate::services::backup::logger::JobLogger;
use crate::services::config::{DatabaseConfig, DbType};
use crate::tests::init_tracing_for_test;
use crate::utils::compress::{compress_to_tar_gz_large, decompress_large_tar_gz};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tiberius::{AuthMethod, Client, Config};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use testcontainers::core::IntoContainerPort;
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};
use tracing::{error, info};

const SA_PASSWORD: &str = "Test!Str0ng1";

async fn start_container() -> ContainerAsync<GenericImage> {
    GenericImage::new("mcr.microsoft.com/azure-sql-edge", "latest")
        .with_exposed_port(1433.tcp())
        .with_env_var("ACCEPT_EULA", "Y")
        .with_env_var("MSSQL_SA_PASSWORD", SA_PASSWORD)
        .start()
        .await
        .expect("azure-sql-edge container started")
}

async fn create_user_database(host: &str, port: u16, db_name: &str) {
    let mut config = Config::new();
    config.host(host);
    config.port(port);
    config.authentication(AuthMethod::sql_server("sa", SA_PASSWORD));
    config.trust_cert();

    let tcp = TcpStream::connect(config.get_addr()).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    let mut client = Client::connect(config, tcp.compat_write()).await.unwrap();

    let sql = format!(
        "IF NOT EXISTS (SELECT name FROM sys.databases WHERE name = N'{}') CREATE DATABASE [{}]",
        db_name, db_name
    );
    client.simple_query(sql.as_str()).await.unwrap();
}

fn make_config(host: String, port: u16, database: &str, generated_id: &str) -> DatabaseConfig {
    DatabaseConfig {
        name: "Test MSSQL".to_string(),
        database: database.to_string(),
        db_type: DbType::Mssql,
        username: "sa".to_string(),
        password: SA_PASSWORD.to_string(),
        port,
        host,
        generated_id: generated_id.to_string(),
        path: "".to_string(),
        max_packet_size: "".to_string(),
        volume_name: "".to_string(),
        container_name: None,
        options: std::collections::HashMap::new(),
    }
}

async fn start() -> (ContainerAsync<GenericImage>, String, u16) {
    let container = start_container().await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let host = container.get_host().await.unwrap().to_string();
    let port = container.get_host_port_ipv4(1433).await.unwrap();
    (container, host, port)
}

async fn connect_as(host: &str, port: u16, database: &str, user: &str, password: &str) -> Client<Compat<TcpStream>> {
    let mut config = Config::new();
    config.host(host);
    config.port(port);
    config.database(database);
    config.authentication(AuthMethod::sql_server(user, password));
    config.trust_cert();

    let tcp = TcpStream::connect(config.get_addr()).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    Client::connect(config, tcp.compat_write()).await.unwrap()
}

async fn exec(host: &str, port: u16, database: &str, sql: &str) {
    let mut client = connect_as(host, port, database, "sa", SA_PASSWORD).await;
    client.simple_query(sql).await.unwrap().into_results().await.unwrap();
}

async fn scalar(host: &str, port: u16, database: &str, user: &str, password: &str, sql: &str) -> i32 {
    let mut client = connect_as(host, port, database, user, password).await;
    let row = client.simple_query(sql).await.unwrap().into_row().await.unwrap().unwrap();
    row.get::<i32, _>(0).unwrap()
}

fn with_backup_format(mut config: DatabaseConfig, format: &str) -> DatabaseConfig {
    config.options.insert("backup_format".to_string(), serde_json::json!(format));
    config
}

#[tokio::test]
async fn mssql_ping_test() {
    init_tracing_for_test();

    let container = start_container().await;
    tokio::time::sleep(Duration::from_secs(30)).await;

    let host = container.get_host().await.unwrap().to_string();
    let port = container.get_host_port_ipv4(1433).await.unwrap();
    let config = make_config(host, port, "master", "5a445eb4-c2c6-4bde-a423-ee1385dcf6d3");

    let db = DatabaseFactory::create_for_backup(config).await;
    let reachable = db.ping().await.unwrap_or(false);

    assert!(reachable, "MSSQL ping should succeed");
}

#[tokio::test]
async fn mssql_backup_test() {
    init_tracing_for_test();

    let container = start_container().await;
    tokio::time::sleep(Duration::from_secs(30)).await;

    let host = container.get_host().await.unwrap().to_string();
    let port = container.get_host_port_ipv4(1433).await.unwrap();

    create_user_database(&host, port, "backupdb").await;

    let config = make_config(host, port, "backupdb", "5a445eb4-c2c6-4bde-a423-ee1385dcf6d4");
    let temp_dir = TempDir::new().unwrap();

    let db = DatabaseFactory::create_for_backup(config).await;
    let file_path = db.backup(temp_dir.path(), std::sync::Arc::new(crate::services::backup::logger::JobLogger::new())).await.unwrap();

    assert!(file_path.is_file(), "backup file should exist");
    assert!(
        file_path.metadata().unwrap().len() > 0,
        "backup file should be non-empty"
    );
    assert!(
        file_path.extension().and_then(|e| e.to_str()) == Some("dacpac"),
        "backup file should have .dacpac extension"
    );
}

#[tokio::test]
async fn mssql_backup_restore_test() {
    init_tracing_for_test();

    let container = start_container().await;
    tokio::time::sleep(Duration::from_secs(30)).await;

    let host = container.get_host().await.unwrap().to_string();
    let port = container.get_host_port_ipv4(1433).await.unwrap();

    create_user_database(&host, port, "sourcedb").await;

    let backup_config = make_config(host.clone(), port, "sourcedb", "5a445eb4-c2c6-4bde-a423-ee1385dcf6d5");
    let temp_dir = TempDir::new().unwrap();

    let db = DatabaseFactory::create_for_backup(backup_config).await;
    let file_path = db.backup(temp_dir.path(), std::sync::Arc::new(crate::services::backup::logger::JobLogger::new())).await.unwrap();
    assert!(file_path.is_file());

    let compression = compress_to_tar_gz_large(&file_path, std::sync::Arc::new(crate::services::backup::logger::JobLogger::new())).await.unwrap();
    assert!(compression.compressed_path.is_file());

    let files = decompress_large_tar_gz(
        compression.compressed_path.as_path(),
        temp_dir.path(),
    )
    .await
    .unwrap();

    let backup_file: PathBuf = if files.len() == 1 {
        files[0].clone()
    } else {
        panic!("Unexpected number of files after decompression: {}", files.len());
    };

    let restore_config = make_config(host, port, "restoreddb", "5a445eb4-c2c6-4bde-a423-ee1385dcf6d5");
    let db_restore = DatabaseFactory::create_for_restore(restore_config, &backup_file).await;

    match db_restore.restore(&backup_file, std::sync::Arc::new(crate::services::backup::logger::JobLogger::new())).await {
        Ok(_) => info!("MSSQL restore succeeded"),
        Err(e) => {
            error!("MSSQL restore failed: {:?}", e);
            panic!("Restore failed: {:?}", e);
        }
    }
}

#[tokio::test]
async fn mssql_backup_bacpac_test() {
    init_tracing_for_test();
    let (_container, host, port) = start().await;
    create_user_database(&host, port, "bacpacdb").await;

    let config = with_backup_format(
        make_config(host, port, "bacpacdb", "5a445eb4-c2c6-4bde-a423-ee1385dcf6d6"),
        "bacpac",
    );
    let temp_dir = TempDir::new().unwrap();
    let file_path = DatabaseFactory::create_for_backup(config)
        .await
        .backup(temp_dir.path(), Arc::new(JobLogger::new()))
        .await
        .unwrap();

    assert!(file_path.metadata().unwrap().len() > 0);
    assert_eq!(file_path.extension().and_then(|e| e.to_str()), Some("bacpac"));
}

#[tokio::test]
async fn mssql_backup_windows_principal_test() {
    init_tracing_for_test();
    let (_container, host, port) = start().await;
    create_user_database(&host, port, "windowsdb").await;
    exec(
        &host,
        port,
        "windowsdb",
        "CREATE TABLE dbo.t (id INT PRIMARY KEY); INSERT dbo.t VALUES (1); \
         CREATE USER [NT AUTHORITY\\SYSTEM] FOR LOGIN [NT AUTHORITY\\SYSTEM];",
    )
    .await;

    let config = make_config(host, port, "windowsdb", "5a445eb4-c2c6-4bde-a423-ee1385dcf6d7");
    let temp_dir = TempDir::new().unwrap();

    let bacpac = DatabaseFactory::create_for_backup(with_backup_format(config.clone(), "bacpac"))
        .await
        .backup(temp_dir.path(), Arc::new(JobLogger::new()))
        .await;
    assert!(bacpac.unwrap_err().to_string().contains("SQL71627"));

    let file_path = DatabaseFactory::create_for_backup(config)
        .await
        .backup(temp_dir.path(), Arc::new(JobLogger::new()))
        .await
        .unwrap();
    assert_eq!(file_path.extension().and_then(|e| e.to_str()), Some("dacpac"));
}

async fn restore_over_existing(format: &str, generated_id: &str) {
    init_tracing_for_test();
    let (_container, host, port) = start().await;
    create_user_database(&host, port, "appdb").await;
    exec(&host, port, "master", "CREATE LOGIN app_login WITH PASSWORD = 'App!Str0ng1'").await;
    exec(
        &host,
        port,
        "appdb",
        "CREATE TABLE dbo.users (id INT PRIMARY KEY, email NVARCHAR(100) NOT NULL); \
         INSERT dbo.users VALUES (1, 'a@x'), (2, 'b@x'), (3, 'c@x'); \
         CREATE USER app_user FOR LOGIN app_login; \
         GRANT SELECT ON dbo.users TO app_user;",
    )
    .await;

    let config = with_backup_format(make_config(host.clone(), port, "appdb", generated_id), format);
    let temp_dir = TempDir::new().unwrap();
    let file_path = DatabaseFactory::create_for_backup(config.clone())
        .await
        .backup(temp_dir.path(), Arc::new(JobLogger::new()))
        .await
        .unwrap();
    assert_eq!(file_path.extension().and_then(|e| e.to_str()), Some(format));

    exec(
        &host,
        port,
        "appdb",
        "DELETE FROM dbo.users WHERE id = 1; INSERT dbo.users VALUES (4, 'd@x'); CREATE TABLE dbo.extra (id INT);",
    )
    .await;

    DatabaseFactory::create_for_restore(config, &file_path)
        .await
        .restore(&file_path, Arc::new(JobLogger::new()))
        .await
        .unwrap();

    assert_eq!(scalar(&host, port, "appdb", "sa", SA_PASSWORD, "SELECT COUNT(*) FROM dbo.users").await, 3);
    assert_eq!(
        scalar(&host, port, "appdb", "sa", SA_PASSWORD, "SELECT COUNT(*) FROM dbo.users WHERE id IN (1, 2, 3)").await,
        3
    );
    assert_eq!(
        scalar(&host, port, "appdb", "sa", SA_PASSWORD, "SELECT CASE WHEN OBJECT_ID('dbo.extra') IS NULL THEN 1 ELSE 0 END").await,
        1
    );
    assert_eq!(
        scalar(&host, port, "appdb", "app_login", "App!Str0ng1", "SELECT COUNT(*) FROM dbo.users").await,
        3
    );
}

#[tokio::test]
async fn mssql_restore_over_existing_dacpac_test() {
    restore_over_existing("dacpac", "5a445eb4-c2c6-4bde-a423-ee1385dcf6d8").await;
}

#[tokio::test]
async fn mssql_restore_over_existing_bacpac_test() {
    restore_over_existing("bacpac", "5a445eb4-c2c6-4bde-a423-ee1385dcf6d9").await;
}

#[tokio::test]
async fn mssql_restore_rejects_unknown_extension_test() {
    init_tracing_for_test();
    let (_container, host, port) = start().await;
    create_user_database(&host, port, "keepdb").await;

    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("backup.dump");
    std::fs::write(&file_path, b"not a package").unwrap();

    let config = make_config(host.clone(), port, "keepdb", "5a445eb4-c2c6-4bde-a423-ee1385dcf6da");
    let result = DatabaseFactory::create_for_restore(config, &file_path)
        .await
        .restore(&file_path, Arc::new(JobLogger::new()))
        .await;

    assert!(result.unwrap_err().to_string().contains("expected .bacpac or .dacpac"));
    assert_eq!(
        scalar(&host, port, "master", "sa", SA_PASSWORD, "SELECT CASE WHEN DB_ID('keepdb') IS NULL THEN 0 ELSE 1 END").await,
        1
    );
}

mod format_tests {
    use super::{make_config, with_backup_format};
    use crate::domain::mssql::format::MssqlFormat;
    use std::path::Path;

    #[test]
    fn from_config_defaults_to_dacpac() {
        let cfg = make_config("localhost".into(), 1433, "db", "00000000-0000-0000-0000-000000000000");
        assert_eq!(MssqlFormat::from_config(&cfg), (MssqlFormat::Dacpac, None));
        assert_eq!(
            MssqlFormat::from_config(&with_backup_format(cfg.clone(), "dacpac")),
            (MssqlFormat::Dacpac, None)
        );
        assert_eq!(
            MssqlFormat::from_config(&with_backup_format(cfg.clone(), "bacpac")),
            (MssqlFormat::Bacpac, None)
        );
        assert_eq!(
            MssqlFormat::from_config(&with_backup_format(cfg, "zip")),
            (MssqlFormat::Dacpac, Some("zip".to_string()))
        );
    }

    #[test]
    fn from_path_reads_extension() {
        assert_eq!(MssqlFormat::from_path(Path::new("/tmp/a.dacpac")), Some(MssqlFormat::Dacpac));
        assert_eq!(MssqlFormat::from_path(Path::new("/tmp/a.BACPAC")), Some(MssqlFormat::Bacpac));
        assert_eq!(MssqlFormat::from_path(Path::new("/tmp/backup.dump")), None);
        assert_eq!(MssqlFormat::from_path(Path::new("/tmp/noext")), None);
        assert_eq!(MssqlFormat::Dacpac.extension(), ".dacpac");
        assert_eq!(MssqlFormat::Bacpac.extension(), ".bacpac");
    }
}
