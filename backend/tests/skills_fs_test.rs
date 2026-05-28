use hermes_hub_backend::skills_fs::{normalize_skills_path, SkillsFs};
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
    op.write("managed-skills/current/hub-empty/.hub-directory", "")
        .await
        .expect("hub empty directory marker fixture can be written");
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
async fn skills_fs_lists_and_reads_from_prefix() {
    let fs =
        SkillsFs::new(test_operator().await, "managed-skills/current").expect("fs can be created");

    assert!(matches!(fs.capabilities(), VFSCapabilities::ReadWrite));
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
            .any(|entry| entry.name.as_ref() == b"skills"),
        "NFS root must expose the managed skills directory"
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
    let skills_id = fs
        .lookup(root_id, &b"skills".as_slice().into())
        .await
        .expect("skills dir can be looked up");
    let skills_entries = fs
        .readdir(skills_id, 0, 16)
        .await
        .expect("skills directory can be listed");
    assert!(
        skills_entries
            .entries
            .iter()
            .any(|entry| entry.name.as_ref() == b"writing"),
        "NFS clients must be able to discover skill directories under /skills"
    );
    assert!(
        skills_entries.entries.iter().all(|entry| {
            !matches!(
                entry.name.as_ref(),
                b".curator_state" | b".bundled_manifest"
            )
        }),
        "Hub curator metadata must not be visible through the managed skills directory"
    );
    assert!(
        skills_entries
            .entries
            .iter()
            .all(|entry| entry.name.as_ref() != b"hidden-only"),
        "directories backed only by hidden curator metadata must not leak through NFS"
    );
    assert!(matches!(
        fs.lookup(skills_id, &b".curator_state".as_slice().into())
            .await,
        Err(nfsstat3::NFS3ERR_NOENT)
    ));
    assert!(matches!(
        fs.lookup(skills_id, &b"hidden-only".as_slice().into())
            .await,
        Err(nfsstat3::NFS3ERR_NOENT)
    ));
    assert!(matches!(
        fs.lookup(skills_id, &b"hub-empty".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_NOENT)
    ));

    let empty_id = fs
        .lookup(skills_id, &b"empty-dir".as_slice().into())
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
        .lookup(skills_id, &b"writing".as_slice().into())
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
        "managed skills NFS must not advertise symlink or settime support"
    );
    assert!(fsinfo.wtmax > 0, "managed skills NFS must advertise writes");
    assert!(
        fsinfo.wtpref > 0,
        "managed skills NFS must advertise writes"
    );
    assert!(
        fsinfo.wtmult > 0,
        "managed skills NFS must advertise writes"
    );
}

#[tokio::test]
async fn skills_fs_exposes_managed_profile_files_and_skills_directory_at_root() {
    let operator = test_operator().await;
    operator
        .write("managed-profile/current/AGENTS.md", "# Agents\n")
        .await
        .expect("AGENTS.md fixture can be written");
    operator
        .write("managed-profile/current/SOUL.md", "# Soul\n")
        .await
        .expect("SOUL.md fixture can be written");
    let fs = SkillsFs::new(operator.clone(), "managed-skills/current")
        .expect("fs can be created")
        .with_profile_prefix("managed-profile/current")
        .expect("profile prefix is valid");
    let root_id = fs.root_dir();

    let root_entries = fs
        .readdir(root_id, 0, 16)
        .await
        .expect("root directory can be listed");
    assert!(
        root_entries
            .entries
            .iter()
            .any(|entry| entry.name.as_ref() == b"skills"),
        "Hub FS root must include the managed skills directory"
    );
    assert!(
        root_entries
            .entries
            .iter()
            .all(|entry| entry.name.as_ref() != b"AGENTS.md"),
        "Hub FS root must not expose managed AGENTS.md anymore"
    );
    assert!(
        root_entries
            .entries
            .iter()
            .any(|entry| entry.name.as_ref() == b"SOUL.md"),
        "Hub FS root must include managed SOUL.md"
    );

    assert!(matches!(
        fs.lookup(root_id, &b"AGENTS.md".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_NOENT)
    ));

    let soul_id = fs
        .lookup(root_id, &b"SOUL.md".as_slice().into())
        .await
        .expect("managed SOUL.md can be looked up");
    let (bytes, eof) = fs
        .read(soul_id, 0, 1024)
        .await
        .expect("managed SOUL.md can be read");
    assert_eq!(bytes, b"# Soul\n");
    assert!(eof);

    assert!(matches!(
        fs.write(soul_id, 0, b"changed").await,
        Err(nfsstat3::NFS3ERR_ACCES)
    ));
    assert!(matches!(
        fs.setattr(
            soul_id,
            nfsserve::nfs::sattr3 {
                size: nfsserve::nfs::set_size3::size(0),
                ..Default::default()
            },
        )
        .await,
        Err(nfsstat3::NFS3ERR_ACCES)
    ));
    assert!(matches!(
        fs.remove(root_id, &b"SOUL.md".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_ACCES)
    ));
    assert!(matches!(
        fs.rename(
            root_id,
            &b"SOUL.md".as_slice().into(),
            root_id,
            &b"SOUL.old.md".as_slice().into(),
        )
        .await,
        Err(nfsstat3::NFS3ERR_ACCES)
    ));
    assert_eq!(
        operator
            .read("managed-profile/current/SOUL.md")
            .await
            .expect("managed SOUL.md remains untouched")
            .to_vec(),
        b"# Soul\n"
    );
}

#[tokio::test]
async fn skills_fs_refreshes_managed_profile_file_attributes_after_object_update() {
    let operator = test_operator().await;
    operator
        .write("managed-profile/current/SOUL.md", "old")
        .await
        .expect("SOUL.md fixture can be written");
    let fs = SkillsFs::new(operator.clone(), "managed-skills/current")
        .expect("fs can be created")
        .with_profile_prefix("managed-profile/current")
        .expect("profile prefix is valid");
    let root_id = fs.root_dir();

    let soul_id = fs
        .lookup(root_id, &b"SOUL.md".as_slice().into())
        .await
        .expect("managed SOUL.md can be looked up");
    assert_eq!(
        fs.getattr(soul_id)
            .await
            .expect("initial attr can be read")
            .size,
        3
    );

    operator
        .write("managed-profile/current/SOUL.md", "new managed soul")
        .await
        .expect("SOUL.md can be updated outside NFS");

    let refreshed_attr = fs
        .getattr(soul_id)
        .await
        .expect("updated attr can be read through the old file handle");
    assert_eq!(refreshed_attr.size, "new managed soul".len() as u64);
    let (bytes, eof) = fs
        .read(soul_id, 0, 1024)
        .await
        .expect("updated SOUL.md can be read through the old file handle");
    assert_eq!(bytes, b"new managed soul");
    assert!(eof);
}

#[tokio::test]
async fn skills_fs_writes_global_skills_back_to_object_storage() {
    let operator = test_operator().await;
    let fs = SkillsFs::new(operator.clone(), "managed-skills/current").expect("fs can be created");
    let root_id = fs.root_dir();
    let skills_id = fs
        .lookup(root_id, &b"skills".as_slice().into())
        .await
        .expect("skills dir can be looked up");

    assert!(matches!(fs.capabilities(), VFSCapabilities::ReadWrite));
    let (draft_id, draft_attr) = fs
        .create(
            skills_id,
            &b"draft.md".as_slice().into(),
            Default::default(),
        )
        .await
        .expect("admin Hermes can create a global skill file");
    assert_eq!(draft_attr.size, 0);
    let written_attr = fs
        .write(draft_id, 0, b"# Draft\n")
        .await
        .expect("admin Hermes can write a global skill file");
    assert_eq!(written_attr.size, "# Draft\n".len() as u64);
    assert_eq!(
        operator
            .read("managed-skills/current/draft.md")
            .await
            .expect("written skill can be read from object storage")
            .to_vec(),
        b"# Draft\n"
    );

    let docs_id = fs
        .mkdir(skills_id, &b"docs".as_slice().into())
        .await
        .expect("admin Hermes can create a global skill directory")
        .0;
    let (guide_id, _) = fs
        .create(docs_id, &b"guide.md".as_slice().into(), Default::default())
        .await
        .expect("file in created directory can be created");
    fs.write(guide_id, 0, b"guide")
        .await
        .expect("file can be written");
    fs.rename(
        docs_id,
        &b"guide.md".as_slice().into(),
        docs_id,
        &b"README.md".as_slice().into(),
    )
    .await
    .expect("admin Hermes can rename a global skill file");
    assert_eq!(
        operator
            .read("managed-skills/current/docs/README.md")
            .await
            .expect("renamed skill can be read")
            .to_vec(),
        b"guide"
    );
    assert!(matches!(
        operator.stat("managed-skills/current/docs/guide.md").await,
        Err(error) if error.kind() == opendal::ErrorKind::NotFound
    ));

    fs.remove(skills_id, &b"draft.md".as_slice().into())
        .await
        .expect("admin Hermes can remove a global skill file");
    assert!(matches!(
        operator.stat("managed-skills/current/draft.md").await,
        Err(error) if error.kind() == opendal::ErrorKind::NotFound
    ));

    assert!(matches!(
        fs.create(
            skills_id,
            &b".curator_state".as_slice().into(),
            Default::default()
        )
        .await,
        Err(nfsstat3::NFS3ERR_ACCES)
    ));
}

#[tokio::test]
async fn skills_fs_write_from_offset_zero_preserves_existing_tail() {
    let operator = test_operator().await;
    operator
        .write("managed-skills/current/notes.md", "hello world")
        .await
        .expect("fixture can be written");
    let fs = SkillsFs::new(operator.clone(), "managed-skills/current").expect("fs can be created");
    let root_id = fs.root_dir();
    let skills_id = fs
        .lookup(root_id, &b"skills".as_slice().into())
        .await
        .expect("skills dir can be looked up");
    let notes_id = fs
        .lookup(skills_id, &b"notes.md".as_slice().into())
        .await
        .expect("notes file can be looked up");

    fs.write(notes_id, 0, b"hi")
        .await
        .expect("offset zero write should update prefix only");

    assert_eq!(
        operator
            .read("managed-skills/current/notes.md")
            .await
            .expect("updated file can be read")
            .to_vec(),
        b"hillo world"
    );
}

#[tokio::test]
async fn skills_fs_create_exclusive_rejects_existing_file() {
    let operator = test_operator().await;
    let fs = SkillsFs::new(operator.clone(), "managed-skills/current").expect("fs can be created");
    let root_id = fs.root_dir();
    let skills_id = fs
        .lookup(root_id, &b"skills".as_slice().into())
        .await
        .expect("skills dir can be looked up");

    assert!(matches!(
        fs.create_exclusive(skills_id, &b"writing".as_slice().into())
            .await,
        Err(nfsstat3::NFS3ERR_EXIST)
    ));
    assert_eq!(
        operator
            .read("managed-skills/current/writing/SKILL.md")
            .await
            .expect("existing skill must remain untouched")
            .to_vec(),
        b"# Writing\n"
    );
}

#[tokio::test]
async fn skills_fs_rejects_removing_non_empty_directory() {
    let operator = test_operator().await;
    let fs = SkillsFs::new(operator.clone(), "managed-skills/current").expect("fs can be created");
    let root_id = fs.root_dir();
    let skills_id = fs
        .lookup(root_id, &b"skills".as_slice().into())
        .await
        .expect("skills dir can be looked up");
    let writing_id = fs
        .lookup(skills_id, &b"writing".as_slice().into())
        .await
        .expect("writing dir can be looked up");

    assert!(matches!(
        fs.remove(skills_id, &b"writing".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_NOTEMPTY)
    ));
    assert!(matches!(
        fs.remove(writing_id, &b"SKILL.md".as_slice().into()).await,
        Ok(())
    ));
    fs.remove(skills_id, &b"writing".as_slice().into())
        .await
        .expect("empty directory can be removed");
    assert!(matches!(
        fs.lookup(skills_id, &b"writing".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_NOENT)
    ));
}

#[tokio::test]
async fn skills_fs_renames_directories_with_children() {
    let operator = test_operator().await;
    let fs = SkillsFs::new(operator.clone(), "managed-skills/current").expect("fs can be created");
    let root_id = fs.root_dir();
    let skills_id = fs
        .lookup(root_id, &b"skills".as_slice().into())
        .await
        .expect("skills dir can be looked up");
    let writing_id = fs
        .lookup(skills_id, &b"writing".as_slice().into())
        .await
        .expect("writing dir can be looked up");

    fs.rename(
        skills_id,
        &b"writing".as_slice().into(),
        skills_id,
        &b"writing-renamed".as_slice().into(),
    )
    .await
    .expect("directory rename should succeed");

    assert!(matches!(
        fs.lookup(skills_id, &b"writing".as_slice().into()).await,
        Err(nfsstat3::NFS3ERR_NOENT)
    ));
    let renamed_id = fs
        .lookup(skills_id, &b"writing-renamed".as_slice().into())
        .await
        .expect("renamed dir can be looked up");
    let renamed_skill_id = fs
        .lookup(renamed_id, &b"SKILL.md".as_slice().into())
        .await
        .expect("renamed child file can be looked up");
    let (bytes, eof) = fs
        .read(renamed_skill_id, 0, 1024)
        .await
        .expect("renamed child file can be read");
    assert_eq!(bytes, b"# Writing\n");
    assert!(eof);
    assert!(matches!(
        fs.getattr(writing_id).await,
        Err(nfsstat3::NFS3ERR_STALE)
    ));
}

#[tokio::test]
async fn skills_fs_file_handles_survive_server_restart() {
    let operator = test_operator().await;
    let fs = SkillsFs::new(operator.clone(), "managed-skills/current").expect("fs can be created");
    let root_id = fs.root_dir();
    let skills_id = fs
        .lookup(root_id, &b"skills".as_slice().into())
        .await
        .expect("skills dir can be looked up");
    let writing_id = fs
        .lookup(skills_id, &b"writing".as_slice().into())
        .await
        .expect("writing dir can be looked up");
    let skill_id = fs
        .lookup(writing_id, &b"SKILL.md".as_slice().into())
        .await
        .expect("skill file can be looked up");
    let skill_handle = fs.id_to_fh(skill_id);

    let restarted =
        SkillsFs::new(operator, "managed-skills/current").expect("restarted fs can be created");
    let _skills_id = restarted
        .lookup(root_id, &b"skills".as_slice().into())
        .await
        .expect("skills dir can be looked up after restart");
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
