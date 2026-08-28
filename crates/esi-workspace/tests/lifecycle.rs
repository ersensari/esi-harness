use esi_workspace::{
    CleanupApproval, LifecycleState, RecoveryStatus, SessionId, WorkspaceError, WorkspaceManager,
};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    repository: std::path::PathBuf,
    manager: WorkspaceManager,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, ["init", "-b", "main"]);
        git(&repository, ["config", "user.name", "ESI Test"]);
        git(&repository, ["config", "user.email", "esi@example.invalid"]);
        std::fs::write(repository.join("README.md"), "base\n").unwrap();
        git(&repository, ["add", "README.md"]);
        git(&repository, ["commit", "-m", "base"]);
        let manager =
            WorkspaceManager::new(root.path().join("metadata"), root.path().join("worktrees"));
        Self {
            _root: root,
            repository,
            manager,
        }
    }
}

#[test]
fn create_resume_and_inspect_preserve_dirty_main_worktree() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repository.join("local.txt"), "user change\n").unwrap();
    let main_head_before = git_stdout(&fixture.repository, ["rev-parse", "HEAD"]);
    let main_status_before = git_stdout(&fixture.repository, ["status", "--porcelain=v1"]);
    let session_id = SessionId::new("session-001").unwrap();

    let created = fixture
        .manager
        .create(&fixture.repository, session_id.clone(), "HEAD")
        .unwrap();
    let resumed = fixture
        .manager
        .resume(&fixture.repository, &session_id)
        .unwrap();

    assert_eq!(created, resumed);
    assert_eq!(created.record.state, LifecycleState::Ready);
    assert!(created.record.identity.main_was_dirty);
    assert_ne!(created.record.identity.worktree_path, fixture.repository);
    assert_eq!(
        git_stdout(&fixture.repository, ["rev-parse", "HEAD"]),
        main_head_before
    );
    assert_eq!(
        git_stdout(&fixture.repository, ["status", "--porcelain=v1"]),
        main_status_before
    );
}

#[test]
fn cleanup_requires_exact_approval_and_refuses_dirty_worktrees() {
    let fixture = Fixture::new();
    let session_id = SessionId::new("cleanup").unwrap();
    let created = fixture
        .manager
        .create(&fixture.repository, session_id.clone(), "HEAD")
        .unwrap();
    let request = fixture
        .manager
        .prepare_cleanup(&fixture.repository, &session_id)
        .unwrap();
    let mut wrong_request = request.clone();
    wrong_request.expected_head = "0".repeat(40);
    assert!(matches!(
        fixture.manager.cleanup(
            &fixture.repository,
            CleanupApproval {
                request: wrong_request,
                delete_branch: true,
            }
        ),
        Err(WorkspaceError::ApprovalMismatch)
    ));

    std::fs::write(
        created.record.identity.worktree_path.join("dirty.txt"),
        "dirty\n",
    )
    .unwrap();
    assert!(matches!(
        fixture.manager.cleanup(
            &fixture.repository,
            CleanupApproval {
                request: request.clone(),
                delete_branch: true,
            }
        ),
        Err(WorkspaceError::DirtyWorktree)
    ));
    std::fs::remove_file(created.record.identity.worktree_path.join("dirty.txt")).unwrap();

    let cleaned = fixture
        .manager
        .cleanup(
            &fixture.repository,
            CleanupApproval {
                request,
                delete_branch: true,
            },
        )
        .unwrap();
    assert_eq!(cleaned.state, LifecycleState::Cleaned);
    assert!(!cleaned.identity.worktree_path.exists());
    assert!(!git_succeeds(
        &fixture.repository,
        ["show-ref", "--verify", "--quiet", "refs/heads/esi/cleanup"]
    ));
}

#[test]
fn command_guard_blocks_main_worktree_and_destructive_operations() {
    let fixture = Fixture::new();
    let session_id = SessionId::new("guard").unwrap();
    let created = fixture
        .manager
        .create(&fixture.repository, session_id, "HEAD")
        .unwrap();
    assert!(matches!(
        fixture
            .manager
            .guard_git_command(&created.record, &fixture.repository, &["status"]),
        Err(WorkspaceError::MainWorktreeRejected(_))
    ));
    for arguments in [
        vec!["push"],
        vec!["merge", "main"],
        vec!["reset", "--hard"],
        vec!["clean", "-fd"],
        vec!["commit", "--amend"],
        vec!["-C", fixture.repository.to_str().unwrap(), "status"],
        vec!["unknown-alias"],
    ] {
        assert!(matches!(
            fixture.manager.guard_git_command(
                &created.record,
                &created.record.identity.worktree_path,
                &arguments
            ),
            Err(WorkspaceError::ForbiddenGitOperation(_))
        ));
    }
    fixture
        .manager
        .guard_git_command(
            &created.record,
            &created.record.identity.worktree_path,
            &["status", "--short"],
        )
        .unwrap();
}

#[cfg(unix)]
#[test]
fn lifecycle_git_inspection_does_not_execute_repository_fsmonitor() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let marker = fixture._root.path().join("fsmonitor-ran");
    let hook = fixture._root.path().join("fsmonitor-hook");
    std::fs::write(&hook, format!("#!/bin/sh\n: > '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &fixture.repository,
        ["config", "core.fsmonitor", hook.to_str().unwrap()],
    );

    fixture
        .manager
        .create(
            &fixture.repository,
            SessionId::new("no-hooks").unwrap(),
            "HEAD",
        )
        .unwrap();

    assert!(!marker.exists());
}

#[test]
fn promotion_preparation_reports_committed_candidate_without_promoting() {
    let fixture = Fixture::new();
    let session_id = SessionId::new("promotion").unwrap();
    let created = fixture
        .manager
        .create(&fixture.repository, session_id.clone(), "HEAD")
        .unwrap();
    let worktree = &created.record.identity.worktree_path;
    std::fs::write(worktree.join("feature.txt"), "candidate\n").unwrap();
    git(worktree, ["add", "feature.txt"]);
    git(worktree, ["commit", "-m", "candidate"]);

    let preparation = fixture
        .manager
        .prepare_promotion(&fixture.repository, &session_id)
        .unwrap();

    assert!(preparation.human_approval_required);
    assert_eq!(preparation.changed_files, vec!["feature.txt"]);
    assert_eq!(
        preparation.candidate_commit,
        git_stdout(worktree, ["rev-parse", "HEAD"])
    );
    assert_eq!(
        git_stdout(&fixture.repository, ["branch", "--show-current"]),
        "main"
    );
    assert!(!fixture.repository.join("feature.txt").exists());
}

#[test]
fn recovery_distinguishes_missing_managed_worktree() {
    let fixture = Fixture::new();
    let session_id = SessionId::new("recovery").unwrap();
    let created = fixture
        .manager
        .create(&fixture.repository, session_id.clone(), "HEAD")
        .unwrap();
    git(
        &fixture.repository,
        [
            "worktree",
            "remove",
            created.record.identity.worktree_path.to_str().unwrap(),
        ],
    );

    assert!(matches!(
        fixture
            .manager
            .recover(&fixture.repository, &session_id)
            .unwrap(),
        RecoveryStatus::MissingWorktree(_)
    ));
    let request = fixture
        .manager
        .prepare_recovery_cleanup(&fixture.repository, &session_id)
        .unwrap();
    let cleaned = fixture
        .manager
        .finalize_recovery_cleanup(&fixture.repository, request)
        .unwrap();
    assert_eq!(cleaned.state, LifecycleState::Cleaned);
}

#[test]
fn session_id_rejects_branch_and_path_injection() {
    for value in ["", "../escape", "nested/name", "space name", "."] {
        assert!(matches!(
            SessionId::new(value),
            Err(WorkspaceError::InvalidSessionId(_))
        ));
    }
}

fn git<const SIZE: usize>(cwd: &Path, arguments: [&str; SIZE]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const SIZE: usize>(cwd: &Path, arguments: [&str; SIZE]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_succeeds<const SIZE: usize>(cwd: &Path, arguments: [&str; SIZE]) -> bool {
    Command::new("git")
        .current_dir(cwd)
        .args(arguments)
        .status()
        .unwrap()
        .success()
}
