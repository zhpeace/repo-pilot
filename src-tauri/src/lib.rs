// RepoPilot —— 本地多仓库批量管理工具（MVP 核心逻辑）
// 安全约定：调用 git 一律用 Command::new + 参数数组，禁止拼接 shell 字符串。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};
use tauri::{Emitter, Manager};
use tokio::sync::Semaphore;

/// 批量 git 操作的最大并发数：一次最多同时跑 N 个仓库，
/// 避免大批量仓库（如 374 个）同时 SSH/HTTP 握手触发远程限流或界面卡死。
const MAX_CONCURRENT: usize = 16;

/// 批量操作取消标志：前端点击“取消”后置位，尚未开始的仓库直接跳过。
static BATCH_CANCEL: AtomicBool = AtomicBool::new(false);

#[tauri::command]
fn cancel_batch() {
    BATCH_CANCEL.store(true, Ordering::SeqCst);
}

#[derive(Serialize, Clone)]
struct BatchProgress {
    done: i32,
    total: i32,
    ok: i32,
    path: String,
}

#[derive(Serialize, Clone)]
struct RepoEntry {
    path: String,
    name: String,
    parent: Option<String>,
}

#[derive(Serialize, Clone, Default)]
struct RepoStatus {
    path: String,
    branch: String,
    remote_url: String,
    dirty: bool,
    changed: i32,
    ahead: i32,
    behind: i32,
    last_commit: Option<i64>,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
struct OpResult {
    path: String,
    ok: bool,
    message: String,
}

#[derive(serde::Serialize)]
struct RemoteConflictResult {
    ok: bool,
    degraded: bool,
    message: String,
    remote_changes: Vec<String>,
}

/// 比对当前分支与远程跟踪分支的文件差异：列出「远程也改动」的文件（本地也改了 → pull 可能冲突）。
/// 先静默 fetch 更新远程引用（不动工作区），再 diff HEAD..上游分支。
/// 传入 auth_user/auth_pass 时带凭据执行（认证弹窗确认后的重试）。
#[tauri::command]
fn check_remote_conflicts(
    path: String,
    auth_user: Option<String>,
    auth_pass: Option<String>,
    auth_save: bool,
) -> RemoteConflictResult {
    let dir = Path::new(&path);
    let up = match run_git(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]) {
        Ok(u) => u.trim().to_string(),
        Err(_) => {
            return RemoteConflictResult {
                ok: true,
                degraded: false,
                message: "无远程跟踪分支，无法比对".to_string(),
                remote_changes: vec![],
            }
        }
    };
    let fetch_res = match (&auth_user, &auth_pass) {
        (Some(u), Some(p)) => run_git_auth_impl(dir, &["fetch", "origin"], u, p, 40),
        _ => run_git_timeout(dir, &["fetch", "origin"], 30),
    };
    if let Err(e) = fetch_res {
        if auth_save {
            if let Ok(url) = run_git(dir, &["config", "--get", "remote.origin.url"]) {
                if let (Some(u), Some(p)) = (&auth_user, &auth_pass) {
                    approve_credentials(url.trim(), u, p);
                }
            }
        }
        // fetch 失败（认证/网络）：降级用上次同步的远程跟踪分支比对，标注「按上次同步状态」
        if let Ok(out) = run_git(dir, &["diff", "--name-only", "HEAD", &up]) {
            let files: Vec<String> = out
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            return RemoteConflictResult {
                ok: true,
                degraded: true,
                message: format!(
                    "远程需认证，按上次同步状态比对：远程分支 {up} 有 {} 个文件改动",
                    files.len()
                ),
                remote_changes: files,
            };
        }
        return RemoteConflictResult {
            ok: false,
            degraded: false,
            message: format!("fetch 失败：{e}"),
            remote_changes: vec![],
        };
    }
    match run_git(dir, &["diff", "--name-only", "HEAD", &up]) {
        Ok(out) => {
            let files: Vec<String> = out
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            RemoteConflictResult {
                ok: true,
                degraded: false,
                message: format!("远程分支 {up} 有 {} 个文件改动", files.len()),
                remote_changes: files,
            }
        }
        Err(e) => RemoteConflictResult {
            ok: false,
            degraded: false,
            message: format!("比对失败：{e}"),
            remote_changes: vec![],
        },
    }
}

/// 构造 git 命令：Windows 下必须隐藏控制台窗口——
/// GUI 进程启动 git.exe（控制台程序）时若不设 CREATE_NO_WINDOW，每个 git 进程都会弹一个黑框窗口。
fn new_git_cmd() -> Command {
    let mut c = Command::new("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = new_git_cmd()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("无法执行 git：{e}"))?;
    if out.status.success() {
        // 注意：只用 trim_end，不能 trim() —— porcelain 输出行首是状态码的空位（如 " M file"），
        // trim() 会把行首空格也去掉，导致 list_changes 按 line[3..] 解析时路径丢失第一个字符
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { "git 命令失败".to_string() } else { err })
    }
}

/// 带超时的 git 调用（用于批量写操作）：stdin 置空避免交互式认证挂起。
/// 超时会真正 kill 掉 git 子进程并返回明确错误，避免残留挂死的进程堆积。
fn run_git_timeout(dir: &Path, args: &[&str], secs: u64) -> Result<String, String> {
    let mut child = new_git_cmd()
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法执行 git：{e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child
                    .wait_with_output()
                    .map_err(|e| format!("读取 git 输出失败：{e}"))?;
                if out.status.success() {
                    return Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string());
                } else {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    return Err(if err.is_empty() { "git 命令失败".to_string() } else { err });
                }
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("git 命令超时（可能需要认证，请先在终端手动操作一次）".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(format!("等待 git 失败：{e}")),
        }
    }
}

// ---------- 认证支持（SourceTree 风格认证弹窗的后端） ----------
/// GIT_ASKPASS 脚本：git 需要认证时按提示类型从环境变量输出用户名/密码
static ASKPASS: OnceLock<std::path::PathBuf> = OnceLock::new();
fn askpass_script() -> &'static std::path::PathBuf {
    ASKPASS.get_or_init(|| {
        let p = std::env::temp_dir().join("repopilot_askpass.sh");
        let script = r#"#!/bin/sh
case "$1" in
  *[Uu]sername*) printf '%s\n' "$GIT_USERNAME" ;;
  *[Pp]assword*) printf '%s\n' "$GIT_PASSWORD" ;;
  *) exit 1 ;;
esac
"#;
        let _ = std::fs::write(&p, script);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
        }
        p
    })
}

/// 带凭据执行的 git 调用：GIT_ASKPASS 脚本 + 用户名/密码环境变量 + 超时
fn run_git_auth_impl(
    dir: &Path,
    args: &[&str],
    username: &str,
    password: &str,
    secs: u64,
) -> Result<String, String> {
    let mut child = new_git_cmd()
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_ASKPASS", askpass_script())
        .env("GIT_USERNAME", username)
        .env("GIT_PASSWORD", password)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法执行 git：{e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child
                    .wait_with_output()
                    .map_err(|e| format!("读取 git 输出失败：{e}"))?;
                if out.status.success() {
                    return Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string());
                } else {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    return Err(if err.is_empty() { "git 命令失败".to_string() } else { err });
                }
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("git 命令超时".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(format!("等待 git 失败：{e}")),
        }
    }
}

/// 从 remote url 解析 host（含端口），用于凭据写入钥匙串
fn parse_http_host(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches(".git");
    let rest = u.split_once("://")?.1;
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// 把凭据写入 macOS 钥匙串（osxkeychain helper），并启用全局 helper 供后续自动使用
fn approve_credentials(url: &str, username: &str, password: &str) {
    let Some(host) = parse_http_host(url) else {
        return;
    };
    let protocol = if url.starts_with("https://") { "https" } else { "http" };
    let input = format!(
        "protocol={protocol}\nhost={host}\nusername={username}\npassword={password}\n\n"
    );
    use std::io::Write;
    if let Ok(mut child) = new_git_cmd()
        .arg("-c")
        .arg("credential.helper=osxkeychain")
        .args(["credential", "approve"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(input.as_bytes());
        }
        let _ = child.wait();
    }
    // 启用全局 helper，后续 pull/push 自动从钥匙串取凭据
    let _ = new_git_cmd()
        .args(["config", "--global", "credential.helper", "osxkeychain"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// 带凭据重试一个 git 写操作（认证弹窗确认后调用）；save=true 时写入钥匙串
#[tauri::command]
fn run_git_auth(
    path: String,
    args: Vec<String>,
    username: String,
    password: String,
    save: bool,
) -> OpResult {
    let dir = Path::new(&path);
    let strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match run_git_auth_impl(dir, &strs, &username, &password, 90) {
        Ok(_) => {
            if save {
                if let Ok(url) = run_git(dir, &["config", "--get", "remote.origin.url"]) {
                    approve_credentials(url.trim(), &username, &password);
                }
            }
            OpResult {
                path,
                ok: true,
                message: "操作成功".to_string(),
            }
        }
        Err(e) => OpResult {
            path,
            ok: false,
            message: friendly_git_err(&e),
        },
    }
}

/// 向 git 标准输入写入内容执行（用于 git apply --cached 从 stdin 应用补丁）
fn run_git_stdin(dir: &Path, args: &[&str], input: &str) -> Result<String, String> {
    use std::io::Write;
    let mut child = new_git_cmd()
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法执行 git：{e}"))?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(input.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("读取 git 输出失败：{e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { "git 命令失败".to_string() } else { err })
    }
}

/// 带超时执行 shell 命令（mac/linux: sh -c；windows: cmd /C）：超时 kill 掉命令进程，避免交互式命令残留挂起
fn run_shell_timeout(cmd: &str, dir: &Path, secs: u64) -> Result<std::process::Output, String> {
    #[cfg(windows)]
    let mut child = {
        use std::os::windows::process::CommandExt;
        Command::new("cmd")
            .arg("/C")
            .arg(cmd)
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .map_err(|e| format!("无法执行命令：{e}"))?
    };
    #[cfg(not(windows))]
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法执行命令：{e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("读取命令输出失败：{e}"));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("命令执行超时（可能等待输入），已中止".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(format!("等待命令失败：{e}")),
        }
    }
}

/// 把常见的 git 认证错误转成可执行的友好提示
fn friendly_git_err(err: &str) -> String {
    let e = err.to_lowercase();
    if e.contains("authentication failed")
        || e.contains("could not read username")
        || e.contains("could not read password")
        || e.contains("terminal prompts disabled")
        || e.contains("401")
    {
        "认证失败：工具不接管凭据，请先在终端对该仓库手动 git pull 一次保存凭据".to_string()
    } else if e.contains("permission denied (publickey)") {
        "SSH 认证失败：请确认 ssh-add 已加载私钥（ssh-add ~/.ssh/id_ed25519）".to_string()
    } else {
        err.to_string()
    }
}

fn is_git_repo(dir: &Path) -> bool {
    // 兼容普通 .git 目录与 worktree 的 .git 文件
    dir.join(".git").exists()
}

fn is_skip_dir(name: &str) -> bool {
    // 跳过常见的无关/重型目录，加快扫描
    matches!(
        name,
        "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".gradle"
            | ".idea"
            | ".vscode"
            | ".git"
            | ".cache"
            | "__pycache__"
            | ".venv"
            | "venv"
            | "Pods"
    )
}

fn scan_dir(dir: &Path, out: &mut Vec<RepoEntry>, depth: usize, parent: Option<&Path>) {
    if depth > 8 {
        return;
    }
    let dirs: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(e) => e
            .flatten()
            .filter(|en| en.path().is_dir() && !is_skip_dir(&en.file_name().to_string_lossy()))
            .map(|en| en.path())
            .collect(),
        Err(_) => return,
    };
    if dirs.len() < 6 {
        // 目录少时直接串行，避免线程开销
        for d in &dirs {
            collect_one(d, out, depth, parent);
        }
        return;
    }
    // 目录多时并行扫描（一次性线程池，限制并发数避免线程风暴）
    use std::sync::Mutex;
    let lock = Mutex::new(out);
    std::thread::scope(|s| {
        for d in dirs {
            let lock = &lock;
            let depth = depth;
            let parent = parent.map(|p| p.to_path_buf());
            s.spawn(move || {
                let mut local = Vec::new();
                collect_one(&d, &mut local, depth, parent.as_deref());
                lock.lock().unwrap().extend(local);
            });
        }
    });
}

fn collect_one(dir: &Path, out: &mut Vec<RepoEntry>, depth: usize, parent: Option<&Path>) {
    if is_git_repo(dir) {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let parent_path = parent.map(|p| p.to_string_lossy().to_string());
        out.push(RepoEntry {
            path: dir.to_string_lossy().to_string(),
            name,
            parent: parent_path,
        });
        // 继续深入收集嵌套子仓库（如 src/modules 下的独立仓库），并记录父子关系
        scan_dir(dir, out, depth + 1, Some(dir));
    } else {
        scan_dir(dir, out, depth + 1, parent);
    }
}

#[tauri::command]
fn scan_repos(root: String) -> Vec<RepoEntry> {
    let mut out = Vec::new();
    let root_path = PathBuf::from(&root);
    if !root_path.is_dir() {
        return out;
    }
    if is_git_repo(&root_path) {
        let name = root_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push(RepoEntry {
            path: root_path.to_string_lossy().to_string(),
            name,
            parent: None,
        });
        scan_dir(&root_path, &mut out, 0, Some(&root_path));
    } else {
        scan_dir(&root_path, &mut out, 0, None);
    }
    out
}

fn parse_count(s: &str, key: &str) -> i32 {
    // 从 "[ahead 1, behind 2]" 之类的括号中取数字
    // 注意：key 之后可能是 " 1"（前导空格），需先跳过非数字字符再取连续数字
    if let Some(idx) = s.find(key) {
        let rest = &s[idx + key.len()..];
        let num: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        return num.parse().unwrap_or(0);
    }
    0
}

fn get_one_status(path: &str) -> RepoStatus {
    let p = Path::new(path);
    let mut st = RepoStatus {
        path: path.to_string(),
        ..Default::default()
    };
    match run_git(p, &["status", "--porcelain=v1", "-b"]) {
        Ok(sb) => {
            // 第一行：## branch...upstream [ahead x] [behind y]
            let first = sb.lines().next().unwrap_or("").to_string();
            if let Some(rest) = first.strip_prefix("## ") {
                st.branch = rest.split("...").next().unwrap_or(rest).trim().to_string();
                if let Some(idx) = rest.find('[') {
                    let bracket = &rest[idx..];
                    if bracket.contains("ahead") {
                        st.ahead = parse_count(bracket, "ahead");
                    }
                    if bracket.contains("behind") {
                        st.behind = parse_count(bracket, "behind");
                    }
                }
            }
            // 除第一行外还有行 => 有未提交改动；记录改动文件数
            let changed_count = sb.lines().count().saturating_sub(1);
            st.dirty = changed_count > 0;
            st.changed = changed_count as i32;
        }
        Err(e) => st.error = Some(e),
    }
    st.remote_url = run_git(p, &["remote", "get-url", "origin"]).unwrap_or_default();
    // 最近一次提交的 unix 时间戳（无提交则为 None）
    st.last_commit = run_git(p, &["log", "-1", "--format=%ct"])
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok());
    st
}

#[tauri::command]
async fn get_statuses(paths: Vec<String>) -> Vec<RepoStatus> {
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let sem = Arc::clone(&sem);
        handles.push(tauri::async_runtime::spawn(async move {
            let _perm = sem.acquire().await.expect("semaphore closed");
            tauri::async_runtime::spawn_blocking(move || get_one_status(&p))
                .await
                .unwrap_or_default()
        }));
    }
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(h.await.unwrap_or_default());
    }
    out
}

/// 冲突检测：仓库是否有已跟踪文件的未提交改动（暂存区或工作树）
/// git diff --quiet 退出码非 0 表示有差异；未跟踪文件不影响 pull，不纳入检测
fn has_local_changes(dir: &Path) -> bool {
    run_git(dir, &["diff", "--quiet"]).is_err() || run_git(dir, &["diff", "--cached", "--quiet"]).is_err()
}

#[tauri::command]
async fn pull_repos(app: tauri::AppHandle, paths: Vec<String>) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    BATCH_CANCEL.store(false, Ordering::SeqCst);
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        let sem = Arc::clone(&sem);
        handles.push(tauri::async_runtime::spawn(async move {
            let _perm = sem.acquire().await.expect("semaphore closed");
            tauri::async_runtime::spawn_blocking(move || {
                if BATCH_CANCEL.load(Ordering::SeqCst) {
                    return OpResult { path: p, ok: false, message: "已取消".to_string() };
                }
                let dir = Path::new(&p);
                let r = if has_local_changes(dir) {
                    // 冲突检测：有已跟踪文件的未提交改动。
                    // 先看远程（按上次 fetch 的跟踪分支）是否真的领先：若远程无新提交，pull 无意义，直接提示无需拉取（算成功）；
                    // 若远程领先才跳过并列出涉及文件，避免「本地改动」被误报为冲突。
                    let remote_new = run_git(dir, &["rev-list", "--count", "HEAD..@{u}"])
                        .ok()
                        .and_then(|s| s.trim().parse::<i64>().ok());
                    match remote_new {
                        Some(n) if n <= 0 => OpResult {
                            path: p,
                            ok: true,
                            message: "本地有未提交改动，远程无新提交（按上次同步状态），无需 pull".to_string(),
                        },
                        _ => {
                            let mut msg = if let Some(n) = remote_new {
                                format!("有未提交改动且远程领先 {n} 个提交，已跳过 pull")
                            } else {
                                "有未提交改动，已跳过 pull".to_string()
                            };
                            if let Ok(s) = run_git(dir, &["status", "--porcelain=v1"]) {
                                let lines: Vec<&str> = s.lines().collect();
                                if !lines.is_empty() {
                                    let shown = lines.iter().take(8);
                                    let sample = shown
                                        .map(|l| l.get(3..).unwrap_or(l).trim().to_string())
                                        .collect::<Vec<String>>()
                                        .join("、");
                                    let tail = if lines.len() > 8 {
                                        format!(" 等 {} 个文件", lines.len())
                                    } else {
                                        format!("（共 {} 个文件）", lines.len())
                                    };
                                    msg.push_str(&format!("：{sample}{tail}"));
                                }
                            }
                            OpResult {
                                path: p,
                                ok: false,
                                message: msg,
                            }
                        }
                    }
                } else {
                    match run_git_timeout(dir, &["pull", "--no-rebase"], 60) {
                        Ok(_) => OpResult {
                            path: p,
                            ok: true,
                            message: "pull 成功".to_string(),
                        },
                        Err(e) => OpResult {
                            path: p,
                            ok: false,
                            message: friendly_git_err(&e),
                        },
                    }
                };
                if r.ok {
                    okc.fetch_add(1, Ordering::SeqCst);
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit(
                    "repopilot-progress",
                    BatchProgress {
                        done: d,
                        total,
                        ok: okc.load(Ordering::SeqCst),
                        path: r.path.clone(),
                    },
                );
                r
            })
            .await
            .unwrap_or_else(|_| OpResult {
                path: "未知".to_string(),
                ok: false,
                message: "后台任务失败".to_string(),
            })
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.unwrap_or_else(|_| OpResult {
            path: "未知".to_string(),
            ok: false,
            message: "后台任务失败".to_string(),
        }));
    }
    results
}

#[tauri::command]
async fn push_repos(app: tauri::AppHandle, paths: Vec<String>) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    BATCH_CANCEL.store(false, Ordering::SeqCst);
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        let sem = Arc::clone(&sem);
        handles.push(tauri::async_runtime::spawn(async move {
            let _perm = sem.acquire().await.expect("semaphore closed");
            tauri::async_runtime::spawn_blocking(move || {
                if BATCH_CANCEL.load(Ordering::SeqCst) {
                    return OpResult { path: p, ok: false, message: "已取消".to_string() };
                }
                let dir = Path::new(&p);
                let r = match run_git_timeout(dir, &["push"], 60) {
                    Ok(_) => OpResult {
                        path: p,
                        ok: true,
                        message: "push 成功".to_string(),
                    },
                    Err(e) => OpResult {
                        path: p,
                        ok: false,
                        message: friendly_git_err(&e),
                    },
                };
                if r.ok {
                    okc.fetch_add(1, Ordering::SeqCst);
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit(
                    "repopilot-progress",
                    BatchProgress {
                        done: d,
                        total,
                        ok: okc.load(Ordering::SeqCst),
                        path: r.path.clone(),
                    },
                );
                r
            })
            .await
            .unwrap_or_else(|_| OpResult {
                path: "未知".to_string(),
                ok: false,
                message: "后台任务失败".to_string(),
            })
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.unwrap_or_else(|_| OpResult {
            path: "未知".to_string(),
            ok: false,
            message: "后台任务失败".to_string(),
        }));
    }
    results
}

#[tauri::command]
async fn stash_repos(app: tauri::AppHandle, paths: Vec<String>, include_untracked: bool, label: String) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    BATCH_CANCEL.store(false, Ordering::SeqCst);
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let label = label.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        let sem = Arc::clone(&sem);
        handles.push(tauri::async_runtime::spawn(async move {
            let _perm = sem.acquire().await.expect("semaphore closed");
            tauri::async_runtime::spawn_blocking(move || {
                if BATCH_CANCEL.load(Ordering::SeqCst) {
                    return OpResult { path: p, ok: false, message: "已取消".to_string() };
                }
                let dir = Path::new(&p);
                // 无改动则跳过，避免 "No local changes to save"
                let r = match run_git(dir, &["status", "--porcelain"]) {
                    Ok(s) if s.trim().is_empty() => OpResult {
                        path: p,
                        ok: true,
                        message: "无改动，跳过".to_string(),
                    },
                    _ => {
                        let msg = if label.trim().is_empty() {
                            "RepoPilot stash".to_string()
                        } else {
                            label.trim().to_string()
                        };
                        let args: Vec<String> = if include_untracked {
                            vec!["stash".into(), "push".into(), "-u".into(), "-m".into(), msg]
                        } else {
                            vec!["stash".into(), "push".into(), "-m".into(), msg]
                        };
                        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        match run_git_timeout(dir, &arg_refs, 60).map_err(|e| friendly_git_err(&e)) {
                            Ok(out) => OpResult {
                                path: p,
                                ok: true,
                                message: if out.is_empty() { "已暂存改动".to_string() } else { out },
                            },
                            Err(e) => OpResult {
                                path: p,
                                ok: false,
                                message: e,
                            },
                        }
                    }
                };
                if r.ok {
                    okc.fetch_add(1, Ordering::SeqCst);
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit(
                    "repopilot-progress",
                    BatchProgress {
                        done: d,
                        total,
                        ok: okc.load(Ordering::SeqCst),
                        path: r.path.clone(),
                    },
                );
                r
            })
            .await
            .unwrap_or_else(|_| OpResult {
                path: "未知".to_string(),
                ok: false,
                message: "后台任务失败".to_string(),
            })
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.unwrap_or_else(|_| OpResult {
            path: "未知".to_string(),
            ok: false,
            message: "后台任务失败".to_string(),
        }));
    }
    results
}

#[tauri::command]
async fn stash_pop_repos(app: tauri::AppHandle, paths: Vec<String>) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    BATCH_CANCEL.store(false, Ordering::SeqCst);
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        let sem = Arc::clone(&sem);
        handles.push(tauri::async_runtime::spawn(async move {
            let _perm = sem.acquire().await.expect("semaphore closed");
            tauri::async_runtime::spawn_blocking(move || {
                if BATCH_CANCEL.load(Ordering::SeqCst) {
                    return OpResult { path: p, ok: false, message: "已取消".to_string() };
                }
                let dir = Path::new(&p);
                // 无 stash 则跳过
                let r = match run_git(dir, &["stash", "list"]) {
                    Ok(s) if s.trim().is_empty() => OpResult {
                        path: p,
                        ok: true,
                        message: "无 stash，跳过".to_string(),
                    },
                    _ => match run_git_timeout(dir, &["stash", "pop"], 60).map_err(|e| friendly_git_err(&e)) {
                        Ok(out) => OpResult {
                            path: p,
                            ok: true,
                            message: if out.is_empty() { "已恢复改动".to_string() } else { out },
                        },
                        Err(e) => OpResult {
                            path: p,
                            ok: false,
                            message: e,
                        },
                    },
                };
                if r.ok {
                    okc.fetch_add(1, Ordering::SeqCst);
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit(
                    "repopilot-progress",
                    BatchProgress {
                        done: d,
                        total,
                        ok: okc.load(Ordering::SeqCst),
                        path: r.path.clone(),
                    },
                );
                r
            })
            .await
            .unwrap_or_else(|_| OpResult {
                path: "未知".to_string(),
                ok: false,
                message: "后台任务失败".to_string(),
            })
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.unwrap_or_else(|_| OpResult {
            path: "未知".to_string(),
            ok: false,
            message: "后台任务失败".to_string(),
        }));
    }
    results
}

#[derive(Serialize, Clone)]
struct CommitInfo {
    hash: String,
    author: String,
    time: i64,
    subject: String,
}

/// 读取最近提交历史
#[tauri::command]
fn get_log(path: String, count: i64) -> Result<Vec<CommitInfo>, String> {
    let dir = Path::new(&path);
    let n = count.clamp(1, 50);
    // 用 \x1f 分隔，避免提交信息含 | 造成解析错位
    let out = run_git(
        dir,
        &["log", "-n", &n.to_string(), "--pretty=format:%h%x1f%an%x1f%at%x1f%s"],
    )?;
    let mut list = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(4, '\x1f');
        let hash = parts.next().unwrap_or("").to_string();
        let author = parts.next().unwrap_or("").to_string();
        let time = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
        let subject = parts.next().unwrap_or("").to_string();
        list.push(CommitInfo {
            hash,
            author,
            time,
            subject,
        });
    }
    Ok(list)
}

#[derive(Serialize)]
struct GraphCommit {
    hash: String,       // 完整 hash
    short: String,      // 短 hash（显示）
    parents: Vec<String>, // 父提交完整 hash
    author: String,
    time: i64,
    subject: String,
    refs: Vec<String>,  // 该提交所在的分支 / 标签
}

/// 读取提交图数据（含父提交关系与 refs），用于可视化分支图谱
#[tauri::command]
fn get_graph(path: String, count: i64) -> Result<Vec<GraphCommit>, String> {
    let dir = Path::new(&path);
    let n = count.clamp(1, 2000);
    let out = run_git(
        dir,
        &[
            "log",
            "--all",
            "--date-order",
            "-n",
            &n.to_string(),
            "--pretty=format:%H%x1f%P%x1f%an%x1f%at%x1f%s",
        ],
    )?;
    let mut map: std::collections::HashMap<String, GraphCommit> = std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(5, '\x1f');
        let hash = parts.next().unwrap_or("").to_string();
        let parents_raw = parts.next().unwrap_or("").to_string();
        let author = parts.next().unwrap_or("").to_string();
        let time = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
        let subject = parts.next().unwrap_or("").to_string();
        let parents: Vec<String> = if parents_raw.is_empty() {
            Vec::new()
        } else {
            parents_raw.split(' ').map(|s| s.to_string()).collect()
        };
        let short: String = hash.chars().take(8).collect();
        map.insert(
            hash.clone(),
            GraphCommit {
                hash: hash.clone(),
                short,
                parents,
                author,
                time,
                subject,
                refs: Vec::new(),
            },
        );
        order.push(hash);
    }
    // 映射提交 -> 分支 / 标签名
    if let Ok(raw) = run_git(
        dir,
        &[
            "for-each-ref",
            "--format=%(objectname)%x1f%(refname:short)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    ) {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.splitn(2, '\x1f');
            let obj = it.next().unwrap_or("").to_string();
            let name = it.next().unwrap_or("").to_string();
            if name.ends_with("/HEAD") {
                continue;
            }
            if let Some(c) = map.get_mut(&obj) {
                c.refs.push(name);
            }
        }
    }
    let mut list = Vec::new();
    for h in order {
        if let Some(c) = map.remove(&h) {
            list.push(c);
        }
    }
    Ok(list)
}

#[tauri::command]
async fn run_command(app: tauri::AppHandle, paths: Vec<String>, command: String) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    BATCH_CANCEL.store(false, Ordering::SeqCst);
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let cmd = command.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        let sem = Arc::clone(&sem);
        handles.push(tauri::async_runtime::spawn(async move {
            let _perm = sem.acquire().await.expect("semaphore closed");
            tauri::async_runtime::spawn_blocking(move || {
                if BATCH_CANCEL.load(Ordering::SeqCst) {
                    return OpResult { path: p, ok: false, message: "已取消".to_string() };
                }
                // 在仓库目录执行用户自定义命令（sh -c；命令由用户本人输入，等同在终端手动执行）
                // 带超时并 kill 挂起的命令进程，stdin 置空避免交互式挂起
                let out = run_shell_timeout(&cmd, Path::new(&p), 120);
                let r = match out {
                    Ok(o) => {
                        let out_txt = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        let err_txt = String::from_utf8_lossy(&o.stderr).trim().to_string();
                        if o.status.success() {
                            let msg = if out_txt.is_empty() {
                                "命令执行成功".to_string()
                            } else {
                                out_txt
                            };
                            OpResult {
                                path: p,
                                ok: true,
                                message: msg,
                            }
                        } else {
                            let msg = if err_txt.is_empty() {
                                "命令执行失败".to_string()
                            } else {
                                friendly_git_err(&err_txt)
                            };
                            OpResult {
                                path: p,
                                ok: false,
                                message: msg,
                            }
                        }
                    }
                    Err(e) => OpResult {
                        path: p,
                        ok: false,
                        message: e,
                    },
                };
                if r.ok {
                    okc.fetch_add(1, Ordering::SeqCst);
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit(
                    "repopilot-progress",
                    BatchProgress {
                        done: d,
                        total,
                        ok: okc.load(Ordering::SeqCst),
                        path: r.path.clone(),
                    },
                );
                r
            })
            .await
            .unwrap_or_else(|_| OpResult {
                path: "未知".to_string(),
                ok: false,
                message: "后台任务失败".to_string(),
            })
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.unwrap_or_else(|_| OpResult {
            path: "未知".to_string(),
            ok: false,
            message: "后台任务失败".to_string(),
        }));
    }
    results
}

/// 同步更新仓库目录下的 .gitmodules：把 url 行中匹配旧串的部分替换为新串。
/// 返回 Ok(true) 表示有改动并已写回；Ok(false) 表示无 .gitmodules 或无匹配。
fn update_gitmodules(dir: &Path, old: &str, new: &str) -> Result<bool, String> {
    let gm = dir.join(".gitmodules");
    if !gm.exists() {
        return Ok(false);
    }
    let content =
        std::fs::read_to_string(&gm).map_err(|e| format!("读取 .gitmodules 失败：{e}"))?;
    let mut changed = false;
    let new_content = content
        .lines()
        .map(|line| {
            // 仅处理 submodule 的 url 配置行，避免误伤其他字段
            if line.trim_start().starts_with("url") && line.contains(old) {
                changed = true;
                line.replace(old, new)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if changed {
        std::fs::write(&gm, new_content).map_err(|e| format!("写入 .gitmodules 失败：{e}"))?;
    }
    Ok(changed)
}

#[tauri::command]
async fn replace_remotes(app: tauri::AppHandle, paths: Vec<String>, old: String, new: String) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    BATCH_CANCEL.store(false, Ordering::SeqCst);
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let old = old.clone();
        let new = new.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        let sem = Arc::clone(&sem);
        handles.push(tauri::async_runtime::spawn(async move {
            let _perm = sem.acquire().await.expect("semaphore closed");
            tauri::async_runtime::spawn_blocking(move || {
                if BATCH_CANCEL.load(Ordering::SeqCst) {
                    return OpResult { path: p, ok: false, message: "已取消".to_string() };
                }
                let dir = Path::new(&p);
                let r = match run_git(dir, &["remote", "get-url", "origin"]) {
                    Ok(u) => {
                        if !u.contains(&old) {
                            OpResult {
                                path: p,
                                ok: false,
                                message: format!("地址不含旧串，跳过：{u}"),
                            }
                        } else {
                            let new_url = u.replace(&old, &new);
                            match run_git(dir, &["remote", "set-url", "origin", &new_url]) {
                                Ok(_) => {
                                    let mut msg = format!("{u}  →  {new_url}");
                                    // 同步更新 .gitmodules（如有）
                                    match update_gitmodules(dir, &old, &new) {
                                        Ok(true) => msg.push_str("；已同步更新 .gitmodules"),
                                        Ok(false) => {}
                                        Err(e) => msg.push_str(&format!("；警告：{e}")),
                                    }
                                    OpResult {
                                        path: p,
                                        ok: true,
                                        message: msg,
                                    }
                                }
                                Err(e) => OpResult {
                                    path: p,
                                    ok: false,
                                    message: e,
                                },
                            }
                        }
                    }
                    Err(e) => OpResult {
                        path: p,
                        ok: false,
                        message: format!("读取 remote 失败：{e}"),
                    },
                };
                if r.ok {
                    okc.fetch_add(1, Ordering::SeqCst);
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit(
                    "repopilot-progress",
                    BatchProgress {
                        done: d,
                        total,
                        ok: okc.load(Ordering::SeqCst),
                        path: r.path.clone(),
                    },
                );
                r
            })
            .await
            .unwrap_or_else(|_| OpResult {
                path: "未知".to_string(),
                ok: false,
                message: "后台任务失败".to_string(),
            })
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.unwrap_or_else(|_| OpResult {
            path: "未知".to_string(),
            ok: false,
            message: "后台任务失败".to_string(),
        }));
    }
    results
}

#[derive(Serialize, Clone)]
struct ChangeFile {
    path: String,
    status: String,
}

/// 列出仓库的改动文件（git status --porcelain），用于部分提交
#[tauri::command]
fn list_changes(path: String) -> Result<Vec<ChangeFile>, String> {
    let dir = Path::new(&path);
    let out = run_git(dir, &["status", "--porcelain=v1"])?;
    let mut list = Vec::new();
    for line in out.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = line[..2].to_string();
        let mut p = line[3..].to_string();
        // 路径含空格时 git 用引号包裹，去掉引号并反转义
        if p.len() >= 2 && p.starts_with('"') && p.ends_with('"') {
            p = p[1..p.len() - 1].replace("\\\\", "\\").replace("\\\"", "\"");
        }
        list.push(ChangeFile { path: p, status });
    }
    Ok(list)
}

/// 部分提交：仅 add 指定的文件再 commit，未勾选的文件保留在工作区
#[tauri::command]
fn commit_files(path: String, files: Vec<String>, message: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    let msg = message.trim();
    if msg.is_empty() {
        return Err("提交信息不能为空".to_string());
    }
    if files.is_empty() {
        return Err("请选择要提交的文件".to_string());
    }
    let mut add_args = vec!["add", "--"]; // "--" 防止以 - 开头的文件名被 git 当作选项
    for f in &files {
        add_args.push(f.as_str());
    }
    if let Err(e) = run_git(dir, &add_args) {
        return Err(e);
    }
    match run_git(dir, &["commit", "-m", msg]) {
        Ok(out) => {
            let short = if out.is_empty() { "提交成功".to_string() } else { out };
            Ok(OpResult {
                path: path.clone(),
                ok: true,
                message: short,
            })
        }
        Err(e) => Err(e),
    }
}

/// 撤销暂存：把已暂存的文件移出暂存区（git reset HEAD -- <file>）
#[tauri::command]
fn unstage_file(path: String, file: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    run_git(dir, &["reset", "HEAD", "--", &file]).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已取消暂存 {file}"),
    })
}

/// 放弃改动：已跟踪文件还原工作树（git checkout -- <file>），未跟踪文件直接删除
#[tauri::command]
fn discard_file(path: String, file: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    let out = run_git(dir, &["status", "--porcelain=v1", "--", &file]).unwrap_or_default();
    let untracked = out.lines().any(|l| l.starts_with("??"));
    if untracked {
        let full = dir.join(&file);
        if full.is_dir() {
            std::fs::remove_dir_all(&full).map_err(|e| format!("删除目录失败：{e}"))?;
        } else if full.exists() {
            std::fs::remove_file(&full).map_err(|e| format!("删除文件失败：{e}"))?;
        }
    } else {
        run_git(dir, &["checkout", "--", &file]).map_err(|e| friendly_git_err(&e))?;
    }
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已放弃 {file} 的改动"),
    })
}

/// 查看单个文件的改动内容（含已暂存 + 未暂存 diff；未跟踪新文件则返回文件内容）
#[tauri::command]
fn get_file_diff(path: String, file: String) -> Result<String, String> {
    let dir = Path::new(&path);
    let mut body = String::new();
    if let Ok(d) = run_git(dir, &["diff", "--", &file]) {
        if !d.is_empty() {
            body.push_str(&d);
        }
    }
    if let Ok(d) = run_git(dir, &["diff", "--cached", "--", &file]) {
        if !d.is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&d);
        }
    }
    // 未跟踪的新文件 diff 为空：直接读文件内容供查看（每行加 + 前缀，前端统一按新增行高亮）
    if body.is_empty() {
        let full = dir.join(&file);
        if let Ok(content) = std::fs::read_to_string(&full) {
            let lines: Vec<String> = content.lines().map(|l| format!("+ {l}")).collect();
            body = format!("(新文件，未跟踪，以下为当前内容)\n{}", lines.join("\n"));
        }
    }
    if body.is_empty() {
        Err("该文件没有可查看的改动或内容".to_string())
    } else {
        Ok(body)
    }
}

/// 单个 Hunk 块：patch 是可直接 `git apply --cached` 的完整补丁（文件头 + 该 @@ 块），lines 供前端渲染高亮
#[derive(Serialize)]
struct HunkInfo {
    patch: String,
    lines: Vec<String>,
}

#[derive(Serialize)]
struct FileHunks {
    header: Vec<String>,
    hunks: Vec<HunkInfo>,
    /// 文件已有部分改动在暂存区，hunk 补丁上下文会不匹配，禁止块级暂存
    partial: bool,
    /// 未跟踪新文件，没有 diff，只能整文件提交
    untracked: bool,
    /// 无法提供 hunk 时（partial/untracked）的可查看内容：untracked 为文件内容，partial 为未暂存 diff
    content: Option<String>,
}

/// 解析单个文件的未暂存 diff 为 Hunk 块（供行级/块级暂存）
#[tauri::command]
fn get_hunks(path: String, file: String) -> Result<FileHunks, String> {
    let dir = Path::new(&path);
    let diff = run_git(dir, &["diff", "--", &file]).unwrap_or_default();
    if diff.trim().is_empty() {
        // 无未暂存改动：若 cached 也无，则视为未跟踪新文件，返回其内容供查看
        let cached = run_git(dir, &["diff", "--cached", "--", &file]).unwrap_or_default();
        let untracked = cached.trim().is_empty();
        let content = if untracked {
            std::fs::read_to_string(dir.join(&file)).ok()
        } else {
            None
        };
        return Ok(FileHunks {
            header: vec![],
            hunks: vec![],
            partial: false,
            untracked,
            content,
        });
    }
    // 已有部分暂存 → 块级暂存不可用（上下文基于工作树，与 index 不匹配），仅提供未暂存 diff 查看
    let cached = run_git(dir, &["diff", "--cached", "--", &file]).unwrap_or_default();
    if !cached.trim().is_empty() {
        return Ok(FileHunks {
            header: vec![],
            hunks: vec![],
            partial: true,
            untracked: false,
            content: Some(diff),
        });
    }
    let mut header: Vec<String> = vec![];
    let mut hunks: Vec<HunkInfo> = vec![];
    let mut cur: Vec<String> = vec![];
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("@@") {
            if in_hunk {
                let hlines = std::mem::take(&mut cur);
                let patch = format!("{}\n{}\n", header.join("\n"), hlines.join("\n"));
                hunks.push(HunkInfo { patch, lines: hlines });
            }
            in_hunk = true;
            cur.push(line.to_string());
        } else if in_hunk {
            cur.push(line.to_string());
        } else {
            header.push(line.to_string());
        }
    }
    if in_hunk {
        let hlines = std::mem::take(&mut cur);
        let patch = format!("{}\n{}\n", header.join("\n"), hlines.join("\n"));
        hunks.push(HunkInfo { patch, lines: hlines });
    }
    Ok(FileHunks {
        header,
        hunks,
        partial: false,
        untracked: false,
        content: None,
    })
}

/// 把某个 Hunk 块暂存（git apply --cached 应用到暂存区）
#[tauri::command]
fn stage_hunk(path: String, patch: String) -> Result<OpResult, String> {
    if patch.trim().is_empty() {
        return Err("补丁内容为空".to_string());
    }
    let dir = Path::new(&path);
    run_git_stdin(dir, &["apply", "--cached", "--whitespace=nowarn"], &patch)
        .map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: "已暂存该改动块".to_string(),
    })
}

/// 从 git URL 提取仓库名（支持 git@host:user/repo.git 与 https://host/user/repo.git）
fn repo_name_from_url(url: &str) -> Result<String, String> {
    let s = url.trim_end_matches('/');
    let base = s.rsplit(['/', ':']).next().unwrap_or(s).trim_end_matches(".git");
    if base.is_empty() {
        return Err("无法从 URL 识别仓库名".to_string());
    }
    Ok(base.to_string())
}

/// 克隆新仓库到指定根目录
#[tauri::command]
fn clone_repo(url: String, base_dir: String) -> Result<OpResult, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("请填写仓库 URL".to_string());
    }
    let name = repo_name_from_url(&url)?;
    let base = Path::new(&base_dir);
    let target = base.join(&name);
    if target.exists() {
        return Err(format!("目标目录已存在：{}（请换根目录或先删除）", target.display()));
    }
    let url_c = url.clone();
    let name_c = name.clone();
    match run_git_timeout(base, &["clone", "--", &url_c, &name_c], 120)
        .map_err(|e| friendly_git_err(&e))
    {
        Ok(_) => Ok(OpResult {
            path: target.display().to_string(),
            ok: true,
            message: "克隆成功".to_string(),
        }),
        Err(e) => {
            // 克隆失败时清理可能的残留目录
            let _ = std::fs::remove_dir_all(&target);
            Err(e)
        }
    }
}

#[tauri::command]
fn save_roots(app: tauri::AppHandle, roots: Vec<String>) -> Result<(), String> {
    let dir = app_config_dir(&app)?;
    let json = serde_json::to_string_pretty(&roots).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("state.json"), format!("{{\"roots\":{json}}}"))
        .map_err(|e| e.to_string())
}

/// 读取根目录列表；兼容旧版 {"root": "..."} 格式
#[tauri::command]
fn load_roots(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let dir = app_config_dir(&app)?;
    let file = dir.join("state.json");
    if !file.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
        if let Some(arr) = v.get("roots").and_then(|r| r.as_array()) {
            let list: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect();
            if !list.is_empty() {
                return Ok(list);
            }
        }
        if let Some(r) = v.get("root").and_then(|r| r.as_str()) {
            return Ok(vec![r.to_string()]);
        }
    }
    // 兜底：旧版手写 JSON 解析
    if let Some(idx) = content.find("\"root\":\"") {
        let rest = &content[idx + "\"root\":\"".len()..];
        if let Some(end) = rest.find('"') {
            let raw = rest[..end].to_string();
            return Ok(vec![raw.replace("\\\"", "\"").replace("\\\\", "\\")]);
        }
    }
    Ok(Vec::new())
}

/// 保存收藏的仓库路径，存 favs.json
#[tauri::command]
fn save_favs(app: tauri::AppHandle, paths: Vec<String>) -> Result<(), String> {
    let dir = app_config_dir(&app)?;
    let json = serde_json::to_string_pretty(&paths).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("favs.json"), json).map_err(|e| e.to_string())
}

/// 读取收藏的仓库路径
#[tauri::command]
fn load_favs(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let dir = app_config_dir(&app)?;
    let file = dir.join("favs.json");
    if !file.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    Ok(serde_json::from_str::<Vec<String>>(&content).unwrap_or_default())
}

/// 仓库别名：仓库路径 -> 显示别名，存 aliases.json
#[derive(Serialize, Deserialize, Default)]
struct AliasState(HashMap<String, String>);

#[tauri::command]
fn save_aliases(app: tauri::AppHandle, state: AliasState) -> Result<(), String> {
    let dir = app_config_dir(&app)?;
    let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("aliases.json"), json).map_err(|e| e.to_string())
}

#[tauri::command]
fn load_aliases(app: tauri::AppHandle) -> Result<AliasState, String> {
    let dir = app_config_dir(&app)?;
    let file = dir.join("aliases.json");
    if !file.exists() {
        return Ok(AliasState::default());
    }
    let content = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    Ok(serde_json::from_str::<AliasState>(&content).unwrap_or_default())
}

/// 导出配置到用户选择的文件（数据由前端组装为 JSON 字符串）
#[tauri::command]
fn export_config(path: String, data: String) -> Result<(), String> {
    std::fs::write(&path, data).map_err(|e| e.to_string())
}

/// 读取用户选择的配置文件内容（返回原始 JSON，由前端解析并应用）
#[tauri::command]
fn import_config(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// 批量检查目录是否存在（用于导入配置时过滤无效根目录）
#[tauri::command]
fn check_dirs(paths: Vec<String>) -> Vec<bool> {
    paths.iter().map(|p| PathBuf::from(p).is_dir()).collect()
}

fn app_config_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 分组状态：names 是独立的分组名列表，assign 是 仓库路径 -> 分组名
#[derive(Serialize, Deserialize, Default)]
struct GroupState {
    names: Vec<String>,
    assign: HashMap<String, String>,
}

/// 保存仓库分组，存 groups.json
#[tauri::command]
fn save_groups(app: tauri::AppHandle, state: GroupState) -> Result<(), String> {
    let dir = app_config_dir(&app)?;
    let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("groups.json"), json).map_err(|e| e.to_string())
}

/// 读取仓库分组；兼容旧版 {path: group} 格式
#[tauri::command]
fn load_groups(app: tauri::AppHandle) -> Result<GroupState, String> {
    let dir = app_config_dir(&app)?;
    let file = dir.join("groups.json");
    if !file.exists() {
        return Ok(GroupState::default());
    }
    let content = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    if let Ok(s) = serde_json::from_str::<GroupState>(&content) {
        return Ok(s);
    }
    if let Ok(old) = serde_json::from_str::<HashMap<String, String>>(&content) {
        let mut names: Vec<String> = old.values().cloned().collect();
        names.sort();
        names.dedup();
        return Ok(GroupState { names, assign: old });
    }
    Ok(GroupState::default())
}

/// 列出仓库的可用分支（本地 + 远程名去重，去掉 origin/HEAD）
/// 注意：本地分支名可能含斜杠（如 feature/login），必须保留完整短名，不能按 '/' 截断；
/// 仅远程分支（origin/xxx）才去掉 remote 前缀。
#[tauri::command]
fn list_branches(path: String) -> Vec<String> {
    let dir = Path::new(&path);
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    // 本地分支：短名即完整分支名（本地优先）
    if let Ok(raw) = run_git(
        dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    ) {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if seen.insert(line.to_string()) {
                out.push(line.to_string());
            }
        }
    }
    // 远程分支：origin/dev → dev；origin/feature/foo → feature/foo（只去掉 remote 前缀）
    if let Ok(raw) = run_git(
        dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
    ) {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.ends_with("/HEAD") {
                continue;
            }
            let short = match line.find('/') {
                Some(idx) => line[idx + 1..].to_string(),
                None => line.to_string(),
            };
            if seen.insert(short.clone()) {
                out.push(short);
            }
        }
    }
    out.sort();
    out
}

/// 批量切换分支：对每个仓库执行 git switch {branch}
#[tauri::command]
async fn switch_branches(app: tauri::AppHandle, paths: Vec<String>, branch: String) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
    BATCH_CANCEL.store(false, Ordering::SeqCst);
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let branch = branch.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        let sem = Arc::clone(&sem);
        handles.push(tauri::async_runtime::spawn(async move {
            let _perm = sem.acquire().await.expect("semaphore closed");
            tauri::async_runtime::spawn_blocking(move || {
                if BATCH_CANCEL.load(Ordering::SeqCst) {
                    return OpResult { path: p, ok: false, message: "已取消".to_string() };
                }
                let dir = Path::new(&p);
                let r = match run_git(dir, &["switch", &branch]) {
                    Ok(_) => OpResult {
                        path: p,
                        ok: true,
                        message: format!("已切换到分支 {branch}"),
                    },
                    Err(e) => OpResult {
                        path: p,
                        ok: false,
                        message: e,
                    },
                };
                if r.ok {
                    okc.fetch_add(1, Ordering::SeqCst);
                }
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app.emit(
                    "repopilot-progress",
                    BatchProgress {
                        done: d,
                        total,
                        ok: okc.load(Ordering::SeqCst),
                        path: r.path.clone(),
                    },
                );
                r
            })
            .await
            .unwrap_or_else(|_| OpResult {
                path: "未知".to_string(),
                ok: false,
                message: "后台任务失败".to_string(),
            })
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.unwrap_or_else(|_| OpResult {
            path: "未知".to_string(),
            ok: false,
            message: "后台任务失败".to_string(),
        }));
    }
    results
}

#[derive(Serialize)]
struct BranchInfo {
    name: String,
    is_local: bool,
    is_remote: bool,
    is_current: bool,
}

/// 分支管理：返回本地 + 远程分支列表（标注当前分支）
#[tauri::command]
fn get_branches(path: String) -> Result<Vec<BranchInfo>, String> {
    let dir = Path::new(&path);
    let current = run_git(dir, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_default();
    let current = current.trim().to_string();
    let mut out: Vec<BranchInfo> = Vec::new();
    if let Ok(raw) = run_git(dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]) {
        for line in raw.lines() {
            let name = line.trim().to_string();
            if name.is_empty() {
                continue;
            }
            out.push(BranchInfo {
                name: name.clone(),
                is_local: true,
                is_remote: false,
                is_current: name == current,
            });
        }
    }
    if let Ok(raw) = run_git(dir, &["for-each-ref", "--format=%(refname:short)", "refs/remotes"]) {
        for line in raw.lines() {
            let name = line.trim().to_string();
            if name.is_empty() || name.ends_with("/HEAD") {
                continue;
            }
            out.push(BranchInfo {
                name,
                is_local: false,
                is_remote: true,
                is_current: false,
            });
        }
    }
    Ok(out)
}

fn valid_branch_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("分支名不能为空".to_string());
    }
    for ch in name.chars() {
        if ch == ' '
            || ch == '~'
            || ch == '^'
            || ch == ':'
            || ch == '?'
            || ch == '*'
            || ch == '['
            || ch == '\\'
        {
            return Err("分支名包含非法字符".to_string());
        }
    }
    if name.contains("..") || name.contains("@{") {
        return Err("分支名包含非法字符".to_string());
    }
    Ok(())
}

/// 基于当前 HEAD 创建本地分支
#[tauri::command]
fn create_branch(path: String, name: String) -> Result<OpResult, String> {
    valid_branch_name(&name)?;
    let dir = Path::new(&path);
    match run_git(dir, &["branch", name.trim()]) {
        Ok(_) => Ok(OpResult {
            path,
            ok: true,
            message: format!("已创建分支 {}", name.trim()),
        }),
        Err(e) => Err(e),
    }
}

/// 把指定分支合并到当前分支
#[tauri::command]
fn merge_branch(path: String, name: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    match run_git(dir, &["merge", "--no-edit", &name]) {
        Ok(out) => Ok(OpResult {
            path,
            ok: true,
            message: if out.is_empty() {
                format!("已合并分支 {name}")
            } else {
                out
            },
        }),
        Err(e) => Err(e),
    }
}

/// 删除本地分支（force=true 用 -D 强制）
#[tauri::command]
fn delete_branch(path: String, name: String, force: bool) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    let flag = if force { "-D" } else { "-d" };
    match run_git(dir, &["branch", flag, &name]) {
        Ok(_) => Ok(OpResult {
            path,
            ok: true,
            message: format!("已删除分支 {name}"),
        }),
        Err(e) => Err(e),
    }
}

/// 把指定提交 Cherry-pick 到当前分支
#[tauri::command]
fn cherry_pick(path: String, hash: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    match run_git(dir, &["cherry-pick", &hash]) {
        Ok(out) => Ok(OpResult {
            path,
            ok: true,
            message: if out.is_empty() {
                format!("已摘取提交 {hash}")
            } else {
                out
            },
        }),
        Err(e) => Err(e),
    }
}

/// 查看某次提交的改动内容（提交信息 + diff）
#[tauri::command]
fn get_commit_diff(path: String, hash: String) -> Result<String, String> {
    let dir = Path::new(&path);
    run_git(
        dir,
        &["show", "--format=%h %an %at %s", "--stat", &hash],
    )
}

/// 列出本地标签（按创建时间倒序）
#[tauri::command]
fn get_tags(path: String) -> Result<Vec<String>, String> {
    let dir = Path::new(&path);
    let out = run_git(dir, &["tag", "--sort=-creatordate"]).unwrap_or_default();
    Ok(out
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// 在 HEAD 上创建轻量标签
#[tauri::command]
fn create_tag(path: String, name: String) -> Result<OpResult, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("请输入标签名".to_string());
    }
    let dir = Path::new(&path);
    run_git_timeout(dir, &["tag", &name], 30).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已创建标签 {name}"),
    })
}

/// 删除本地标签
#[tauri::command]
fn delete_tag(path: String, name: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    run_git_timeout(dir, &["tag", "-d", &name], 30).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已删除标签 {name}"),
    })
}

/// 推送标签到 origin 远程
#[tauri::command]
fn push_tag(path: String, name: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    let refspec = format!("refs/tags/{name}");
    run_git_timeout(dir, &["push", "origin", &refspec], 60).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已推送标签 {name}"),
    })
}

#[derive(Serialize)]
struct RemoteInfo {
    name: String,
    url: String,
}

/// 列出远程仓库（name + 抓取 url）
#[tauri::command]
fn get_remotes(path: String) -> Result<Vec<RemoteInfo>, String> {
    let dir = Path::new(&path);
    let out = run_git(dir, &["remote", "-v"]).unwrap_or_default();
    let mut map = std::collections::BTreeMap::new();
    for line in out.lines() {
        let mut it = line.split_whitespace();
        let name = it.next().unwrap_or("").to_string();
        let url = it.next().unwrap_or("").to_string();
        if name.is_empty() || url.is_empty() {
            continue;
        }
        map.insert(name, url); // fetch/push 两行 url 相同，覆盖即可
    }
    Ok(map
        .into_iter()
        .map(|(name, url)| RemoteInfo { name, url })
        .collect())
}

/// 新增远程仓库
#[tauri::command]
fn add_remote(path: String, name: String, url: String) -> Result<OpResult, String> {
    let name = name.trim().to_string();
    let url = url.trim().to_string();
    if name.is_empty() || url.is_empty() {
        return Err("请填写远程名称和地址".to_string());
    }
    let dir = Path::new(&path);
    run_git_timeout(dir, &["remote", "add", &name, &url], 30).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已添加远程 {name}"),
    })
}

/// 删除远程仓库
#[tauri::command]
fn remove_remote(path: String, name: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    run_git_timeout(dir, &["remote", "remove", &name], 30).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已删除远程 {name}"),
    })
}

/// 修改远程仓库地址
#[tauri::command]
fn set_remote_url(path: String, name: String, url: String) -> Result<OpResult, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("请输入远程地址".to_string());
    }
    let dir = Path::new(&path);
    run_git_timeout(dir, &["remote", "set-url", &name, &url], 30).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已更新远程 {name} 地址"),
    })
}

#[derive(Serialize, Clone)]
struct StashInfo {
    /// 形如 stash@{0}
    index: String,
    subject: String,
}

/// 列出 stash（git stash list），按栈顶优先
#[tauri::command]
fn get_stash_list(path: String) -> Result<Vec<StashInfo>, String> {
    let dir = Path::new(&path);
    let out = run_git(dir, &["stash", "list", "--format=%gd%x09%gs"])?;
    let mut list = Vec::new();
    for line in out.lines() {
        let mut it = line.splitn(2, '\t');
        let index = it.next().unwrap_or("").trim().to_string();
        let subject = it.next().unwrap_or("").trim().to_string();
        if index.is_empty() {
            continue;
        }
        list.push(StashInfo { index, subject });
    }
    Ok(list)
}

/// 新建 stash（只暂存已跟踪改动，label 为可选的备注）
#[tauri::command]
fn stash_create(path: String, label: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("请输入 stash 备注".to_string());
    }
    // git stash push 在无改动时可能输出 "No local changes to save" 但仍退出 0，需前置检测避免误报成功
    if !has_local_changes(dir) {
        return Err("没有可暂存的改动（工作区干净）".to_string());
    }
    run_git_timeout(dir, &["stash", "push", "-m", &label], 30).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已暂存改动（{label}）"),
    })
}

/// 恢复指定 stash（git stash pop stash@{n}）
#[tauri::command]
fn stash_pop_one(path: String, index: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    let safe = index.trim().to_string();
    if safe.is_empty() {
        return Err("stash 编号无效".to_string());
    }
    run_git_timeout(dir, &["stash", "pop", &safe], 60).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已恢复 {safe}"),
    })
}

/// 删除指定 stash（git stash drop stash@{n}）
#[tauri::command]
fn stash_drop(path: String, index: String) -> Result<OpResult, String> {
    let dir = Path::new(&path);
    let safe = index.trim().to_string();
    if safe.is_empty() {
        return Err("stash 编号无效".to_string());
    }
    run_git_timeout(dir, &["stash", "drop", &safe], 30).map_err(|e| friendly_git_err(&e))?;
    Ok(OpResult {
        path,
        ok: true,
        message: format!("已删除 {safe}"),
    })
}

/// 在仓库目录打开终端（mac: Terminal；windows: 新开 cmd 窗口）
#[tauri::command]
fn open_terminal(path: String) -> Result<(), String> {
    #[cfg(not(windows))]
    let out = Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(&path)
        .output()
        .map_err(|e| format!("无法打开终端：{e}"))?;
    #[cfg(windows)]
    let out = {
        use std::os::windows::process::CommandExt;
        Command::new("cmd")
            .args(["/C", "start", "", "cmd"])
            .current_dir(&path)
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| format!("无法打开终端：{e}"))?
    };
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            "打开终端失败".to_string()
        } else {
            err
        })
    }
}

// 用系统文本编辑器打开仓库的 .git/config（mac: TextEdit；windows: 记事本）
#[tauri::command]
fn open_git_config(path: String) -> Result<(), String> {
    let cfg = std::path::Path::new(&path).join(".git").join("config");
    if !cfg.exists() {
        return Err("未找到 .git/config".to_string());
    }
    #[cfg(not(windows))]
    let out = Command::new("open")
        .arg("-e")
        .arg(&cfg)
        .output()
        .map_err(|e| format!("无法打开配置文件：{e}"))?;
    #[cfg(windows)]
    let out = Command::new("notepad")
        .arg(&cfg)
        .output()
        .map_err(|e| format!("无法打开配置文件：{e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            "打开配置文件失败".to_string()
        } else {
            err
        })
    }
}

// 检查路径是否存在（添加根目录时校验）
#[tauri::command]
fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).exists()
}

// 返回用户家目录（用于 ~ 展开）
#[tauri::command]
fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // 精简 macOS 菜单栏：只保留应用菜单（关于 / 退出），去掉 File/Edit/View/Window/Help 空壳菜单
            use tauri::menu::{AboutMetadata, MenuBuilder, PredefinedMenuItem, SubmenuBuilder};
            let about = PredefinedMenuItem::about(app, None, Some(AboutMetadata::default()))?;
            let quit = PredefinedMenuItem::quit(app, None)?;
            let app_menu = SubmenuBuilder::new(app, "RepoPilot")
                .item(&about)
                .separator()
                .item(&quit)
                .build()?;
            let menu = MenuBuilder::new(app).item(&app_menu).build()?;
            app.set_menu(menu)?;
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            scan_repos,
            get_statuses,
            pull_repos,
            push_repos,
            run_command,
            replace_remotes,
            save_roots,
            load_roots,
            save_groups,
            load_groups,
            save_favs,
            load_favs,
            save_aliases,
            load_aliases,
            switch_branches,
            cancel_batch,
            list_branches,
            get_branches,
            create_branch,
            merge_branch,
            delete_branch,
            cherry_pick,
            get_commit_diff,
            get_tags,
            create_tag,
            delete_tag,
            push_tag,
            get_remotes,
            add_remote,
            remove_remote,
            set_remote_url,
            get_stash_list,
            stash_create,
            stash_pop_one,
            stash_drop,
            open_terminal,
            list_changes,
            get_file_diff,
            get_hunks,
            stage_hunk,
            unstage_file,
            discard_file,
            commit_files,
            check_remote_conflicts,
            run_git_auth,
            open_git_config,
            path_exists,
            home_dir,
            clone_repo,
            stash_repos,
            stash_pop_repos,
            get_log,
            get_graph,
            export_config,
            import_config,
            check_dirs
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_update_gitmodules_replaces_url_only() {
        let dir = std::env::temp_dir().join("repopilot_gm_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let gm = dir.join(".gitmodules");
        fs::write(
            &gm,
            "[submodule \"sub/a\"]\n\tpath = sub/a\n\turl = git@gitlab.old.com:group/a.git\n\
             [submodule \"sub/b\"]\n\tpath = sub/b\n\turl = https://gitlab.old.com/group/b.git\n\
             [submodule \"sub/c\"]\n\tpath = sub/c\n\turl = https://other.com/c.git\n",
        )
        .unwrap();

        // 替换域名
        let changed = update_gitmodules(&dir, "gitlab.old.com", "gitlab.new.com").unwrap();
        assert!(changed, "应识别到需要变更");

        let content = fs::read_to_string(&gm).unwrap();
        assert!(
            content.contains("url = git@gitlab.new.com:group/a.git"),
            "ssh 格式 submodule 未替换"
        );
        assert!(
            content.contains("url = https://gitlab.new.com/group/b.git"),
            "https 格式 submodule 未替换"
        );
        assert!(
            content.contains("https://other.com/c.git"),
            "无关地址不应被改动"
        );
        assert!(
            !content.contains("gitlab.old.com"),
            "旧地址应被全部清除"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_update_gitmodules_no_file_returns_false() {
        let dir = std::env::temp_dir().join("repopilot_gm_none");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let changed = update_gitmodules(&dir, "old", "new").unwrap();
        assert!(!changed, "无 .gitmodules 时应返回 false");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_count_ahead_behind() {
        // git status --porcelain=v1 -b 的典型分支行：## main...origin/main [ahead 1, behind 2]
        let bracket = "[ahead 1, behind 2]";
        assert_eq!(parse_count(bracket, "ahead"), 1, "ahead 应解析为 1");
        assert_eq!(parse_count(bracket, "behind"), 2, "behind 应解析为 2");
        // 只有 ahead
        assert_eq!(parse_count("[ahead 3]", "ahead"), 3);
        assert_eq!(parse_count("[ahead 3]", "behind"), 0);
        // 不含关键词
        assert_eq!(parse_count("[gone]", "ahead"), 0);
    }

    #[test]
    fn test_parse_count_branch_line() {
        // 直接喂 get_one_status 中使用的完整 bracket（rest.find('[') 之后的片段）
        let rest = "main...origin/main [ahead 12, behind 3]";
        if let Some(idx) = rest.find('[') {
            let bracket = &rest[idx..];
            assert_eq!(parse_count(bracket, "ahead"), 12);
            assert_eq!(parse_count(bracket, "behind"), 3);
        } else {
            panic!("未找到 [");
        }
    }

    #[test]
    fn test_list_branches_keeps_slash_in_local_branch() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_branch_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let init = new_git_cmd()
            .arg("-C").arg(&dir).arg("init").arg("-b").arg("main")
            .output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd()
            .arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t")
            .output().unwrap();
        let _ = new_git_cmd()
            .arg("-C").arg(&dir).arg("config").arg("user.name").arg("t")
            .output().unwrap();
        fs::write(dir.join("a.txt"), "x").unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        let commit = new_git_cmd()
            .arg("-C").arg(&dir).arg("commit").arg("-m").arg("init")
            .output().unwrap();
        assert!(commit.status.success(), "git commit 失败: {}", String::from_utf8_lossy(&commit.stderr));
        // 创建一个含斜杠的本地分支（feature/login），这是本 bug 的复现点
        let br = new_git_cmd()
            .arg("-C").arg(&dir).arg("branch").arg("feature/login")
            .output().unwrap();
        assert!(br.status.success(), "创建 feature/login 失败");

        let branches = list_branches(dir.to_string_lossy().to_string());
        assert!(
            branches.iter().any(|b| b == "feature/login"),
            "含斜杠的本地分支名被截断，实际返回: {branches:?}"
        );
        assert!(
            !branches.iter().any(|b| b == "login"),
            "不应出现被截断的分支名 login，实际返回: {branches:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_changes_keeps_first_char_of_unstaged_path() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_list_changes_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let init = new_git_cmd()
            .arg("-C").arg(&dir).arg("init").arg("-b").arg("main")
            .output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd()
            .arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t")
            .output().unwrap();
        let _ = new_git_cmd()
            .arg("-C").arg(&dir).arg("config").arg("user.name").arg("t")
            .output().unwrap();
        fs::write(dir.join("a.txt"), "1").unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        let _ = new_git_cmd()
            .arg("-C").arg(&dir).arg("commit").arg("-m").arg("init")
            .output().unwrap();
        // 先提交 src/App.vue，再未暂存修改它：porcelain 输出 " M src/App.vue"（行首空格），
        // 此前 run_git 的 trim() 去掉行首空格导致路径解析为 "rc/App.vue"
        let sub = dir.join("src");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("App.vue"), "v1").unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        let _ = new_git_cmd()
            .arg("-C").arg(&dir).arg("commit").arg("-m").arg("init2")
            .output().unwrap();
        fs::write(sub.join("App.vue"), "v2").unwrap();

        let list = list_changes(dir.to_string_lossy().to_string()).unwrap();
        assert!(
            list.iter().any(|c| c.path == "src/App.vue"),
            "未暂存文件路径被错误截断，实际返回: {:?}",
            list.iter().map(|c| c.path.as_str()).collect::<Vec<_>>()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_get_hunks_splits_and_stages() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_hunk_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let init = new_git_cmd().arg("-C").arg(&dir).arg("init").arg("-b").arg("main").output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.name").arg("t").output().unwrap();
        let f = dir.join("a.txt");
        let content: String = (1..=20).map(|i| format!("line{i}\n")).collect();
        fs::write(&f, &content).unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("commit").arg("-m").arg("init").output().unwrap();

        // 修改两处相隔足够远的内容（line2 与 line18，中间 >6 行未改动）→ 应解析出两个 hunk
        let mut after = String::new();
        for i in 1..=20 {
            if i == 2 {
                after.push_str("line2-CHANGED\n");
            } else if i == 18 {
                after.push_str("line18-CHANGED\n");
            } else {
                after.push_str(&format!("line{i}\n"));
            }
        }
        fs::write(&f, &after).unwrap();

        let path = dir.to_string_lossy().to_string();
        let fh = get_hunks(path.clone(), "a.txt".to_string()).unwrap();
        assert!(!fh.header.is_empty(), "应解析出文件头，实际: {:?}", fh.header);
        assert_eq!(fh.hunks.len(), 2, "两个不相邻改动应拆成两个 hunk，实际: {}", fh.hunks.len());
        assert!(!fh.partial && !fh.untracked, "普通未暂存文件标记应为 false");
        for h in &fh.hunks {
            assert!(h.patch.contains("@@"), "每个 hunk patch 应含 @@ 行");
            assert!(h.patch.contains("diff --git"), "每个 hunk patch 应含文件头（可独立 apply）");
        }

        // 暂存第一个 hunk → index 应有内容，工作树仍有剩余未暂存改动
        let p0 = fh.hunks[0].patch.clone();
        let res = stage_hunk(path.clone(), p0).unwrap();
        assert!(res.ok, "第一个 hunk 暂存应成功");
        let cached = run_git(Path::new(&dir), &["diff", "--cached", "--", "a.txt"]).unwrap_or_default();
        assert!(!cached.trim().is_empty(), "暂存后 index 应有该块改动");
        let unstaged = run_git(Path::new(&dir), &["diff", "--", "a.txt"]).unwrap_or_default();
        assert!(!unstaged.trim().is_empty(), "剩余 hunk 应仍留在工作树");

        // 此时文件已部分暂存 → get_hunks 应标记 partial 且不再提供 hunk
        let fh2 = get_hunks(path.clone(), "a.txt".to_string()).unwrap();
        assert!(fh2.partial, "部分暂存后应标记 partial");
        assert!(fh2.hunks.is_empty(), "部分暂存后不应再提供 hunk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_get_hunks_untracked() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_hunk_untracked");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let init = new_git_cmd().arg("-C").arg(&dir).arg("init").arg("-b").arg("main").output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.name").arg("t").output().unwrap();
        fs::write(dir.join("new.txt"), "hello").unwrap();
        let path = dir.to_string_lossy().to_string();
        let fh = get_hunks(path, "new.txt".to_string()).unwrap();
        assert!(fh.untracked, "未跟踪文件应标记 untracked");
        assert!(fh.hunks.is_empty(), "未跟踪文件不应有 hunk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_has_local_changes() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_dirty_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let init = new_git_cmd().arg("-C").arg(&dir).arg("init").arg("-b").arg("main").output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.name").arg("t").output().unwrap();
        fs::write(dir.join("a.txt"), "v1").unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("commit").arg("-m").arg("init").output().unwrap();

        // 干净仓库（仅未跟踪文件不视为脏）
        fs::write(dir.join("untracked.txt"), "x").unwrap();
        assert!(!has_local_changes(Path::new(&dir)), "仅未跟踪文件不应视为有未提交改动");

        // 未暂存改动 → 脏
        fs::write(dir.join("a.txt"), "v2").unwrap();
        assert!(has_local_changes(Path::new(&dir)), "未暂存改动应视为脏");

        // 暂存改动 → 脏
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        assert!(has_local_changes(Path::new(&dir)), "暂存改动应视为脏");

        // 提交后干净
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("commit").arg("-m").arg("c2").output().unwrap();
        assert!(!has_local_changes(Path::new(&dir)), "提交后应干净");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tag_crud() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_tag_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let init = new_git_cmd().arg("-C").arg(&dir).arg("init").arg("-b").arg("main").output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.name").arg("t").output().unwrap();
        fs::write(dir.join("a.txt"), "v1").unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("commit").arg("-m").arg("init").output().unwrap();

        let path = dir.to_string_lossy().to_string();
        assert!(get_tags(path.clone()).unwrap().is_empty(), "初始应无标签");
        let res = create_tag(path.clone(), "v1.0.0".to_string()).unwrap();
        assert!(res.ok, "创建标签应成功");
        let tags = get_tags(path.clone()).unwrap();
        assert!(tags.iter().any(|t| t == "v1.0.0"), "应能列出新标签，实际: {tags:?}");
        // 重复创建应报错
        assert!(create_tag(path.clone(), "v1.0.0".to_string()).is_err(), "重复标签应报错");
        let res = delete_tag(path.clone(), "v1.0.0".to_string()).unwrap();
        assert!(res.ok, "删除标签应成功");
        assert!(get_tags(path.clone()).unwrap().is_empty(), "删除后应无标签");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_remote_crud() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_remote_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let init = new_git_cmd().arg("-C").arg(&dir).arg("init").arg("-b").arg("main").output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.name").arg("t").output().unwrap();
        let path = dir.to_string_lossy().to_string();

        assert!(get_remotes(path.clone()).unwrap().is_empty(), "初始应无远程");
        let res = add_remote(path.clone(), "origin".to_string(), "https://github.com/x/y.git".to_string()).unwrap();
        assert!(res.ok, "添加远程应成功");
        let remotes = get_remotes(path.clone()).unwrap();
        assert_eq!(remotes.len(), 1, "应列出 1 个远程");
        assert_eq!(remotes[0].name, "origin");
        assert_eq!(remotes[0].url, "https://github.com/x/y.git");
        // 改 url
        let res = set_remote_url(path.clone(), "origin".to_string(), "https://github.com/x/z.git".to_string()).unwrap();
        assert!(res.ok, "改地址应成功");
        let remotes = get_remotes(path.clone()).unwrap();
        assert_eq!(remotes[0].url, "https://github.com/x/z.git");
        // 删
        let res = remove_remote(path.clone(), "origin".to_string()).unwrap();
        assert!(res.ok, "删除远程应成功");
        assert!(get_remotes(path.clone()).unwrap().is_empty(), "删除后应无远程");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_unstage_and_discard() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_unstage_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let init = new_git_cmd().arg("-C").arg(&dir).arg("init").arg("-b").arg("main").output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.name").arg("t").output().unwrap();
        fs::write(dir.join("a.txt"), "v1").unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("commit").arg("-m").arg("init").output().unwrap();
        let path = dir.to_string_lossy().to_string();

        // 已暂存文件 → 取消暂存
        fs::write(dir.join("a.txt"), "v2").unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg("a.txt").output().unwrap();
        let res = unstage_file(path.clone(), "a.txt".to_string()).unwrap();
        assert!(res.ok, "取消暂存应成功");
        let cached = run_git(Path::new(&dir), &["diff", "--cached"]).unwrap_or_default();
        assert!(cached.trim().is_empty(), "取消暂存后 index 应为空");

        // 未暂存改动 → 放弃
        fs::write(dir.join("a.txt"), "v3").unwrap();
        let res = discard_file(path.clone(), "a.txt".to_string()).unwrap();
        assert!(res.ok, "放弃改动应成功");
        let content = fs::read_to_string(dir.join("a.txt")).unwrap();
        assert_eq!(content, "v1", "放弃后应还原到 HEAD 内容");

        // 未跟踪文件 → 放弃（删除）
        fs::write(dir.join("new.txt"), "x").unwrap();
        let res = discard_file(path.clone(), "new.txt".to_string()).unwrap();
        assert!(res.ok, "放弃未跟踪文件应成功");
        assert!(!dir.join("new.txt").exists(), "未跟踪文件应被删除");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_stash_crud() {
        use std::process::Command;
        let dir = std::env::temp_dir().join("repopilot_stash_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let init = new_git_cmd().arg("-C").arg(&dir).arg("init").arg("-b").arg("main").output().unwrap();
        assert!(init.status.success(), "git init 失败");
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.email").arg("t@t").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("config").arg("user.name").arg("t").output().unwrap();
        fs::write(dir.join("a.txt"), "v1").unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("add").arg(".").output().unwrap();
        let _ = new_git_cmd().arg("-C").arg(&dir).arg("commit").arg("-m").arg("init").output().unwrap();
        let path = dir.to_string_lossy().to_string();

        // 空列表
        assert!(get_stash_list(path.clone()).unwrap().is_empty(), "初始应无 stash");

        // 干净仓库 stash 应报错（git stash push 会 "No local changes to save" 但仍 exit 0）
        let res = stash_create(path.clone(), "noop".to_string());
        assert!(res.is_err(), "干净仓库暂存应报错");

        // 新建 stash
        fs::write(dir.join("a.txt"), "v2").unwrap();
        let res = stash_create(path.clone(), "wip".to_string()).unwrap();
        assert!(res.ok, "新建 stash 应成功");
        let list = get_stash_list(path.clone()).unwrap();
        assert_eq!(list.len(), 1, "应有一条 stash");
        assert_eq!(list[0].index, "stash@{0}", "最新 stash 应为 stash@0");
        assert!(list[0].subject.contains("wip"), "stash 信息应含备注");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "v1", "stash 后工作树应还原");

        // 再建一条 → 两条，栈顶变化
        fs::write(dir.join("a.txt"), "v3").unwrap();
        stash_create(path.clone(), "wip2".to_string()).unwrap();
        let list = get_stash_list(path.clone()).unwrap();
        assert_eq!(list.len(), 2, "应有两条 stash");
        assert!(list[0].subject.contains("wip2"), "栈顶应是最新 stash");

        // 恢复 stash@{1}（第一条）→ 工作树 v2
        let res = stash_pop_one(path.clone(), "stash@{1}".to_string()).unwrap();
        assert!(res.ok, "恢复 stash 应成功");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "v2", "恢复后应回到 v2");

        // 删除剩余 stash
        let res = stash_drop(path.clone(), "stash@{0}".to_string()).unwrap();
        assert!(res.ok, "删除 stash 应成功");
        assert!(get_stash_list(path.clone()).unwrap().is_empty(), "删除后应无 stash");
        let _ = fs::remove_dir_all(&dir);
    }
}
