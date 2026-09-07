use super::*;

#[cfg(target_os = "linux")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Fixed broker program, not model-provided source. All file access happens
// inside the same namespace as shell execution, including symlink resolution.
#[cfg(target_os = "linux")]
const FILE_BROKER: &str = r#"
import json, os, pathlib, sys
a=json.load(sys.stdin); tool=sys.argv[1]; p=pathlib.Path(a.get('path','.'))
if tool=='write':
    p.parent.mkdir(parents=True,exist_ok=True); p.write_text(a['content']); print('File written')
elif tool=='edit':
    text=p.read_text(); before=a['before']
    if not before or text.count(before)!=1: raise ValueError('before must match exactly once')
    p.write_text(text.replace(before,a['after'],1)); print('File edited')
elif tool=='tree':
    depth=min(max(int(a.get('depth',2)),1),20); count=0
    for root, dirs, files in os.walk(p,followlinks=False):
        dirs.sort(); files.sort()
        if len(pathlib.Path(root).relative_to(p).parts)>=depth: dirs[:]=[]
        for name in dirs+files:
            print(os.path.join(root,name)); count+=1
            if count>=2000: print('[tree truncated]'); sys.exit(0)
else: raise ValueError('unsupported file tool')
"#;

#[cfg(target_os = "linux")]
fn check_mount(root: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    // A writable hard link could otherwise modify an inode outside the mount.
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            count += 1;
            ensure!(
                count <= 250_000,
                "Workspace exceeds containment inventory limit"
            );
            let meta = std::fs::symlink_metadata(entry.path())?;
            ensure!(
                !meta.is_file() || meta.nlink() == 1,
                "Workspace contains hard-linked file: {}",
                entry.path().display()
            );
            ensure!(
                meta.is_file() || meta.is_dir() || meta.file_type().is_symlink(),
                "Special workspace files are not supported"
            );
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) async fn execute(
    root: &Path,
    tool: &str,
    mut args: JsonObject,
    cancellation: CancellationToken,
) -> Result<CallToolResult> {
    use std::process::Stdio;
    use tokio::process::Command;
    ensure!(
        tool != "read_image",
        "Contained image reading is not yet supported; no host fallback"
    );
    let inventory_root = root.to_path_buf();
    tokio::task::spawn_blocking(move || check_mount(&inventory_root)).await??;
    if tool != "shell" {
        let raw = args
            .get("path")
            .and_then(Value::as_str)
            .context("A file path is required")?;
        crate::workspace_plan_gate::validate_write_path(root, raw)?;
        let path = Path::new(raw);
        if path.is_absolute() {
            args.insert(
                "path".into(),
                Value::String(path.strip_prefix(root)?.to_string_lossy().into_owned()),
            );
        }
    }
    let mut command = Command::new("/usr/bin/bwrap");
    command.env_clear().args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/tmp/home",
        "--setenv",
        "HOME",
        "/tmp/home",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--setenv",
        "LANG",
        "C.UTF-8",
    ]);
    let mount_option = if tool == "tree" {
        "--ro-bind"
    } else {
        "--bind"
    };
    command.arg(mount_option).arg(root).arg(root);
    for name in [".esi", ".git"] {
        let path = root.join(name);
        if path.try_exists()? {
            ensure!(
                !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "Control directory cannot be a symlink"
            );
            command.arg("--ro-bind").arg(&path).arg(&path);
        }
    }
    command.arg("--chdir").arg(root).arg("--");
    let duration = args
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(120)
        .clamp(1, 300);
    if tool == "shell" {
        command.args([
            "/bin/sh",
            "-c",
            args.get("command")
                .and_then(Value::as_str)
                .context("Missing shell command")?,
        ]);
    } else {
        command.args(["/usr/bin/python3", "-I", "-c", FILE_BROKER, tool]);
    }
    let input = serde_json::to_vec(&args)?;
    ensure!(input.len() <= 1_048_576, "Tool arguments exceed 1 MiB");
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Contained tools require Linux bubblewrap; no unrestricted fallback")?;
    let mut stdin = child.stdin.take().context("Missing tool input pipe")?;
    let mut stdout = child.stdout.take().context("Missing tool output pipe")?;
    let mut stderr = child.stderr.take().context("Missing tool error pipe")?;
    let collect = async {
        let write = async {
            if tool != "shell" {
                stdin.write_all(&input).await?;
            }
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let read = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut buf_out = [0; 8192];
            let mut buf_err = [0; 8192];
            let (mut out_done, mut err_done) = (false, false);
            while !out_done || !err_done {
                tokio::select! {
                    n = stdout.read(&mut buf_out), if !out_done => { let n = n?; out_done = n == 0; out.extend_from_slice(&buf_out[..n]); },
                    n = stderr.read(&mut buf_err), if !err_done => { let n = n?; err_done = n == 0; err.extend_from_slice(&buf_err[..n]); },
                }
                ensure!(
                    out.len() + err.len() <= 1_048_576,
                    "Tool output exceeded 1 MiB; process terminated"
                );
            }
            Ok::<_, anyhow::Error>((out, err))
        };
        let (_, (out, err)) =
            tokio::try_join!(async { write.await.map_err(anyhow::Error::from) }, read)?;
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((status, out, err))
    };
    let result = tokio::select! {
        _ = cancellation.cancelled() => Err(anyhow::anyhow!("Tool cancelled; process terminated")),
        result = tokio::time::timeout(Duration::from_secs(duration), collect) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("Tool timed out; process terminated"))),
    };
    match result {
        Ok((status, out, err)) => {
            let content = vec![ContentBlock::text(serde_json::to_string(&json!({
                "stdout": String::from_utf8_lossy(&out), "stderr": String::from_utf8_lossy(&err),
                "exit_code": status.code(), "contained": true
            }))?)];
            Ok(if status.success() {
                CallToolResult::success(content)
            } else {
                CallToolResult::error(content)
            })
        }
        Err(error) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(error)
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) async fn execute(
    _: &Path,
    _: &str,
    _: JsonObject,
    _: CancellationToken,
) -> Result<CallToolResult> {
    bail!("Contained local tools require Linux bubblewrap on this release; use trusted provider delegation")
}
