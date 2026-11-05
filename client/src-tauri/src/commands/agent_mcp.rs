//! One-click MCP setup for the coding agents on this computer (the MCP & API screen).
//!
//! Each agent's own CLI writes its own config, so comments and other servers in that config are
//! the CLI's to keep. The renderer only names an agent from a fixed list: the programs, the
//! arguments and the address are all decided here. A desktop app starts with a short PATH, so the
//! search also takes the login shell's PATH and the places these CLIs usually install to.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::engine;

const SERVER_NAME: &str = "ps5upload";
const READ_DEADLINE: Duration = Duration::from_secs(25);
const CHANGE_DEADLINE: Duration = Duration::from_secs(60);
#[cfg(unix)]
const SHELL_PATH_DEADLINE: Duration = Duration::from_secs(4);
const SHELL_PATH_MARK: &str = "__ps5upload_path__";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Agent {
    ClaudeCode,
    Codex,
    OpenCode,
}

const AGENTS: [Agent; 3] = [Agent::ClaudeCode, Agent::Codex, Agent::OpenCode];

impl Agent {
    fn from_id(id: &str) -> Option<Self> {
        AGENTS.into_iter().find(|a| a.id() == id)
    }

    fn id(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
        }
    }

    fn read_args(self) -> Vec<String> {
        strings(match self {
            Agent::ClaudeCode => &["mcp", "get", SERVER_NAME],
            Agent::Codex => &["mcp", "get", SERVER_NAME, "--json"],
            Agent::OpenCode => &["debug", "config"],
        })
    }

    fn add_args(self, url: &str) -> Vec<String> {
        strings(&match self {
            Agent::ClaudeCode => vec![
                "mcp",
                "add",
                "--scope",
                "user",
                "--transport",
                "http",
                SERVER_NAME,
                url,
            ],
            Agent::Codex => vec!["mcp", "add", SERVER_NAME, "--url", url],
            Agent::OpenCode => vec!["mcp", "add", SERVER_NAME, "--url", url],
        })
    }

    /// OpenCode has no remove command, and its config is the user's to edit.
    fn remove_args(self) -> Option<Vec<String>> {
        match self {
            Agent::ClaudeCode => Some(strings(&["mcp", "remove", "--scope", "user", SERVER_NAME])),
            Agent::Codex => Some(strings(&["mcp", "remove", SERVER_NAME])),
            Agent::OpenCode => None,
        }
    }

    /// Claude Code refuses to add a name it already has. The others replace the entry.
    fn add_refuses_existing(self) -> bool {
        self == Agent::ClaudeCode
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// What an agent has saved for this server. `url` is `None` for an entry that runs a program,
/// such as the stdio bridge.
#[derive(Debug, PartialEq, Eq)]
struct Saved {
    url: Option<String>,
}

/// Reads an agent's answer. Only the address leaves here: OpenCode's answer is its whole config,
/// which can hold other servers' secrets.
fn parse_saved(agent: Agent, succeeded: bool, stdout: &str) -> Result<Option<Saved>, String> {
    match agent {
        // A missing server exits non-zero, in both of these.
        Agent::ClaudeCode | Agent::Codex if !succeeded => Ok(None),
        Agent::ClaudeCode => {
            let url = stdout
                .lines()
                .find_map(|line| line.trim().strip_prefix("URL:"))
                .map(|u| u.trim().to_string());
            Ok(Some(Saved { url }))
        }
        Agent::Codex => {
            let doc: serde_json::Value =
                serde_json::from_str(stdout).map_err(|_| "unreadable_answer".to_string())?;
            let url = doc["transport"]["url"].as_str().map(str::to_string);
            Ok(Some(Saved { url }))
        }
        Agent::OpenCode => {
            if !succeeded {
                return Err("unreadable_answer".to_string());
            }
            let doc: serde_json::Value =
                serde_json::from_str(stdout).map_err(|_| "unreadable_answer".to_string())?;
            let entry = &doc["mcp"][SERVER_NAME];
            if entry.is_null() {
                return Ok(None);
            }
            Ok(Some(Saved {
                url: entry["url"].as_str().map(str::to_string),
            }))
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AgentMcpStatus {
    agent: &'static str,
    /// The agent's CLI was found on this computer.
    found: bool,
    /// The agent has an entry named `ps5upload`.
    configured: bool,
    saved_url: Option<String>,
    /// Where this app's engine answers MCP now.
    expected_url: String,
    can_remove: bool,
    /// The CLI is there, but its answer couldn't be read.
    read_failed: bool,
}

fn expected_url() -> String {
    format!("{}/mcp", engine::url().trim_end_matches('/'))
}

/// `PATH` for finding and running these CLIs: an npm-installed one starts `node` by name, so what
/// it runs under needs the same search.
fn search_path() -> OsString {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(login_shell_path());
    dirs.extend(usual_dirs());
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
    std::env::join_paths(dirs).unwrap_or_default()
}

/// The directories in the PATH a terminal would have. A shell that takes longer than the deadline
/// (or prints no marker) just adds nothing.
#[cfg(unix)]
fn login_shell_path() -> Vec<PathBuf> {
    let Some(shell) = std::env::var_os("SHELL") else {
        return Vec::new();
    };
    let script = format!("echo {SHELL_PATH_MARK}; printenv PATH");
    let args = vec!["-ilc".to_string(), script];
    let Ok(out) = run(
        Path::new(&shell),
        &args,
        None,
        Path::new("/"),
        SHELL_PATH_DEADLINE,
    ) else {
        return Vec::new();
    };
    parse_shell_path(&out.stdout)
}

#[cfg(not(unix))]
fn login_shell_path() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg_attr(not(unix), allow(dead_code))]
fn parse_shell_path(stdout: &str) -> Vec<PathBuf> {
    let mut lines = stdout.lines().skip_while(|l| l.trim() != SHELL_PATH_MARK);
    lines.next();
    lines
        .next()
        .map(|p| std::env::split_paths(p.trim()).collect())
        .unwrap_or_default()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn usual_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home_dir() {
        for sub in [
            ".local/bin",
            ".claude/local",
            ".npm-global/bin",
            ".bun/bin",
            ".opencode/bin",
            ".volta/bin",
            ".cargo/bin",
        ] {
            dirs.push(home.join(sub));
        }
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
    } else {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/usr/bin"));
    }
    dirs
}

fn find_program(name: &str, path: &OsString) -> Option<PathBuf> {
    let suffixes: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(path).find_map(|dir| {
        suffixes
            .iter()
            .map(|suffix| dir.join(format!("{name}{suffix}")))
            .find(|candidate| is_program(candidate))
    })
}

#[cfg(unix)]
fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_program(path: &Path) -> bool {
    path.is_file()
}

#[derive(Debug)]
struct RunOutput {
    succeeded: bool,
    stdout: String,
    stderr: String,
}

/// Runs a program with nothing on stdin and a time limit. The pipes are read on their own threads,
/// and a grandchild that keeps one open can't hold this up past the limit.
fn run(
    program: &Path,
    args: &[String],
    path: Option<&OsString>,
    cwd: &Path,
    deadline: Duration,
) -> Result<RunOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    // An AppImage's libraries are for this app, not for what it starts.
    #[cfg(target_os = "linux")]
    command.env_remove("LD_LIBRARY_PATH");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let stdout = read_in_background(child.stdout.take());
    let stderr = read_in_background(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed_out".to_string());
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let collect =
        |rx: mpsc::Receiver<String>| rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    Ok(RunOutput {
        succeeded: status.success(),
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

fn read_in_background<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    rx
}

/// A failed change's last words, without a CLI's terminal colours.
fn failure_detail(out: &RunOutput) -> String {
    let text = if out.stderr.trim().is_empty() {
        &out.stdout
    } else {
        &out.stderr
    };
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            clean.push(c);
        }
    }
    let lines: Vec<&str> = clean
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines[lines.len().saturating_sub(3)..].join("\n")
}

fn working_dir() -> PathBuf {
    home_dir().unwrap_or_else(std::env::temp_dir)
}

fn status_of(agent: Agent, path: &OsString) -> AgentMcpStatus {
    let mut status = AgentMcpStatus {
        agent: agent.id(),
        found: false,
        configured: false,
        saved_url: None,
        expected_url: expected_url(),
        can_remove: agent.remove_args().is_some(),
        read_failed: false,
    };
    let Some(program) = find_program(agent.program(), path) else {
        return status;
    };
    status.found = true;
    let answer = run(
        &program,
        &agent.read_args(),
        Some(path),
        &working_dir(),
        READ_DEADLINE,
    )
    .and_then(|out| parse_saved(agent, out.succeeded, &out.stdout));
    match answer {
        Ok(Some(saved)) => {
            status.configured = true;
            status.saved_url = saved.url;
        }
        Ok(None) => {}
        Err(_) => status.read_failed = true,
    }
    status
}

fn change(agent: Agent, args: &[String], path: &OsString) -> Result<(), String> {
    let program = find_program(agent.program(), path).ok_or("cli_not_found")?;
    let out = run(&program, args, Some(path), &working_dir(), CHANGE_DEADLINE)?;
    if out.succeeded {
        Ok(())
    } else {
        Err(failure_detail(&out))
    }
}

fn agent_from(id: &str) -> Result<Agent, String> {
    Agent::from_id(id).ok_or_else(|| "unknown_agent".to_string())
}

fn not_on_phones() -> Result<(), String> {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return Err("desktop_only".to_string());
    }
    Ok(())
}

/// Where each agent stands. They are asked side by side, as one can take seconds to answer.
#[tauri::command]
pub async fn agent_mcp_status() -> Result<Vec<AgentMcpStatus>, String> {
    not_on_phones()?;
    tauri::async_runtime::spawn_blocking(|| {
        let path = search_path();
        std::thread::scope(|scope| {
            let asks: Vec<_> = AGENTS
                .into_iter()
                .map(|agent| {
                    let path = &path;
                    scope.spawn(move || status_of(agent, path))
                })
                .collect();
            asks.into_iter().filter_map(|ask| ask.join().ok()).collect()
        })
    })
    .await
    .map_err(|e| e.to_string())
}

/// Adds this engine to an agent, or moves its entry here. Answers with where it stands after.
#[tauri::command]
pub async fn agent_mcp_add(agent: String) -> Result<AgentMcpStatus, String> {
    not_on_phones()?;
    let agent = agent_from(&agent)?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = search_path();
        let before = status_of(agent, &path);
        if !before.found {
            return Err("cli_not_found".to_string());
        }
        if agent.add_refuses_existing() && before.configured {
            if let Some(args) = agent.remove_args() {
                change(agent, &args, &path)?;
            }
        }
        change(agent, &agent.add_args(&expected_url()), &path)?;
        Ok(status_of(agent, &path))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Takes this engine out of an agent. Answers with where it stands after.
#[tauri::command]
pub async fn agent_mcp_remove(agent: String) -> Result<AgentMcpStatus, String> {
    not_on_phones()?;
    let agent = agent_from(&agent)?;
    let args = agent.remove_args().ok_or("cannot_remove")?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = search_path();
        change(agent, &args, &path)?;
        Ok(status_of(agent, &path))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The engine program, for clients that start the stdio bridge themselves (Claude Desktop).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub async fn agent_mcp_bridge_path(app: tauri::AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        engine::find_engine_binary(&app)
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A phone's engine lives inside the app: there is no program to start.
#[cfg(any(target_os = "android", target_os = "ios"))]
#[tauri::command]
pub async fn agent_mcp_bridge_path() -> Result<String, String> {
    Err("desktop_only".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://127.0.0.1:19113/mcp";

    #[test]
    fn only_the_listed_agents_are_accepted() {
        assert_eq!(Agent::from_id("codex"), Some(Agent::Codex));
        assert_eq!(Agent::from_id("claude-code"), Some(Agent::ClaudeCode));
        assert_eq!(Agent::from_id("sh"), None);
        assert_eq!(Agent::from_id(""), None);
    }

    #[test]
    fn each_agent_gets_the_arguments_its_cli_takes() {
        assert_eq!(
            Agent::ClaudeCode.add_args(URL).join(" "),
            format!("mcp add --scope user --transport http ps5upload {URL}")
        );
        assert_eq!(
            Agent::Codex.add_args(URL).join(" "),
            format!("mcp add ps5upload --url {URL}")
        );
        assert_eq!(
            Agent::OpenCode.add_args(URL).join(" "),
            format!("mcp add ps5upload --url {URL}")
        );
        assert!(Agent::OpenCode.remove_args().is_none());
    }

    #[test]
    fn claude_codes_answer_gives_the_address() {
        let out = "ps5upload:\n  Scope: User config\n  Status: ✔ Connected\n  Type: http\n  URL: http://127.0.0.1:19113/mcp\n";
        assert_eq!(
            parse_saved(Agent::ClaudeCode, true, out).unwrap(),
            Some(Saved {
                url: Some(URL.to_string())
            })
        );
        assert_eq!(parse_saved(Agent::ClaudeCode, false, "").unwrap(), None);
    }

    #[test]
    fn claude_codes_stdio_entry_counts_as_saved_with_no_address() {
        let out = "ps5upload:\n  Type: stdio\n  Command: /x/ps5upload-engine\n";
        assert_eq!(
            parse_saved(Agent::ClaudeCode, true, out).unwrap(),
            Some(Saved { url: None })
        );
    }

    #[test]
    fn codexs_answer_gives_the_address() {
        let out = r#"{"name":"ps5upload","transport":{"type":"streamable_http","url":"http://127.0.0.1:19113/mcp"}}"#;
        assert_eq!(
            parse_saved(Agent::Codex, true, out).unwrap(),
            Some(Saved {
                url: Some(URL.to_string())
            })
        );
        assert_eq!(parse_saved(Agent::Codex, false, "Error").unwrap(), None);
        assert!(parse_saved(Agent::Codex, true, "not json").is_err());
    }

    #[test]
    fn opencodes_config_gives_only_our_entry() {
        let out = r#"{"mcp":{"other":{"type":"remote","url":"https://x","headers":{"Authorization":"secret"}},"ps5upload":{"type":"remote","url":"http://127.0.0.1:19113/mcp"}}}"#;
        assert_eq!(
            parse_saved(Agent::OpenCode, true, out).unwrap(),
            Some(Saved {
                url: Some(URL.to_string())
            })
        );
        assert_eq!(
            parse_saved(Agent::OpenCode, true, r#"{"mcp":{"other":{}}}"#).unwrap(),
            None
        );
        assert_eq!(parse_saved(Agent::OpenCode, true, "{}").unwrap(), None);
        assert!(parse_saved(Agent::OpenCode, false, "").is_err());
    }

    #[test]
    fn the_shell_path_comes_after_the_marker() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let out = format!("welcome text\n{SHELL_PATH_MARK}\n/a/bin{sep}/b/bin\n");
        assert_eq!(
            parse_shell_path(&out),
            vec![PathBuf::from("/a/bin"), PathBuf::from("/b/bin")]
        );
        assert!(parse_shell_path("no marker\n/a/bin\n").is_empty());
    }

    #[test]
    fn a_failure_keeps_its_last_lines_without_colours() {
        let out = RunOutput {
            succeeded: false,
            stdout: String::new(),
            stderr: "one\ntwo\n\u{1b}[31mthree\u{1b}[0m\nfour\n\n".to_string(),
        };
        assert_eq!(failure_detail(&out), "two\nthree\nfour");
    }

    #[cfg(unix)]
    mod with_programs {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn scratch(name: &str) -> PathBuf {
            let dir = std::env::temp_dir()
                .join(format!("ps5upload-agent-mcp-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        #[test]
        fn a_program_is_found_only_if_it_can_run() {
            let dir = scratch("find");
            script(&dir, "codex", "exit 0");
            std::fs::write(dir.join("claude"), "not runnable").unwrap();
            let path = std::env::join_paths([&dir]).unwrap();
            assert_eq!(find_program("codex", &path), Some(dir.join("codex")));
            assert_eq!(find_program("claude", &path), None);
            assert_eq!(find_program("opencode", &path), None);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_program_that_hangs_is_stopped_at_the_limit() {
            let dir = scratch("hang");
            let program = script(&dir, "slow", "sleep 30");
            let started = Instant::now();
            let answer = run(&program, &[], None, &dir, Duration::from_millis(200));
            assert_eq!(answer.unwrap_err(), "timed_out");
            assert!(started.elapsed() < Duration::from_secs(10));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn output_and_exit_status_come_back() {
            let dir = scratch("out");
            let program = script(&dir, "says", "echo out; echo err >&2; exit 3");
            let answer = run(&program, &[], None, &dir, Duration::from_secs(10)).unwrap();
            assert!(!answer.succeeded);
            assert_eq!(answer.stdout.trim(), "out");
            assert_eq!(answer.stderr.trim(), "err");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
