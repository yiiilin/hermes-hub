/// 返回第一版数据库迁移 SQL。
///
/// 这里先把 schema 作为单一迁移文件管理，后续如果引入真实 migration runner，
/// 这个导出函数可以继续作为测试和初始化入口使用。
pub fn schema_migrations() -> &'static str {
    include_str!("../../migrations/0001_init.sql")
}
