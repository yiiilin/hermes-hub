use hermes_hub_backend::{skills_fs::ReadonlySkillsFs, AppConfig};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = AppConfig::from_env();
    let fs = ReadonlySkillsFs::from_object_storage_config(
        &config.object_storage,
        &config.skills_fs.prefix,
    )?;
    let bind_addr = config.skills_fs.bind_addr.to_string();
    let mut listener = NFSTcpListener::bind(&bind_addr, fs).await?;
    listener.with_export_name(&config.skills_fs.export_name);

    tracing::info!(
        bind_addr = %bind_addr,
        export = %format!("/{}", config.skills_fs.export_name.trim_matches('/')),
        prefix = %config.skills_fs.prefix,
        bucket = %config.object_storage.bucket,
        "hermes-hub-skills-fs listening"
    );

    // nfsserve 在同一个 TCP 端口上处理 NFS、mount 和简化 portmap；
    // Docker/NFS 客户端需要同时传入 port 与 mountport 指向这个端口。
    listener.handle_forever().await?;
    Ok(())
}
