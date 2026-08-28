use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thiserror::Error;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("invalid ESI session id: {0}")]
    InvalidSessionId(String),
    #[error("{0:?} is the repository main worktree and cannot be an ESI writable worktree")]
    MainWorktreeRejected(PathBuf),
    #[error("ESI worktree metadata does not match the current repository relationship")]
    OwnershipMismatch,
    #[error("ESI worktree contains uncommitted changes")]
    DirtyWorktree,
    #[error("human approval does not match the exact cleanup request")]
    ApprovalMismatch,
    #[error("Git operation is outside ESI agent authority: {0}")]
    ForbiddenGitOperation(String),
    #[error("Git command failed: {command}: {stderr}")]
    Git { command: String, stderr: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, WorkspaceError>;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if !valid {
            return Err(WorkspaceError::InvalidSessionId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIdentity {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub repository_id: String,
    pub source_repository: PathBuf,
    pub main_worktree: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub main_head_at_creation: String,
    pub main_was_dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    Ready,
    CleanupPending,
    Cleaned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub identity: WorktreeIdentity,
    pub state: LifecycleState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInspection {
    pub record: WorktreeRecord,
    pub head: String,
    pub dirty: bool,
    pub changed_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryStatus {
    Healthy(WorktreeInspection),
    MissingWorktree(WorktreeRecord),
    UnregisteredWorktree(WorktreeRecord),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupRequest {
    pub session_id: SessionId,
    pub repository_id: String,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub expected_head: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupApproval {
    pub request: CleanupRequest,
    pub delete_branch: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCleanupRequest {
    pub session_id: SessionId,
    pub repository_id: String,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub observed_status: RecoveryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryKind {
    MissingWorktree,
    UnregisteredWorktree,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionPreparation {
    pub identity: WorktreeIdentity,
    pub candidate_commit: String,
    pub changed_files: Vec<String>,
    pub human_approval_required: bool,
}

#[derive(Clone, Debug)]
pub struct WorkspaceManager {
    metadata_root: PathBuf,
    worktree_root: PathBuf,
}

impl WorkspaceManager {
    pub fn new(metadata_root: impl Into<PathBuf>, worktree_root: impl Into<PathBuf>) -> Self {
        Self {
            metadata_root: metadata_root.into(),
            worktree_root: worktree_root.into(),
        }
    }

    pub fn create(
        &self,
        source_repository: impl AsRef<Path>,
        session_id: SessionId,
        base_ref: &str,
    ) -> Result<WorktreeInspection> {
        let repository = RepositoryRelationship::inspect(source_repository.as_ref())?;
        let metadata_path = self.metadata_path(&repository.repository_id, &session_id);
        if metadata_path.exists() {
            return self.resume(source_repository, &session_id);
        }

        fs::create_dir_all(&self.worktree_root)?;
        let worktree_path = canonical_or_absolute(&self.worktree_root)?
            .join(&repository.repository_id)
            .join(session_id.as_str());
        if paths_equal(&worktree_path, &repository.main_worktree)? {
            return Err(WorkspaceError::MainWorktreeRejected(worktree_path));
        }

        let branch = format!("esi/{}", session_id.as_str());
        let base_commit = git_stdout(
            &repository.main_worktree,
            ["rev-parse", &format!("{base_ref}^{{commit}}")],
        )?;
        if git_succeeds(
            &repository.main_worktree,
            [
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )? {
            return Err(WorkspaceError::OwnershipMismatch);
        }

        fs::create_dir_all(worktree_path.parent().expect("worktree path has a parent"))?;
        git(
            &repository.main_worktree,
            [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("-b"),
                OsString::from(&branch),
                worktree_path.as_os_str().to_owned(),
                OsString::from(&base_commit),
            ],
        )?;

        let record = WorktreeRecord {
            identity: WorktreeIdentity {
                schema_version: SCHEMA_VERSION,
                session_id,
                repository_id: repository.repository_id,
                source_repository: repository.source_repository,
                main_worktree: repository.main_worktree,
                worktree_path,
                branch,
                base_commit,
                main_head_at_creation: repository.main_head,
                main_was_dirty: repository.main_dirty,
            },
            state: LifecycleState::Ready,
        };
        self.write_record(&record)?;
        self.inspect_record(record)
    }

    pub fn resume(
        &self,
        source_repository: impl AsRef<Path>,
        session_id: &SessionId,
    ) -> Result<WorktreeInspection> {
        let repository = RepositoryRelationship::inspect(source_repository.as_ref())?;
        let record = self.read_record(&repository.repository_id, session_id)?;
        self.verify_relationship(&repository, &record)?;
        self.inspect_record(record)
    }

    pub fn inspect(
        &self,
        source_repository: impl AsRef<Path>,
        session_id: &SessionId,
    ) -> Result<WorktreeInspection> {
        self.resume(source_repository, session_id)
    }

    pub fn recover(
        &self,
        source_repository: impl AsRef<Path>,
        session_id: &SessionId,
    ) -> Result<RecoveryStatus> {
        let repository = RepositoryRelationship::inspect(source_repository.as_ref())?;
        let record = self.read_record(&repository.repository_id, session_id)?;
        self.verify_relationship(&repository, &record)?;
        if !record.identity.worktree_path.exists() {
            return Ok(RecoveryStatus::MissingWorktree(record));
        }
        if !is_registered_worktree(&repository.main_worktree, &record.identity.worktree_path)? {
            return Ok(RecoveryStatus::UnregisteredWorktree(record));
        }
        Ok(RecoveryStatus::Healthy(self.inspect_record(record)?))
    }

    pub fn prepare_recovery_cleanup(
        &self,
        source_repository: impl AsRef<Path>,
        session_id: &SessionId,
    ) -> Result<RecoveryCleanupRequest> {
        let (record, observed_status) = match self.recover(source_repository, session_id)? {
            RecoveryStatus::MissingWorktree(record) => (record, RecoveryKind::MissingWorktree),
            RecoveryStatus::UnregisteredWorktree(record) => {
                (record, RecoveryKind::UnregisteredWorktree)
            }
            RecoveryStatus::Healthy(_) => return Err(WorkspaceError::OwnershipMismatch),
        };
        Ok(RecoveryCleanupRequest {
            session_id: record.identity.session_id,
            repository_id: record.identity.repository_id,
            worktree_path: record.identity.worktree_path,
            branch: record.identity.branch,
            observed_status,
        })
    }

    pub fn finalize_recovery_cleanup(
        &self,
        source_repository: impl AsRef<Path>,
        approval: RecoveryCleanupRequest,
    ) -> Result<WorktreeRecord> {
        let expected =
            self.prepare_recovery_cleanup(source_repository.as_ref(), &approval.session_id)?;
        if approval != expected {
            return Err(WorkspaceError::ApprovalMismatch);
        }
        let repository = RepositoryRelationship::inspect(source_repository.as_ref())?;
        let mut record = self.read_record(&repository.repository_id, &approval.session_id)?;
        record.state = LifecycleState::Cleaned;
        self.write_record(&record)?;
        Ok(record)
    }

    pub fn prepare_cleanup(
        &self,
        source_repository: impl AsRef<Path>,
        session_id: &SessionId,
    ) -> Result<CleanupRequest> {
        let inspection = self.inspect(source_repository, session_id)?;
        if inspection.dirty {
            return Err(WorkspaceError::DirtyWorktree);
        }
        let identity = inspection.record.identity;
        Ok(CleanupRequest {
            session_id: identity.session_id,
            repository_id: identity.repository_id,
            worktree_path: identity.worktree_path,
            branch: identity.branch,
            expected_head: inspection.head,
        })
    }

    pub fn cleanup(
        &self,
        source_repository: impl AsRef<Path>,
        approval: CleanupApproval,
    ) -> Result<WorktreeRecord> {
        let inspection = self.inspect(source_repository.as_ref(), &approval.request.session_id)?;
        let expected = CleanupRequest {
            session_id: inspection.record.identity.session_id.clone(),
            repository_id: inspection.record.identity.repository_id.clone(),
            worktree_path: inspection.record.identity.worktree_path.clone(),
            branch: inspection.record.identity.branch.clone(),
            expected_head: inspection.head.clone(),
        };
        if approval.request != expected {
            return Err(WorkspaceError::ApprovalMismatch);
        }
        if inspection.dirty {
            return Err(WorkspaceError::DirtyWorktree);
        }

        let mut record = inspection.record;
        record.state = LifecycleState::CleanupPending;
        self.write_record(&record)?;
        git(
            &record.identity.main_worktree,
            [
                OsString::from("worktree"),
                OsString::from("remove"),
                record.identity.worktree_path.as_os_str().to_owned(),
            ],
        )?;
        if approval.delete_branch {
            git(
                &record.identity.main_worktree,
                [
                    OsString::from("branch"),
                    OsString::from("-d"),
                    OsString::from(&record.identity.branch),
                ],
            )?;
        }
        record.state = LifecycleState::Cleaned;
        self.write_record(&record)?;
        Ok(record)
    }

    pub fn prepare_promotion(
        &self,
        source_repository: impl AsRef<Path>,
        session_id: &SessionId,
    ) -> Result<PromotionPreparation> {
        let inspection = self.inspect(source_repository, session_id)?;
        if inspection.dirty {
            return Err(WorkspaceError::DirtyWorktree);
        }
        Ok(PromotionPreparation {
            identity: inspection.record.identity,
            candidate_commit: inspection.head,
            changed_files: inspection.changed_files,
            human_approval_required: true,
        })
    }

    pub fn guard_git_command(
        &self,
        record: &WorktreeRecord,
        cwd: impl AsRef<Path>,
        arguments: &[impl AsRef<OsStr>],
    ) -> Result<()> {
        let cwd = canonical_or_absolute(cwd.as_ref())?;
        if paths_equal(&cwd, &record.identity.main_worktree)? {
            return Err(WorkspaceError::MainWorktreeRejected(cwd));
        }
        if !paths_equal(&cwd, &record.identity.worktree_path)? {
            return Err(WorkspaceError::OwnershipMismatch);
        }

        let arguments = arguments
            .iter()
            .map(|argument| argument.as_ref().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if arguments.iter().any(|argument| {
            argument == "-C"
                || argument.starts_with("--git-dir")
                || argument.starts_with("--work-tree")
                || argument.starts_with("--config-env")
                || argument == "-c"
        }) {
            return Err(WorkspaceError::ForbiddenGitOperation(arguments.join(" ")));
        }
        let subcommand = arguments
            .iter()
            .find(|argument| !argument.starts_with('-'))
            .map(String::as_str)
            .unwrap_or_default();
        let allowed = matches!(
            subcommand,
            "add"
                | "blame"
                | "commit"
                | "diff"
                | "grep"
                | "log"
                | "ls-files"
                | "rev-parse"
                | "show"
                | "status"
        );
        if !allowed
            || (subcommand == "commit"
                && arguments
                    .iter()
                    .any(|argument| argument == "--amend" || argument.starts_with("--fixup")))
        {
            return Err(WorkspaceError::ForbiddenGitOperation(arguments.join(" ")));
        }
        Ok(())
    }

    fn inspect_record(&self, record: WorktreeRecord) -> Result<WorktreeInspection> {
        if record.state == LifecycleState::Cleaned {
            return Err(WorkspaceError::OwnershipMismatch);
        }
        let relationship = RepositoryRelationship::inspect(&record.identity.worktree_path)?;
        self.verify_relationship(&relationship, &record)?;
        if paths_equal(
            &record.identity.worktree_path,
            &record.identity.main_worktree,
        )? {
            return Err(WorkspaceError::MainWorktreeRejected(
                record.identity.worktree_path.clone(),
            ));
        }
        if !is_registered_worktree(
            &record.identity.main_worktree,
            &record.identity.worktree_path,
        )? {
            return Err(WorkspaceError::OwnershipMismatch);
        }
        let branch = git_stdout(&record.identity.worktree_path, ["branch", "--show-current"])?;
        if branch != record.identity.branch {
            return Err(WorkspaceError::OwnershipMismatch);
        }
        let head = git_stdout(&record.identity.worktree_path, ["rev-parse", "HEAD"])?;
        let status = git_stdout(
            &record.identity.worktree_path,
            ["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        let changed_files = git_stdout(
            &record.identity.worktree_path,
            [
                "diff",
                "--name-only",
                &format!("{}..HEAD", record.identity.base_commit),
            ],
        )?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
        Ok(WorktreeInspection {
            record,
            head,
            dirty: !status.is_empty(),
            changed_files,
        })
    }

    fn verify_relationship(
        &self,
        repository: &RepositoryRelationship,
        record: &WorktreeRecord,
    ) -> Result<()> {
        let identity = &record.identity;
        if identity.schema_version != SCHEMA_VERSION
            || identity.repository_id != repository.repository_id
            || !paths_equal(&identity.main_worktree, &repository.main_worktree)?
        {
            return Err(WorkspaceError::OwnershipMismatch);
        }
        Ok(())
    }

    fn metadata_path(&self, repository_id: &str, session_id: &SessionId) -> PathBuf {
        self.metadata_root
            .join(repository_id)
            .join(format!("{}.json", session_id.as_str()))
    }

    fn read_record(&self, repository_id: &str, session_id: &SessionId) -> Result<WorktreeRecord> {
        let bytes = fs::read(self.metadata_path(repository_id, session_id))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn write_record(&self, record: &WorktreeRecord) -> Result<()> {
        let path = self.metadata_path(&record.identity.repository_id, &record.identity.session_id);
        fs::create_dir_all(path.parent().expect("metadata path has a parent"))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

#[derive(Debug)]
struct RepositoryRelationship {
    source_repository: PathBuf,
    main_worktree: PathBuf,
    repository_id: String,
    main_head: String,
    main_dirty: bool,
}

impl RepositoryRelationship {
    fn inspect(path: &Path) -> Result<Self> {
        let source_repository = canonical_or_absolute(path)?;
        let common_dir = canonical_or_absolute(Path::new(&git_stdout(
            &source_repository,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?))?;
        let worktrees = git_stdout(&source_repository, ["worktree", "list", "--porcelain"])?;
        let main_worktree = worktrees
            .lines()
            .find_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .ok_or(WorkspaceError::OwnershipMismatch)
            .and_then(|path| canonical_or_absolute(&path))?;
        let repository_id = Sha256::digest(common_dir.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let main_head = git_stdout(&main_worktree, ["rev-parse", "HEAD"])?;
        let main_dirty = !git_stdout(
            &main_worktree,
            ["status", "--porcelain=v1", "--untracked-files=all"],
        )?
        .is_empty();
        Ok(Self {
            source_repository,
            main_worktree,
            repository_id,
            main_head,
            main_dirty,
        })
    }
}

fn is_registered_worktree(main_worktree: &Path, expected: &Path) -> Result<bool> {
    let output = git_stdout(main_worktree, ["worktree", "list", "--porcelain"])?;
    output
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(Path::new)
        .try_fold(false, |matched, candidate| {
            Ok(matched || paths_equal(candidate, expected)?)
        })
}

fn paths_equal(left: &Path, right: &Path) -> Result<bool> {
    Ok(canonical_or_absolute(left)? == canonical_or_absolute(right)?)
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::path::absolute(path)?)
        }
        Err(error) => Err(error.into()),
    }
}

fn git_stdout<I, S>(cwd: &Path, arguments: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git(cwd, arguments)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn git_succeeds<I, S>(cwd: &Path, arguments: I) -> Result<bool>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arguments = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect::<Vec<_>>();
    let output = git_command(cwd).args(&arguments).output()?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(git_error(&arguments, output))
}

fn git<I, S>(cwd: &Path, arguments: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let arguments = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect::<Vec<_>>();
    let output = git_command(cwd).args(&arguments).output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(git_error(&arguments, output))
    }
}

fn git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=",
            "-c",
            "credential.helper=",
        ]);
    command
}

fn git_error(arguments: &[OsString], output: Output) -> WorkspaceError {
    WorkspaceError::Git {
        command: format!(
            "git {}",
            arguments
                .iter()
                .map(|argument| argument.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}
