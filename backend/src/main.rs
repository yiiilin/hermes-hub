use hermes_hub_backend::{build_router_from_config, AppConfig};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // 启动路径统一从环境变量读取部署参数，并注入真实代理和 Docker adapter。
    let config = AppConfig::from_env();
    let listener = TcpListener::bind(config.bind_addr).await?;
    tracing::info!("hermes-hub backend listening on {}", listener.local_addr()?);

    axum::serve(listener, build_router_from_config(config).await?).await?;
    Ok(())
}
