use std::future::Future;

/// 在现有同步 store API 内部运行 PostgreSQL 异步查询。
///
/// HTTP handler 本身已经运行在 Tokio runtime 里；这里用 `block_in_place`
/// 保持当前 API 形状，后续可再整体改成原生 async store trait。
pub fn block_on_db<T>(future: impl Future<Output = T>) -> T {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
}
