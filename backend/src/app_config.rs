use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;

/// 应用启动配置。后续模块会逐步扩展数据库、Docker 与代理相关配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub cookie_name: String,
}

impl AppConfig {
    /// 测试环境使用固定的本地配置，避免依赖真实端口和外部环境变量。
    pub fn for_tests() -> Self {
        Self {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            cookie_name: "hermes_hub_session".to_string(),
        }
    }

    /// 运行时配置从环境变量读取，未配置时使用可在本地启动的默认值。
    pub fn from_env() -> Self {
        let bind_addr = std::env::var("HERMES_HUB_BIND_ADDR")
            .ok()
            .and_then(|value| SocketAddr::from_str(&value).ok())
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080));

        Self {
            bind_addr,
            cookie_name: std::env::var("HERMES_HUB_COOKIE_NAME")
                .unwrap_or_else(|_| "hermes_hub_session".to_string()),
        }
    }
}
