use hermes_hub_backend::skills_fs::{normalize_skills_path, ReadonlySkillsFs};
use nfsserve::{
    nfs::{ftype3, nfs_fh3, nfsstat3, FSF_CANSETTIME, FSF_SYMLINK},
    vfs::{NFSFileSystem, VFSCapabilities},
};
use opendal::{services::Memory, Operator};

async fn test_operator() -> Operator {
    let op = Operator::new(Memory::default())
        .expect("memory operator can be created")
        .finish();
    op.write("managed-skills/current/writing/SKILL.md", "# Writing\n")
        .await
        .expect("fixture can be written");
    op.write("managed-skills/current/coding/tools.md", "cargo test\n")
        .await
        .expect("fixture can be written");
    op.write("managed-skills/current/.curator_state/state.json", "{}")
        .await
        .expect("hidden curator state fixture can be written");
    op.write("managed-skills/current/.bundled_manifest", "[]")
        .await
        .expect("hidden manifest fixture can be written");
    op.write(
        "managed-skills/current/hidden-only/.curator_state/state.json",
        "{}",
    )
    .await
    .expect("hidden-only parent fixture can be written");
    op.create_dir("managed-skills/current/empty-dir/")
        .await
        .expect("empty directory marker fixture can be written");
    op
}

#[test]
fn normalize_skills_path_rejects_escape_paths() {
    assert_eq!(normalize_skills_path("/"), Some(String::new()));
    assert_eq!(
        normalize_skills_path("writing/SKILL.md"),
        Some("writing/SKILL.md".to_string())
    );
    assert_eq!(
        normalize_skills_path("./writing//SKILL.md"),
        Some("writing/SKILL.md".to_string())
    );

    for path in [
        "",
        "../secret",
        "writing/../../secret",
        "/absolute",
        "writing/..",
        "a/./../b",
        "a\0b",
    ] {
        assert!(
            normalize_skills_path(path).is_none(),
            "path {path:?} must not be accepted"
        );
    }
}

#[tokio::test]
async fn readonly_skills_fs_lists_and_reads_from_prefix() {
    let fs = ReadonlySkillsFs::new(test_operator().await, "managed-skills/current")
        .expect("fs can be created");

    assert!(matches!(fs.capabilities(), VFSCapabilities::ReadOnly));
    let root_id = fs.root_dir();
    let root = fs.getattr(root_id).await.expect("root has attributes");
    assert_eq!(root.ftype as u32, ftype3::NF3DIR as u32);
    let root_entries = fs
        .readdir(root_id, 0, 16)
        .await
        .expect("root directory can be listed");
    assert!(
        root_entries
            .entries
            .iter()
            .any(|entry| entry.name.as_ref() == b"writing"),
        "NFS clients must be able to discover top-level skill directories"
    );
    assert!(
        root_entries.entries.iter().all(|entry| {
            !matches!(
                entry.name.as_ref(),
                b".curator_state" | b".bundled_manifest"
            )
        }),
        "Hub curator metadata must not be visible through the managed skills mount"
    );
    assert!(
        root_entries
            .entries
            .iter()
            .all(|entry| entry.name.as_ref() != b"hidden-only"),
        "directories backed only by hidden curator metadata must not leak through NFS"
    );
    assert!(matches!(
        fs.lookup(root_id, &b".curator_state".as_slice().into())
            .await,
        Err(nfsstat3::NFS3ERR_NOENT)
    ));
    assert!(matches!(
        fs.lookup(root_id, &b"hidden-only".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_NOENT)
    ));

    let empty_id = fs
        .lookup(root_id, &b"empty-dir".as_slice().into())
        .await
        .expect("explicit empty directory marker can be looked up");
    let empty_entries = fs
        .readdir(empty_id, 0, 16)
        .await
        .expect("empty directory marker can be listed");
    assert!(
        empty_entries.entries.is_empty(),
        "empty directory marker should not synthesize bogus children"
    );

    let writing_id = fs
        .lookup(root_id, &b"writing".as_slice().into())
        .await
        .expect("writing dir can be looked up");
    let skill_id = fs
        .lookup(writing_id, &b"SKILL.md".as_slice().into())
        .await
        .expect("skill file can be looked up");
    let skill_attr = fs.getattr(skill_id).await.expect("skill has attributes");
    assert_eq!(skill_attr.ftype as u32, ftype3::NF3REG as u32);
    assert_eq!(skill_attr.size, "# Writing\n".len() as u64);

    let (bytes, eof) = fs.read(skill_id, 2, 7).await.expect("file can be read");
    assert_eq!(bytes, b"Writing");
    assert_eq!(eof, false);

    let (bytes, eof) = fs.read(skill_id, 0, 1024).await.expect("file can be read");
    assert_eq!(bytes, b"# Writing\n");
    assert_eq!(eof, true);

    let fsinfo = fs.fsinfo(root_id).await.expect("fsinfo can be read");
    assert_eq!(
        fsinfo.properties & (FSF_SYMLINK | FSF_CANSETTIME),
        0,
        "readonly skills NFS must not advertise symlink or settime support"
    );
}

#[tokio::test]
async fn readonly_skills_fs_rejects_writes() {
    let fs = ReadonlySkillsFs::new(test_operator().await, "managed-skills/current")
        .expect("fs can be created");
    let root_id = fs.root_dir();

    assert!(matches!(
        fs.mkdir(root_id, &b"new".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_ROFS)
    ));
    assert!(matches!(
        fs.create(root_id, &b"new.md".as_slice().into(), Default::default())
            .await,
        Err(nfsstat3::NFS3ERR_ROFS)
    ));
    assert!(matches!(
        fs.remove(root_id, &b"writing".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_ROFS)
    ));
}

#[tokio::test]
async fn readonly_skills_fs_file_handles_survive_server_restart() {
    let operator = test_operator().await;
    let fs = ReadonlySkillsFs::new(operator.clone(), "managed-skills/current")
        .expect("fs can be created");
    let root_id = fs.root_dir();
    let writing_id = fs
        .lookup(root_id, &b"writing".as_slice().into())
        .await
        .expect("writing dir can be looked up");
    let skill_id = fs
        .lookup(writing_id, &b"SKILL.md".as_slice().into())
        .await
        .expect("skill file can be looked up");
    let skill_handle = fs.id_to_fh(skill_id);

    let restarted = ReadonlySkillsFs::new(operator, "managed-skills/current")
        .expect("restarted fs can be created");
    let restored_id = restarted
        .fh_to_id(&skill_handle)
        .expect("stable handle can be decoded after restart");
    let (bytes, eof) = restarted
        .read(restored_id, 0, 1024)
        .await
        .expect("stable handle can be read after restart");
    assert_eq!(bytes, b"# Writing\n");
    assert!(eof);

    let mut legacy_handle = Vec::new();
    legacy_handle.extend_from_slice(&123_u64.to_le_bytes());
    legacy_handle.extend_from_slice(&root_id.to_le_bytes());
    let decoded_legacy_root = restarted
        .fh_to_id(&nfs_fh3 {
            data: legacy_handle,
        })
        .expect("旧版 nfsserve 默认 handle 也要兼容，否则升级后仍会要求手动重新挂载");
    assert_eq!(decoded_legacy_root, root_id);
}
