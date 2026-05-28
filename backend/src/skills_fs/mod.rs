use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use nfsserve::{
    nfs::{
        fattr3, fileid3, filename3, fsinfo3, ftype3, gid3, nfs_fh3, nfspath3, nfsstat3, nfstime3,
        post_op_attr, sattr3, set_size3, specdata3, uid3, FSF_HOMOGENEOUS,
    },
    vfs::{NFSFileSystem, ReadDirResult, VFSCapabilities},
};
use opendal::{services::S3, Entry, ErrorKind, Metadata, Operator};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::app_config::ObjectStorageConfig;

const ROOT_ID: fileid3 = 1;
const FS_ID: u64 = 0x4848_534b_494c_4c53;
const DIR_MODE: u32 = 0o755;
const FILE_MODE: u32 = 0o644;
const HIDDEN_SEGMENTS: [&str; 3] = [".curator_state", ".bundled_manifest", ".hub-directory"];
const MANAGED_SKILLS_DIR: &str = "skills";
const MANAGED_PROFILE_FILES: [&str; 1] = ["SOUL.md"];

#[derive(Debug, Error)]
pub enum SkillsFsError {
    #[error("invalid skills filesystem prefix")]
    InvalidPrefix,
    #[error("skills path not found")]
    NotFound,
    #[error("object storage endpoint is required")]
    MissingEndpoint,
    #[error("object storage access key is required")]
    MissingAccessKey,
    #[error("object storage secret key is required")]
    MissingSecretKey,
    #[error("opendal operation failed: {0}")]
    Opendal(#[from] opendal::Error),
    #[error("skills filesystem lock failed")]
    LockFailed,
    #[error("skills directory is not empty")]
    DirectoryNotEmpty,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillsNode {
    pub id: fileid3,
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug)]
struct SkillsFsIndex {
    path_to_id: HashMap<String, fileid3>,
    id_to_node: HashMap<fileid3, SkillsNode>,
}

impl Default for SkillsFsIndex {
    fn default() -> Self {
        let mut path_to_id = HashMap::new();
        let mut id_to_node = HashMap::new();
        path_to_id.insert(String::new(), ROOT_ID);
        id_to_node.insert(
            ROOT_ID,
            SkillsNode {
                id: ROOT_ID,
                path: String::new(),
                name: String::new(),
                is_dir: true,
                size: 0,
                modified: None,
            },
        );

        Self {
            path_to_id,
            id_to_node,
        }
    }
}

/// OpenDAL 负责实时读取 S3/RustFS；这里的索引只保存 NFS fileid 与路径映射。
pub struct SkillsFs {
    operator: Operator,
    prefix: String,
    profile_prefix: Option<String>,
    index: Mutex<SkillsFsIndex>,
}

impl SkillsFs {
    pub fn new(operator: Operator, prefix: impl AsRef<str>) -> Result<Self, SkillsFsError> {
        let prefix = normalize_prefix(prefix.as_ref()).ok_or(SkillsFsError::InvalidPrefix)?;
        Ok(Self {
            operator,
            prefix,
            profile_prefix: None,
            index: Mutex::new(SkillsFsIndex::default()),
        })
    }

    pub fn with_profile_prefix(mut self, prefix: impl AsRef<str>) -> Result<Self, SkillsFsError> {
        self.profile_prefix =
            Some(normalize_prefix(prefix.as_ref()).ok_or(SkillsFsError::InvalidPrefix)?);
        Ok(self)
    }

    pub fn from_object_storage_config(
        config: &ObjectStorageConfig,
        prefix: impl AsRef<str>,
    ) -> Result<Self, SkillsFsError> {
        let mut builder = S3::default()
            .bucket(&config.bucket)
            .region(&config.region)
            .disable_ec2_metadata();

        if let Some(endpoint) = config.endpoint.as_deref() {
            builder = builder.endpoint(endpoint);
        } else {
            return Err(SkillsFsError::MissingEndpoint);
        }
        if let Some(access_key) = config.access_key.as_deref() {
            builder = builder.access_key_id(access_key);
        } else {
            return Err(SkillsFsError::MissingAccessKey);
        }
        if let Some(secret_key) = config.secret_key.as_deref() {
            builder = builder.secret_access_key(secret_key);
        } else {
            return Err(SkillsFsError::MissingSecretKey);
        }
        if !config.force_path_style {
            builder = builder.enable_virtual_host_style();
        }

        let operator = Operator::new(builder)?.finish();
        Self::new(operator, prefix)
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<SkillsNode>, SkillsFsError> {
        // NFS 内部用空字符串表示根目录；对外的路径规范化仍拒绝空字符串，
        // 避免把用户传入的空路径误当作合法文件名。
        let path = normalize_skills_path(if path.is_empty() { "/" } else { path })
            .ok_or(SkillsFsError::InvalidPrefix)?;
        if path.is_empty() {
            let mut nodes_by_path = HashMap::new();
            let skills_node = self.node_for_path(MANAGED_SKILLS_DIR.to_string(), true, 0, None)?;
            nodes_by_path.insert(skills_node.path.clone(), skills_node);
            for node in self.root_profile_nodes().await? {
                nodes_by_path.insert(node.path.clone(), node);
            }
            let mut nodes = nodes_by_path.into_values().collect::<Vec<_>>();
            nodes.sort_by(|lhs, rhs| lhs.id.cmp(&rhs.id));
            return Ok(nodes);
        }

        let Some(storage_dir) = nfs_skills_storage_path(&path) else {
            return Ok(Vec::new());
        };
        let list_prefix = list_object_prefix(&self.prefix, &storage_dir);
        let mut entries = self.operator.list(&list_prefix).await?;
        entries.sort_by(|lhs, rhs| lhs.path().cmp(rhs.path()));

        let mut nodes_by_path = HashMap::new();
        for entry in entries {
            let Some(relative) = relative_entry_path(&self.prefix, entry.path()) else {
                continue;
            };
            if relative == storage_dir || relative.is_empty() || has_hidden_segment(&relative) {
                continue;
            }
            let Some((child_storage_path, is_virtual_dir)) =
                direct_child_path(&storage_dir, &relative)
            else {
                continue;
            };
            let is_dir = is_virtual_dir || entry.metadata().mode().is_dir();
            let child_path = skills_nfs_path(&child_storage_path);
            if is_dir && !self.directory_has_visible_entry(&child_path).await? {
                continue;
            }
            let node = if is_virtual_dir {
                // S3/RustFS 通常只有文件 key，没有显式目录对象；NFS 客户端需要能
                // 逐层进入目录，所以从子孙 key 合成中间目录。
                self.node_for_path(child_path, true, 0, None)?
            } else {
                let (entry_path, metadata) = entry.into_parts();
                self.node_for_entry_path(entry_path, metadata)?
            };
            nodes_by_path
                .entry(node.path.clone())
                .and_modify(|existing: &mut SkillsNode| {
                    if node.is_dir {
                        *existing = node.clone();
                    }
                })
                .or_insert(node);
        }
        let mut nodes = nodes_by_path.into_values().collect::<Vec<_>>();
        nodes.sort_by(|lhs, rhs| lhs.id.cmp(&rhs.id));
        Ok(nodes)
    }

    pub async fn read_path(&self, path: &str) -> Result<Vec<u8>, SkillsFsError> {
        let path = normalize_skills_path(path).ok_or(SkillsFsError::InvalidPrefix)?;
        if let Some(key) = self.profile_object_key(&path) {
            let bytes = self.operator.read(&key).await?;
            return Ok(bytes.to_vec());
        }
        let storage_path = nfs_skills_storage_file_path(&path).ok_or(SkillsFsError::NotFound)?;
        let bytes = self
            .operator
            .read(&object_key(&self.prefix, &storage_path))
            .await?;
        Ok(bytes.to_vec())
    }

    async fn write_path_at(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<SkillsNode, SkillsFsError> {
        let (path, storage_path) =
            writable_skills_storage_path(path).ok_or(SkillsFsError::InvalidPrefix)?;
        let mut bytes = match self.read_path(&path).await {
            Ok(bytes) => bytes,
            Err(SkillsFsError::Opendal(error)) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let start = usize::try_from(offset).map_err(|_| SkillsFsError::InvalidPrefix)?;
        if bytes.len() < start {
            bytes.resize(start, 0);
        }
        let end = start.saturating_add(data.len());
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[start..end].copy_from_slice(data);
        let key = object_key(&self.prefix, &storage_path);
        let metadata = self.operator.write(&key, bytes).await?;
        self.node_for_path(
            path,
            false,
            metadata.content_length(),
            metadata.last_modified().map(Into::into),
        )
    }

    async fn truncate_path(&self, path: &str, size: u64) -> Result<SkillsNode, SkillsFsError> {
        let (path, storage_path) =
            writable_skills_storage_path(path).ok_or(SkillsFsError::InvalidPrefix)?;
        let mut bytes = match self.read_path(&path).await {
            Ok(bytes) => bytes,
            Err(SkillsFsError::Opendal(error)) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let size = usize::try_from(size).map_err(|_| SkillsFsError::InvalidPrefix)?;
        bytes.resize(size, 0);
        let metadata = self
            .operator
            .write(&object_key(&self.prefix, &storage_path), bytes)
            .await?;
        self.node_for_path(
            path,
            false,
            metadata.content_length(),
            metadata.last_modified().map(Into::into),
        )
    }

    async fn create_empty_file(&self, path: &str) -> Result<SkillsNode, SkillsFsError> {
        self.write_path_at(path, 0, &[]).await
    }

    async fn create_directory(&self, path: &str) -> Result<SkillsNode, SkillsFsError> {
        let (path, storage_path) =
            writable_skills_storage_path(path).ok_or(SkillsFsError::InvalidPrefix)?;
        // NFS 新建的空目录需要跨进程重启可见；对象存储没有真实目录时，用目录对象持久化。
        self.operator
            .create_dir(&object_key(&self.prefix, &dir_path(&storage_path)))
            .await?;
        self.operator
            .write(
                &object_key(&self.prefix, &format!("{storage_path}/.hub-directory")),
                "",
            )
            .await?;
        self.node_for_path(path, true, 0, Some(SystemTime::now()))
    }

    async fn delete_path(&self, path: &str) -> Result<(), SkillsFsError> {
        let (path, storage_path) =
            writable_skills_storage_path(path).ok_or(SkillsFsError::InvalidPrefix)?;
        let node = match self.lookup_path(&path).await {
            Ok(node) => node,
            Err(nfsstat3::NFS3ERR_NOENT) => self
                .cached_node_for_path(&path)?
                .ok_or(SkillsFsError::NotFound)?,
            Err(_) => return Err(SkillsFsError::InvalidPrefix),
        };
        if node.is_dir {
            if !self.list_dir(&path).await?.is_empty() {
                return Err(SkillsFsError::DirectoryNotEmpty);
            }
            self.delete_object_if_exists(&format!("{storage_path}/.hub-directory"))
                .await?;
            self.delete_object_if_exists(&dir_path(&storage_path))
                .await?;
        } else {
            self.operator
                .delete(&object_key(&self.prefix, &storage_path))
                .await?;
        }
        self.remove_index_prefix(&path)?;
        Ok(())
    }

    async fn rename_path(&self, from: &str, to: &str) -> Result<(), SkillsFsError> {
        let (from, from_storage) =
            writable_skills_storage_path(from).ok_or(SkillsFsError::InvalidPrefix)?;
        let (_to, to_storage) =
            writable_skills_storage_path(to).ok_or(SkillsFsError::InvalidPrefix)?;
        let node = match self.lookup_path(&from).await {
            Ok(node) => node,
            Err(nfsstat3::NFS3ERR_NOENT) => self
                .cached_node_for_path(&from)?
                .ok_or(SkillsFsError::NotFound)?,
            Err(_) => return Err(SkillsFsError::InvalidPrefix),
        };
        if node.is_dir {
            let entries = self
                .operator
                .list(&object_key(&self.prefix, &dir_path(&from_storage)))
                .await?;
            for entry in entries {
                let Some(relative) = relative_entry_path(&self.prefix, entry.path()) else {
                    continue;
                };
                if relative == from_storage || !relative.starts_with(&format!("{from_storage}/")) {
                    continue;
                }
                let target_relative = format!("{to_storage}{}", &relative[from_storage.len()..]);
                if entry.metadata().mode().is_dir() {
                    self.operator
                        .create_dir(&object_key(&self.prefix, &dir_path(&target_relative)))
                        .await?;
                } else {
                    let bytes = self
                        .operator
                        .read(&object_key(&self.prefix, &relative))
                        .await?
                        .to_vec();
                    self.operator
                        .write(&object_key(&self.prefix, &target_relative), bytes)
                        .await?;
                    self.operator
                        .delete(&object_key(&self.prefix, &relative))
                        .await?;
                }
            }
            self.operator
                .delete(&object_key(&self.prefix, &dir_path(&from_storage)))
                .await?;
        } else {
            let bytes = self.read_path(&from).await?;
            self.operator
                .write(&object_key(&self.prefix, &to_storage), bytes)
                .await?;
            self.operator
                .delete(&object_key(&self.prefix, &from_storage))
                .await?;
        }
        self.remove_index_prefix(&from)?;
        Ok(())
    }

    async fn delete_object_if_exists(&self, path: &str) -> Result<(), SkillsFsError> {
        match self.operator.delete(&object_key(&self.prefix, path)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn cached_node_for_path(&self, path: &str) -> Result<Option<SkillsNode>, SkillsFsError> {
        let index = self.index.lock().map_err(|_| SkillsFsError::LockFailed)?;
        Ok(index
            .path_to_id
            .get(path)
            .and_then(|id| index.id_to_node.get(id))
            .cloned())
    }

    fn remove_index_prefix(&self, path: &str) -> Result<(), SkillsFsError> {
        let mut index = self.index.lock().map_err(|_| SkillsFsError::LockFailed)?;
        let removed_ids = index
            .path_to_id
            .iter()
            .filter_map(|(cached_path, id)| {
                (cached_path == path || cached_path.starts_with(&format!("{path}/"))).then_some(*id)
            })
            .collect::<Vec<_>>();
        index.path_to_id.retain(|cached_path, _| {
            cached_path != path && !cached_path.starts_with(&format!("{path}/"))
        });
        for id in removed_ids {
            index.id_to_node.remove(&id);
        }
        Ok(())
    }

    fn root_node(&self) -> SkillsNode {
        SkillsNode {
            id: ROOT_ID,
            path: String::new(),
            name: String::new(),
            is_dir: true,
            size: 0,
            modified: None,
        }
    }

    async fn lookup_path(&self, path: &str) -> Result<SkillsNode, nfsstat3> {
        let path = normalize_skills_path(path).ok_or(nfsstat3::NFS3ERR_NOENT)?;
        if path.is_empty() {
            return Ok(self.root_node());
        }
        if path == MANAGED_SKILLS_DIR {
            return self
                .node_for_path(MANAGED_SKILLS_DIR.to_string(), true, 0, None)
                .map_err(|_| nfsstat3::NFS3ERR_IO);
        }
        if has_hidden_segment(&path) {
            return Err(nfsstat3::NFS3ERR_NOENT);
        }

        if let Some(key) = self.profile_object_key(&path) {
            return match self.operator.stat(&key).await {
                Ok(metadata) if metadata.mode().is_file() => self
                    .node_for_path(
                        path,
                        false,
                        metadata.content_length(),
                        metadata.last_modified().map(Into::into),
                    )
                    .map_err(|_| nfsstat3::NFS3ERR_IO),
                Ok(_) => Err(nfsstat3::NFS3ERR_NOENT),
                Err(error) if error.kind() == ErrorKind::NotFound => Err(nfsstat3::NFS3ERR_NOENT),
                Err(_) => Err(nfsstat3::NFS3ERR_IO),
            };
        }

        let Some(storage_path) = nfs_skills_storage_path(&path) else {
            return Err(nfsstat3::NFS3ERR_NOENT);
        };
        let file_key = object_key(&self.prefix, &storage_path);
        match self.operator.stat(&file_key).await {
            Ok(metadata) if metadata.mode().is_file() => {
                return self
                    .node_for_path(
                        path,
                        false,
                        metadata.content_length(),
                        metadata.last_modified().map(Into::into),
                    )
                    .map_err(|_| nfsstat3::NFS3ERR_IO);
            }
            Ok(metadata) if metadata.mode().is_dir() => {
                return self
                    .node_for_path(path, true, 0, metadata.last_modified().map(Into::into))
                    .map_err(|_| nfsstat3::NFS3ERR_IO);
            }
            Ok(_) => return Err(nfsstat3::NFS3ERR_NOENT),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(nfsstat3::NFS3ERR_IO),
        }

        match self.directory_has_visible_entry(&path).await {
            Ok(true) => self
                .node_for_path(path, true, 0, None)
                .map_err(|_| nfsstat3::NFS3ERR_IO),
            Ok(false) => Err(nfsstat3::NFS3ERR_NOENT),
            Err(error) if error.kind() == ErrorKind::NotFound => Err(nfsstat3::NFS3ERR_NOENT),
            Err(_) => Err(nfsstat3::NFS3ERR_IO),
        }
    }

    async fn directory_has_visible_entry(&self, path: &str) -> Result<bool, opendal::Error> {
        if path.is_empty() || path == MANAGED_SKILLS_DIR {
            return Ok(true);
        }
        let Some(storage_path) = nfs_skills_storage_path(path) else {
            return Ok(false);
        };
        let dir_key = object_key(&self.prefix, &dir_path(&storage_path));
        let entries = self.operator.list(&dir_key).await?;
        Ok(entries
            .iter()
            .filter_map(|entry| visible_relative_entry(&self.prefix, entry))
            .any(|relative| {
                relative == storage_path || relative.starts_with(&format!("{storage_path}/"))
            }))
    }

    fn node_for_entry_path(
        &self,
        entry_path: String,
        metadata: Metadata,
    ) -> Result<SkillsNode, SkillsFsError> {
        let relative =
            relative_entry_path(&self.prefix, &entry_path).ok_or(SkillsFsError::InvalidPrefix)?;
        let path = skills_nfs_path(&relative);
        self.node_for_path(
            path,
            metadata.mode().is_dir(),
            metadata.content_length(),
            metadata.last_modified().map(Into::into),
        )
    }

    fn node_for_path(
        &self,
        path: String,
        is_dir: bool,
        size: u64,
        modified: Option<SystemTime>,
    ) -> Result<SkillsNode, SkillsFsError> {
        let path = normalize_skills_path(&path).ok_or(SkillsFsError::InvalidPrefix)?;
        let name = basename(&path).to_string();
        let id = stable_fileid_for_path(&path);
        let mut index = self.index.lock().map_err(|_| SkillsFsError::LockFailed)?;
        index.path_to_id.insert(path.clone(), id);

        let node = SkillsNode {
            id,
            path: path.clone(),
            name,
            is_dir,
            size: if is_dir { 0 } else { size },
            modified,
        };
        index.id_to_node.insert(id, node.clone());
        Ok(node)
    }

    async fn node_by_id(&self, id: fileid3) -> Result<SkillsNode, nfsstat3> {
        if id == ROOT_ID {
            return Ok(self.root_node());
        }
        let cached_node = {
            self.index
                .lock()
                .map_err(|_| nfsstat3::NFS3ERR_IO)?
                .id_to_node
                .get(&id)
                .cloned()
        };
        if let Some(node) = cached_node {
            if is_managed_profile_file(&node.path) {
                // SOUL.md 由 Hub 管理界面直接写对象存储；容器里的 NFS 旧 file handle
                // 必须重新 stat 对象，避免继续返回缓存的 size/mtime。
                return self.lookup_path(&node.path).await;
            }
            return Ok(node);
        }
        self.find_node_by_id(id)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .ok_or(nfsstat3::NFS3ERR_STALE)
    }

    async fn find_node_by_id(&self, id: fileid3) -> Result<Option<SkillsNode>, SkillsFsError> {
        if stable_fileid_for_path(MANAGED_SKILLS_DIR) == id {
            return self
                .node_for_path(MANAGED_SKILLS_DIR.to_string(), true, 0, None)
                .map(Some);
        }
        for node in self.root_profile_nodes().await? {
            if node.id == id {
                return Ok(Some(node));
            }
        }

        let mut pending_dirs = vec![String::new()];
        let mut visited_dirs = HashSet::new();

        while let Some(dir) = pending_dirs.pop() {
            if !visited_dirs.insert(dir.clone()) {
                continue;
            }
            let mut entries = self
                .operator
                .list(&list_object_prefix(&self.prefix, &dir))
                .await?;
            entries.sort_by(|lhs, rhs| lhs.path().cmp(rhs.path()));

            for entry in entries {
                let Some(relative) = relative_entry_path(&self.prefix, entry.path()) else {
                    continue;
                };
                if relative.is_empty() || has_hidden_segment(&relative) {
                    continue;
                }
                let Some((child_storage_path, is_virtual_dir)) = direct_child_path(&dir, &relative)
                else {
                    continue;
                };
                let is_dir = is_virtual_dir || entry.metadata().mode().is_dir();
                let child_path = skills_nfs_path(&child_storage_path);
                if is_dir && !self.directory_has_visible_entry(&child_path).await? {
                    continue;
                }

                if stable_fileid_for_path(&child_path) == id {
                    if is_dir {
                        return self.node_for_path(child_path, true, 0, None).map(Some);
                    }
                    let (entry_path, metadata) = entry.into_parts();
                    return self.node_for_entry_path(entry_path, metadata).map(Some);
                }

                if is_dir {
                    pending_dirs.push(child_storage_path);
                }
            }
        }

        Ok(None)
    }

    async fn root_profile_nodes(&self) -> Result<Vec<SkillsNode>, SkillsFsError> {
        let mut nodes = Vec::new();
        for file_name in MANAGED_PROFILE_FILES {
            let Some(key) = self.profile_object_key(file_name) else {
                continue;
            };
            match self.operator.stat(&key).await {
                Ok(metadata) if metadata.mode().is_file() => nodes.push(self.node_for_path(
                    file_name.to_string(),
                    false,
                    metadata.content_length(),
                    metadata.last_modified().map(Into::into),
                )?),
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(nodes)
    }

    fn profile_object_key(&self, path: &str) -> Option<String> {
        if !MANAGED_PROFILE_FILES.contains(&path) {
            return None;
        }
        let prefix = self.profile_prefix.as_deref()?;
        Some(object_key(prefix, path))
    }
}

#[async_trait]
impl NFSFileSystem for SkillsFs {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadWrite
    }

    fn root_dir(&self) -> fileid3 {
        ROOT_ID
    }

    fn id_to_fh(&self, id: fileid3) -> nfs_fh3 {
        // nfsserve 默认会把服务启动代号写进 file handle，进程重启后内核客户端会立刻
        // 收到 ESTALE。Hub 的 Skill 挂载是长期只读挂载，因此这里使用稳定 handle。
        nfs_fh3 {
            data: id.to_le_bytes().to_vec(),
        }
    }

    fn fh_to_id(&self, handle: &nfs_fh3) -> Result<fileid3, nfsstat3> {
        let id = match handle.data.len() {
            8 => fileid3::from_le_bytes(
                handle.data[0..8]
                    .try_into()
                    .map_err(|_| nfsstat3::NFS3ERR_BADHANDLE)?,
            ),
            16 => {
                // 兼容已经挂载在旧版本 nfsserve 默认 handle 上的客户端：忽略旧的启动代号，
                // 只取后 8 字节 fileid，避免升级后仍必须人工重新挂载。
                fileid3::from_le_bytes(
                    handle.data[8..16]
                        .try_into()
                        .map_err(|_| nfsstat3::NFS3ERR_BADHANDLE)?,
                )
            }
            _ => return Err(nfsstat3::NFS3ERR_BADHANDLE),
        };
        if id == 0 {
            Err(nfsstat3::NFS3ERR_BADHANDLE)
        } else {
            Ok(id)
        }
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let dir = self.node_by_id(dirid).await?;
        if !dir.is_dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let filename =
            std::str::from_utf8(filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        if filename.is_empty() || filename.contains('/') {
            return Err(nfsstat3::NFS3ERR_NOENT);
        }
        let path = if dir.path.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", dir.path, filename)
        };
        Ok(self.lookup_path(&path).await?.id)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let node = self.node_by_id(id).await?;
        Ok(fattr_for_node(&node))
    }

    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3> {
        let node = self.node_by_id(id).await?;
        if let set_size3::size(size) = setattr.size {
            if node.is_dir {
                return Err(nfsstat3::NFS3ERR_ISDIR);
            }
            return self
                .truncate_path(&node.path, size)
                .await
                .map(|node| fattr_for_node(&node))
                .map_err(skills_error_to_nfs);
        }
        Ok(fattr_for_node(&node))
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let node = self.node_by_id(id).await?;
        if node.is_dir {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        let bytes = self
            .read_path(&node.path)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let start = usize::try_from(offset).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        if start >= bytes.len() {
            return Ok((Vec::new(), true));
        }
        let count = usize::try_from(count).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        let end = bytes.len().min(start.saturating_add(count));
        Ok((bytes[start..end].to_vec(), end >= bytes.len()))
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        let node = self.node_by_id(id).await?;
        if node.is_dir {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        self.write_path_at(&node.path, offset, data)
            .await
            .map(|node| fattr_for_node(&node))
            .map_err(skills_error_to_nfs)
    }

    async fn create(
        &self,
        dirid: fileid3,
        filename: &filename3,
        attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let dir = self.node_by_id(dirid).await?;
        if !dir.is_dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let path = child_path_for_nfs_name(&dir.path, filename)?;
        let mut node = self
            .create_empty_file(&path)
            .await
            .map_err(skills_error_to_nfs)?;
        if let set_size3::size(size) = attr.size {
            node = self
                .truncate_path(&path, size)
                .await
                .map_err(skills_error_to_nfs)?;
        }
        Ok((node.id, fattr_for_node(&node)))
    }

    async fn create_exclusive(
        &self,
        dirid: fileid3,
        filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        let dir = self.node_by_id(dirid).await?;
        let path = child_path_for_nfs_name(&dir.path, filename)?;
        if self.lookup_path(&path).await.is_ok() {
            return Err(nfsstat3::NFS3ERR_EXIST);
        }
        let (id, _) = self.create(dirid, filename, sattr3::default()).await?;
        Ok(id)
    }

    async fn mkdir(
        &self,
        dirid: fileid3,
        dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let dir = self.node_by_id(dirid).await?;
        if !dir.is_dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let path = child_path_for_nfs_name(&dir.path, dirname)?;
        let node = self
            .create_directory(&path)
            .await
            .map_err(skills_error_to_nfs)?;
        Ok((node.id, fattr_for_node(&node)))
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        let dir = self.node_by_id(dirid).await?;
        if !dir.is_dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let path = child_path_for_nfs_name(&dir.path, filename)?;
        self.delete_path(&path).await.map_err(skills_error_to_nfs)
    }

    async fn rename(
        &self,
        from_dirid: fileid3,
        from_filename: &filename3,
        to_dirid: fileid3,
        to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        let from_dir = self.node_by_id(from_dirid).await?;
        let to_dir = self.node_by_id(to_dirid).await?;
        if !from_dir.is_dir || !to_dir.is_dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let from = child_path_for_nfs_name(&from_dir.path, from_filename)?;
        let to = child_path_for_nfs_name(&to_dir.path, to_filename)?;
        self.rename_path(&from, &to)
            .await
            .map_err(skills_error_to_nfs)
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let dir = self.node_by_id(dirid).await?;
        if !dir.is_dir {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let mut nodes = self
            .list_dir(&dir.path)
            .await
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;
        nodes.retain(|node| node.id > start_after);
        nodes.sort_by(|lhs, rhs| lhs.id.cmp(&rhs.id));
        let end = nodes.len() <= max_entries;
        nodes.truncate(max_entries);
        Ok(ReadDirResult {
            entries: nodes
                .into_iter()
                .map(|node| nfsserve::vfs::DirEntry {
                    fileid: node.id,
                    name: node.name.as_bytes().into(),
                    attr: fattr_for_node(&node),
                })
                .collect(),
            end,
        })
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readlink(&self, _id: fileid3) -> Result<nfspath3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_INVAL)
    }

    async fn fsinfo(&self, root_fileid: fileid3) -> Result<fsinfo3, nfsstat3> {
        let obj_attributes = self
            .getattr(root_fileid)
            .await
            .map(post_op_attr::attributes)
            .unwrap_or(post_op_attr::Void);
        Ok(fsinfo3 {
            obj_attributes,
            rtmax: 1024 * 1024,
            rtpref: 1024 * 124,
            rtmult: 1024 * 1024,
            wtmax: 1024 * 1024,
            wtpref: 1024 * 1024,
            wtmult: 1024 * 1024,
            dtpref: 1024 * 1024,
            maxfilesize: 128 * 1024 * 1024 * 1024,
            time_delta: nfstime3 {
                seconds: 0,
                nseconds: 0,
            },
            properties: FSF_HOMOGENEOUS,
        })
    }
}

pub type ReadonlySkillsFs = SkillsFs;

pub fn normalize_skills_path(path: &str) -> Option<String> {
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    if path == "/" {
        return Some(String::new());
    }
    if path.starts_with('/') {
        return None;
    }

    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => {
                let segment = segment.to_str()?;
                if segment.is_empty() {
                    return None;
                }
                parts.push(segment.to_string());
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

fn normalize_prefix(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() {
        return Some(String::new());
    }
    normalize_skills_path(trimmed)
}

fn object_key(prefix: &str, path: &str) -> String {
    match (prefix.is_empty(), path.is_empty()) {
        (true, true) => String::new(),
        (true, false) => path.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}/{path}"),
    }
}

fn list_object_prefix(prefix: &str, path: &str) -> String {
    if path.is_empty() && !prefix.is_empty() {
        format!("{prefix}/")
    } else {
        object_key(prefix, &dir_path(path))
    }
}

fn dir_path(path: &str) -> String {
    if path.is_empty() || path.ends_with('/') {
        path.to_string()
    } else {
        format!("{path}/")
    }
}

fn relative_entry_path(prefix: &str, entry_path: &str) -> Option<String> {
    let entry_path = entry_path.trim_matches('/');
    let relative = if prefix.is_empty() {
        entry_path
    } else if entry_path == prefix {
        ""
    } else {
        entry_path.strip_prefix(&format!("{prefix}/"))?
    };
    let normalized = normalize_skills_path(if relative.is_empty() { "/" } else { relative })?;
    Some(normalized)
}

fn visible_relative_entry(prefix: &str, entry: &Entry) -> Option<String> {
    let relative = relative_entry_path(prefix, entry.path())?;
    if relative.is_empty() || has_hidden_segment(&relative) {
        return None;
    }
    Some(relative)
}

fn direct_child_path(parent: &str, child: &str) -> Option<(String, bool)> {
    let descendant = if parent.is_empty() {
        child
    } else {
        child.strip_prefix(&format!("{parent}/"))?
    };
    if descendant.is_empty() {
        return None;
    }
    let mut parts = descendant.split('/');
    let child_name = parts.next()?.trim_end_matches('/');
    if child_name.is_empty() {
        return None;
    }
    let child_path = if parent.is_empty() {
        child_name.to_string()
    } else {
        format!("{parent}/{child_name}")
    };
    Some((child_path, parts.next().is_some()))
}

fn basename(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches('/')
}

fn has_hidden_segment(path: &str) -> bool {
    path.split('/')
        .any(|segment| HIDDEN_SEGMENTS.contains(&segment))
}

fn nfs_skills_storage_path(path: &str) -> Option<String> {
    let path = normalize_skills_path(path)?;
    if path == MANAGED_SKILLS_DIR || path.starts_with(&format!("{MANAGED_SKILLS_DIR}/")) {
        path.strip_prefix(MANAGED_SKILLS_DIR)
            .map(|value| value.trim_start_matches('/').to_string())
    } else {
        None
    }
}

fn nfs_skills_storage_file_path(path: &str) -> Option<String> {
    let storage_path = nfs_skills_storage_path(path)?;
    (storage_path != MANAGED_SKILLS_DIR).then_some(storage_path)
}

fn writable_skills_storage_path(path: &str) -> Option<(String, String)> {
    let path = writable_skills_path(path)?;
    let storage_path = nfs_skills_storage_file_path(&path)?;
    Some((path, storage_path))
}

fn writable_skills_path(path: &str) -> Option<String> {
    let path = normalize_skills_path(path)?;
    if !path.starts_with(&format!("{MANAGED_SKILLS_DIR}/"))
        || has_hidden_segment(&path)
        || is_managed_profile_file(&path)
    {
        None
    } else {
        Some(path)
    }
}

fn is_managed_profile_file(path: &str) -> bool {
    MANAGED_PROFILE_FILES.contains(&path)
}

fn skills_nfs_path(storage_path: &str) -> String {
    if storage_path.is_empty() {
        MANAGED_SKILLS_DIR.to_string()
    } else {
        format!("{MANAGED_SKILLS_DIR}/{storage_path}")
    }
}

fn child_path_for_nfs_name(parent: &str, filename: &filename3) -> Result<String, nfsstat3> {
    let filename = std::str::from_utf8(filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
    if filename.is_empty() || filename.contains('/') {
        return Err(nfsstat3::NFS3ERR_INVAL);
    }
    let path = if parent.is_empty() {
        filename.to_string()
    } else {
        format!("{parent}/{filename}")
    };
    writable_skills_path(&path).ok_or(nfsstat3::NFS3ERR_ACCES)
}

fn skills_error_to_nfs(error: SkillsFsError) -> nfsstat3 {
    match error {
        SkillsFsError::InvalidPrefix => nfsstat3::NFS3ERR_ACCES,
        SkillsFsError::Opendal(error) if error.kind() == ErrorKind::NotFound => {
            nfsstat3::NFS3ERR_NOENT
        }
        SkillsFsError::DirectoryNotEmpty => nfsstat3::NFS3ERR_NOTEMPTY,
        SkillsFsError::NotFound => nfsstat3::NFS3ERR_NOENT,
        _ => nfsstat3::NFS3ERR_IO,
    }
}

fn stable_fileid_for_path(path: &str) -> fileid3 {
    if path.is_empty() {
        return ROOT_ID;
    }
    let digest = Sha256::digest(path.as_bytes());
    let mut id = fileid3::from_le_bytes(digest[0..8].try_into().expect("sha256 has 32 bytes"));
    id &= 0x7fff_ffff_ffff_ffff;
    if id <= ROOT_ID {
        id + ROOT_ID + 1
    } else {
        id
    }
}

fn fattr_for_node(node: &SkillsNode) -> fattr3 {
    let timestamp = nfs_time(node.modified);
    let size = if node.is_dir { 0 } else { node.size };
    fattr3 {
        ftype: if node.is_dir {
            ftype3::NF3DIR
        } else {
            ftype3::NF3REG
        },
        mode: if node.is_dir { DIR_MODE } else { FILE_MODE },
        nlink: if node.is_dir { 2 } else { 1 },
        uid: 0 as uid3,
        gid: 0 as gid3,
        size,
        used: size,
        rdev: specdata3::default(),
        fsid: FS_ID,
        fileid: node.id,
        atime: timestamp,
        mtime: timestamp,
        ctime: timestamp,
    }
}

fn nfs_time(time: Option<SystemTime>) -> nfstime3 {
    let duration = time
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    nfstime3 {
        seconds: duration.as_secs().min(u32::MAX as u64) as u32,
        nseconds: duration.subsec_nanos(),
    }
}
