// RepoPilot —— 本地多仓库批量管理工具（MVP 核心逻辑）
// 安全约定：调用 git 一律用 Command::new + 参数数组，禁止拼接 shell 字符串。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager};

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

fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("无法执行 git：{e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { "git 命令失败".to_string() } else { err })
    }
}

/// 带超时的 git 调用（用于批量写操作）：stdin 置空避免交互式认证挂起，超时返回明确错误
fn run_git_timeout(dir: &Path, args: &[&str], secs: u64) -> Result<String, String> {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(Ok(out)) => {
            if out.status.success() {
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                Err(if err.is_empty() { "git 命令失败".to_string() } else { err })
            }
        }
        Ok(Err(e)) => Err(format!("无法执行 git：{e}")),
        Err(_) => Err("git 命令超时（可能需要认证，请先在终端手动操作一次）".to_string()),
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

fn scan_dir(dir: &Path, out: &mut Vec<RepoEntry>, depth: usize) {
    if depth > 8 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // 跳过常见的无关/重型目录，加快扫描
        if matches!(name.as_str(), "node_modules" | "target" | "dist" | "build" | ".gradle" | ".idea" | ".vscode" | ".git")
        {
            continue;
        }
        if is_git_repo(&path) {
            out.push(RepoEntry {
                path: path.to_string_lossy().to_string(),
                name,
            });
        } else {
            scan_dir(&path, out, depth + 1);
        }
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
        });
    } else {
        scan_dir(&root_path, &mut out, 0);
    }
    out
}

fn parse_count(s: &str, key: &str) -> i32 {
    // 从 "[ahead 1, behind 2]" 之类的括号中取数字
    if let Some(idx) = s.find(key) {
        let rest = &s[idx + key.len()..];
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
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
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        handles.push(tauri::async_runtime::spawn_blocking(move || get_one_status(&p)));
    }
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(h.await.unwrap_or_default());
    }
    out
}

#[tauri::command]
async fn pull_repos(app: tauri::AppHandle, paths: Vec<String>) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        handles.push(tauri::async_runtime::spawn_blocking(move || {
            let dir = Path::new(&p);
            let r = match run_git_timeout(dir, &["pull"], 60) {
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
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        handles.push(tauri::async_runtime::spawn_blocking(move || {
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
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let label = label.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        handles.push(tauri::async_runtime::spawn_blocking(move || {
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
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        handles.push(tauri::async_runtime::spawn_blocking(move || {
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

#[tauri::command]
async fn run_command(app: tauri::AppHandle, paths: Vec<String>, command: String) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let cmd = command.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        handles.push(tauri::async_runtime::spawn_blocking(move || {
            // 在仓库目录执行用户自定义命令（sh -c；命令由用户本人输入，等同在终端手动执行）
            // 用线程 + 超时避免交互式命令挂起，stdin 置空
            let (tx, rx) = std::sync::mpsc::channel();
            let p_c = p.clone();
            let cmd_c = cmd.clone();
            std::thread::spawn(move || {
                let out = Command::new("sh")
                    .arg("-c")
                    .arg(&cmd_c)
                    .current_dir(&p_c)
                    .stdin(std::process::Stdio::null())
                    .output();
                let _ = tx.send(out);
            });
            let out = rx.recv_timeout(std::time::Duration::from_secs(120));
            let r = match out {
                Ok(Ok(o)) => {
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
                Ok(Err(e)) => OpResult {
                    path: p,
                    ok: false,
                    message: format!("无法执行命令：{e}"),
                },
                Err(_) => OpResult {
                    path: p,
                    ok: false,
                    message: "命令执行超时（可能等待输入），已中止".to_string(),
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
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let old = old.clone();
        let new = new.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        handles.push(tauri::async_runtime::spawn_blocking(move || {
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
    let mut add_args = vec!["add"];
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

/// 批量切换分支：对每个仓库执行 git switch {branch}
#[tauri::command]
async fn switch_branches(app: tauri::AppHandle, paths: Vec<String>, branch: String) -> Vec<OpResult> {
    let total = paths.len() as i32;
    let done = Arc::new(AtomicI32::new(0));
    let okc = Arc::new(AtomicI32::new(0));
    let mut handles = Vec::new();
    for p in &paths {
        let p = p.clone();
        let branch = branch.clone();
        let app = app.clone();
        let done = Arc::clone(&done);
        let okc = Arc::clone(&okc);
        handles.push(tauri::async_runtime::spawn_blocking(move || {
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

/// 在指定目录打开 macOS 终端
#[tauri::command]
fn open_terminal(path: String) -> Result<(), String> {
    let out = Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(&path)
        .output()
        .map_err(|e| format!("无法打开终端：{e}"))?;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
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
            switch_branches,
            open_terminal,
            list_changes,
            commit_files,
            clone_repo,
            stash_repos,
            stash_pop_repos,
            get_log
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
}
