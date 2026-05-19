/// Hermes 健康状态占位结构。后续接入 `/health` 与 `/health/detailed` 时扩展。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthStatus {
    pub status: String,
}
