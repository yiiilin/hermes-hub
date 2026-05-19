use hermes_hub_backend::{build_router, AppConfig};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // MVP 默认绑定本地端口；后续配置模块会从环境变量加载真实部署参数。
    let config = AppConfig::from_env();
    let listener = TcpListener::bind(config.bind_addr).await?;
    tracing::info!("hermes-hub backend listening on {}", listener.local_addr()?);

    axum::serve(listener, build_router(config)).await?;
    Ok(())
}
