pub mod migrations;
pub mod runtime;

use sqlx::{postgres::PgPoolOptions, PgPool};

/// 创建运行时共享的 PostgreSQL 连接池。
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}
