/// 返回第一版数据库迁移 SQL。
///
/// 这个导出函数保留给轻量 schema 测试使用。
pub fn schema_migrations() -> &'static str {
    include_str!("../../migrations/0001_init.sql")
}

/// 执行 MVP schema 初始化。
///
/// 使用 raw SQL 是为了允许单文件迁移里包含多条 DDL 语句。
pub async fn run_migrations(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(schema_migrations()).execute(pool).await?;
    Ok(())
}
