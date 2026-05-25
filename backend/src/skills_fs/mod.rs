use std::{
    collections::HashMap,
    path::{Component, Path},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use nfsserve::{
    nfs::{
        fattr3, fileid3, filename3, fsinfo3, ftype3, gid3, nfspath3, nfsstat3, nfstime3,
        post_op_attr, sattr3, specdata3, uid3, FSF_HOMOGENEOUS,
    },
    vfs::{NFSFileSystem, ReadDirResult, VFSCapabilities},
};
use opendal::{services::S3, Entry, ErrorKind, Metadata, Operator};
use thiserror::Error;

use crate::app_config::ObjectStorageConfig;

const ROOT_ID: fileid3 = 1;
const FS_ID: u64 = 0x4848_534b_494c_4c53;
const DIR_MODE: u32 = 0o555;
const FILE_MODE: u32 = 0o444;
const HIDDEN_SEGMENTS: [&str; 2] = [".curator_state", ".bundled_manifest"];

#[derive(Debug, Error)]
pub enum SkillsFsError {
    #[error("invalid skills filesystem prefix")]
    InvalidPrefix,
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
    next_id: fileid3,
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
            next_id: ROOT_ID + 1,
            path_to_id,
            id_to_node,
        }
    }
}

/// OpenDAL 负责实时读取 S3/RustFS；这里的索引只保存 NFS fileid 与路径映射。
pub struct ReadonlySkillsFs {
    operator: Operator,
    prefix: String,
    index: Mutex<SkillsFsIndex>,
}

impl ReadonlySkillsFs {
    pub fn new(operator: Operator, prefix: impl AsRef<str>) -> Result<Self, SkillsFsError> {
        let prefix = normalize_prefix(prefix.as_ref()).ok_or(SkillsFsError::InvalidPrefix)?;
        Ok(Self {
            operator,
            prefix,
            index: Mutex::new(SkillsFsIndex::default()),
        })
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
        let list_prefix = list_object_prefix(&self.prefix, &path);
        let mut entries = self.operator.list(&list_prefix).await?;
        entries.sort_by(|lhs, rhs| lhs.path().cmp(rhs.path()));

        let mut nodes_by_path = HashMap::new();
        for entry in entries {
            let Some(relative) = relative_entry_path(&self.prefix, entry.path()) else {
                continue;
            };
            if relative == path || relative.is_empty() || has_hidden_segment(&relative) {
                continue;
            }
            let Some((child_path, is_virtual_dir)) = direct_child_path(&path, &relative) else {
                continue;
            };
            let is_dir = is_virtual_dir || entry.metadata().mode().is_dir();
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
        let bytes = self.operator.read(&object_key(&self.prefix, &path)).await?;
        Ok(bytes.to_vec())
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
        if has_hidden_segment(&path) {
            return Err(nfsstat3::NFS3ERR_NOENT);
        }

        let file_key = object_key(&self.prefix, &path);
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
        let dir_key = object_key(&self.prefix, &dir_path(path));
        let entries = self.operator.list(&dir_key).await?;
        Ok(entries
            .iter()
            .filter_map(|entry| visible_relative_entry(&self.prefix, entry))
            .any(|relative| relative == path || relative.starts_with(&format!("{path}/"))))
    }

    fn node_for_entry_path(
        &self,
        entry_path: String,
        metadata: Metadata,
    ) -> Result<SkillsNode, SkillsFsError> {
        let relative =
            relative_entry_path(&self.prefix, &entry_path).ok_or(SkillsFsError::InvalidPrefix)?;
        self.node_for_path(
            relative,
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
        let mut index = self.index.lock().map_err(|_| SkillsFsError::LockFailed)?;
        let id = if let Some(id) = index.path_to_id.get(&path).copied() {
            id
        } else {
            let id = index.next_id;
            index.next_id += 1;
            index.path_to_id.insert(path.clone(), id);
            id
        };

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

    fn node_by_id(&self, id: fileid3) -> Result<SkillsNode, nfsstat3> {
        self.index
            .lock()
            .map_err(|_| nfsstat3::NFS3ERR_IO)?
            .id_to_node
            .get(&id)
            .cloned()
            .ok_or(nfsstat3::NFS3ERR_STALE)
    }
}

#[async_trait]
impl NFSFileSystem for ReadonlySkillsFs {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        ROOT_ID
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let dir = self.node_by_id(dirid)?;
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
        let node = self.node_by_id(id)?;
        Ok(fattr_for_node(&node))
    }

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let node = self.node_by_id(id)?;
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

    async fn write(&self, _id: fileid3, _offset: u64, _data: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
        _attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(
        &self,
        _dirid: fileid3,
        _dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(&self, _dirid: fileid3, _filename: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(
        &self,
        _from_dirid: fileid3,
        _from_filename: &filename3,
        _to_dirid: fileid3,
        _to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let dir = self.node_by_id(dirid)?;
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
            wtmax: 0,
            wtpref: 0,
            wtmult: 0,
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
