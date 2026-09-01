//! Omarchy/OS actions behind the local "Omarchy" virtual chat.
//! Orchestrator-owned: this module executes processes.
//!
//! Security model: input only ever comes from the user typing in THEIR OWN
//! client (the virtual chat is local, never a Telegram message), so there is
//! no remote surface. Still: everything is off until `os.enabled`, arbitrary
//! shell is a separate opt-in (`os.shell`) and confirm-gated by the UI, every
//! execution is audit-logged, and processes get a hard timeout.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

const OMARCHY_BIN: &str = "/usr/share/omarchy/bin";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Curated by Omarchygram (always listed first).
    Builtin,
    /// Discovered from the Omarchy tool metadata on this machine.
    Omarchy,
    /// From the user's settings (`[os.actions]`).
    User,
}

#[derive(Debug, Clone)]
pub struct Action {
    /// What the user types: `screenshot`, `theme`, `omarchy-menu`, …
    pub name: String,
    pub summary: String,
    /// Argument hint shown in help, e.g. `<theme-name>`.
    pub args: String,
    /// Program + fixed args; user args are appended.
    argv: Vec<String>,
    /// For User actions the command line runs through `sh -c` with args as $@.
    shell_line: Option<String>,
    pub source: Source,
}

/// Curated builtins: (name, summary, args hint, argv).
const BUILTINS: &[(&str, &str, &str, &[&str])] = &[
    ("screenshot", "Take a screenshot (saved to the screenshots folder)", "[smart|region|windows|fullscreen] [copy|save]", &["omarchy-capture-screenshot"]),
    ("lock", "Lock the computer and turn off the display", "", &["omarchy-system-lock"]),
    ("notify", "Show a desktop notification", "<text>", &["omarchy-notification-send", "--app-name", "Omarchygram"]),
    ("volume", "Adjust output volume", "<raise|lower|mute-toggle|+N|-N>", &["omarchy-audio-output-volume"]),
    ("theme", "Apply an Omarchy theme", "<theme-name>", &["omarchy-theme-set"]),
    ("themes", "List available themes", "", &["omarchy-theme-list"]),
    ("theme-current", "Show the active theme", "", &["omarchy-theme-current"]),
    ("terminal", "Open a terminal (optionally running a command)", "[command...]", &["omarchy-launch-terminal"]),
    ("transparency", "Toggle transparency of the focused window", "", &["omarchy-hyprland-window-transparency-toggle"]),
    ("status", "Uptime, memory and disk at a glance", "", &["sh", "-c", "uptime; echo; free -h; echo; df -h ~ | tail -1"]),
];

/// Everything the user can run right now: builtins, user actions, then the
/// Omarchy tools discovered on this machine (skipping hidden/dev ones).
pub fn catalog(user_actions: &BTreeMap<String, String>) -> Vec<Action> {
    let mut out: Vec<Action> = BUILTINS
        .iter()
        .map(|(name, summary, args, argv)| Action {
            name: name.to_string(),
            summary: summary.to_string(),
            args: args.to_string(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            shell_line: None,
            source: Source::Builtin,
        })
        .collect();
    for (name, line) in user_actions {
        if name.trim().is_empty() || line.trim().is_empty() {
            continue;
        }
        out.push(Action {
            name: name.clone(),
            summary: format!("your action: {line}"),
            args: "[args...]".into(),
            argv: vec![],
            shell_line: Some(line.clone()),
            source: Source::User,
        });
    }
    let taken: Vec<String> = out.iter().map(|a| a.name.clone()).collect();
    for a in discover_omarchy() {
        if !taken.contains(&a.name) {
            out.push(a);
        }
    }
    out
}

/// Parse `# omarchy:summary=` / `# omarchy:args=` / `# omarchy:hidden=true`
/// headers from every `omarchy-*` tool. Names keep their full `omarchy-…`
/// form so they never collide with builtins.
fn discover_omarchy() -> Vec<Action> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(OMARCHY_BIN) else { return out };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.starts_with("omarchy-") || name.contains("-dev-") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let head: Vec<&str> = text.lines().take(12).collect();
        if head.iter().any(|l| l.contains("omarchy:hidden=true")) {
            continue;
        }
        let Some(summary) = head
            .iter()
            .find_map(|l| l.strip_prefix("# omarchy:summary="))
        else {
            continue;
        };
        let args = head
            .iter()
            .find_map(|l| l.strip_prefix("# omarchy:args="))
            .unwrap_or("")
            .to_string();
        out.push(Action {
            name: name.to_string(),
            summary: summary.trim().to_string(),
            args,
            argv: vec![name.to_string()],
            shell_line: None,
            source: Source::Omarchy,
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Help,
    /// List actions; `filter` narrows by substring.
    List { filter: String },
    Run { name: String, args: Vec<String> },
    /// `run <command line>` — only with os.shell, and only after the UI's
    /// confirmation dialog.
    Shell(String),
    Empty,
}

/// Turn a typed line into an intent. Quotes are honored for args.
pub fn parse(line: &str) -> Parsed {
    let line = line.trim();
    if line.is_empty() {
        return Parsed::Empty;
    }
    let mut words = shell_words(line).into_iter();
    let Some(head) = words.next() else { return Parsed::Empty };
    match head.as_str() {
        "help" | "?" => Parsed::Help,
        "list" | "actions" | "ls" => Parsed::List { filter: words.collect::<Vec<_>>().join(" ") },
        "run" | "sh" | "$" => Parsed::Shell(line[head.len()..].trim().to_string()),
        _ => Parsed::Run { name: head, args: words.collect() },
    }
}

/// Minimal POSIX-ish word splitting with single/double quotes and backslash.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    let mut had = false;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), ch) if ch == q => quote = None,
            (Some(_), ch) => cur.push(ch),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                had = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (None, ch) if ch.is_whitespace() => {
                if !cur.is_empty() || had {
                    out.push(std::mem::take(&mut cur));
                    had = false;
                }
            }
            (None, ch) => cur.push(ch),
        }
    }
    if !cur.is_empty() || had {
        out.push(cur);
    }
    out
}

pub fn help_text(actions: &[Action]) -> String {
    let mut s = String::from(
        "Omarchy control — type an action name, optionally with arguments.\n\
         `list [filter]` shows everything available; `help` shows this.\n\n",
    );
    for a in actions.iter().filter(|a| a.source != Source::Omarchy) {
        s.push_str(&format!("  {:<14} {}", a.name, a.summary));
        if !a.args.is_empty() {
            s.push_str(&format!("  {}", a.args));
        }
        s.push('\n');
    }
    let n = actions.iter().filter(|a| a.source == Source::Omarchy).count();
    s.push_str(&format!(
        "\n  … plus {n} Omarchy tools discovered on this machine (`list omarchy-`).\n\
         `run <command>` runs a shell command — only if enabled in Settings, and each one is confirmed first."
    ));
    s
}

pub fn list_text(actions: &[Action], filter: &str) -> String {
    let f = filter.to_lowercase();
    let mut s = String::new();
    for a in actions.iter().filter(|a| f.is_empty() || a.name.to_lowercase().contains(&f) || a.summary.to_lowercase().contains(&f)) {
        s.push_str(&format!("{:<40} {}", a.name, a.summary));
        if !a.args.is_empty() {
            s.push_str(&format!("  {}", a.args));
        }
        s.push('\n');
    }
    if s.is_empty() {
        s = "nothing matches".into();
    }
    s
}

/// Execute a catalog action with user args. Output is stdout+stderr,
/// truncated; a non-zero exit is reported in the text, not as Err.
pub async fn run_action(action: &Action, args: &[String]) -> Result<String, String> {
    let (program, argv): (String, Vec<String>) = match &action.shell_line {
        Some(line) => {
            let mut v = vec!["-c".to_string(), format!("{line} \"$@\""), "omarchygram".to_string()];
            v.extend(args.iter().cloned());
            ("sh".to_string(), v)
        }
        None => {
            let mut v = action.argv.clone();
            let program = v.remove(0);
            v.extend(args.iter().cloned());
            (program, v)
        }
    };
    if action.name == "notify" && args.is_empty() {
        return Err("notify needs some text".into());
    }
    let result = exec(&program, &argv).await;
    audit("action", &action.name, args, &result);
    result
}

/// Arbitrary shell. The UI MUST only call this when `os.shell` is on AND the
/// user confirmed this exact command line in a dialog.
pub async fn run_shell(cmdline: &str) -> Result<String, String> {
    let result = exec("sh", &["-c".to_string(), cmdline.to_string()]).await;
    audit("shell", cmdline, &[], &result);
    result
}

async fn exec(program: &str, argv: &[String]) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(argv).kill_on_drop(true).stdin(std::process::Stdio::null());
    let child = cmd.output();
    let out = match tokio::time::timeout(TIMEOUT, child).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("could not start `{program}`: {e}")),
        Err(_) => return Err(format!("`{program}` timed out after {}s", TIMEOUT.as_secs())),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(err.trim());
    }
    let mut text = text.trim().to_string();
    if text.chars().count() > MAX_OUTPUT {
        text = text.chars().take(MAX_OUTPUT).collect::<String>() + "\n…(truncated)";
    }
    if !out.status.success() {
        let code = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
        if text.is_empty() {
            text = format!("exited with status {code}");
        } else {
            text = format!("{text}\n(exit {code})");
        }
    } else if text.is_empty() {
        text = "done".into();
    }
    Ok(text)
}

fn audit_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
        .join("omarchygram/os-audit.log")
}

fn audit(kind: &str, what: &str, args: &[String], result: &Result<String, String>) {
    use std::io::Write;
    let p = audit_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let outcome = match result {
        Ok(t) => format!("ok: {}", t.lines().next().unwrap_or("").chars().take(200).collect::<String>()),
        Err(e) => format!("err: {e}"),
    };
    let line = format!(
        "{} | {kind} | {what} | {} | {outcome}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        args.join(" ")
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = f.write_all(line.as_bytes());
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
}

#[allow(dead_code)]
pub fn omarchy_available() -> bool {
    Path::new(OMARCHY_BIN).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quotes_and_intents() {
        assert_eq!(parse("help"), Parsed::Help);
        assert_eq!(parse("  "), Parsed::Empty);
        assert_eq!(
            parse("notify \"build done\" now"),
            Parsed::Run { name: "notify".into(), args: vec!["build done".into(), "now".into()] }
        );
        assert_eq!(parse("run ls -la /tmp"), Parsed::Shell("ls -la /tmp".into()));
        assert_eq!(parse("list omarchy-"), Parsed::List { filter: "omarchy-".into() });
    }

    #[test]
    fn catalog_has_builtins_first_and_user_actions() {
        let mut user = BTreeMap::new();
        user.insert("hello".to_string(), "echo hi".to_string());
        let c = catalog(&user);
        assert_eq!(c[0].name, "screenshot");
        assert!(c.iter().any(|a| a.name == "hello" && a.source == Source::User));
    }
}
