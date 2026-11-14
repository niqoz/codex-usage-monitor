//! Interactive sign-in for the CLI-backed services.
//!
//! Each service is re-authenticated with its own official CLI command, run in
//! a visible console so the user can follow the browser or device-code flow.
//! The widget never sees the tokens: once the console closes it only asks the
//! poller to read the refreshed credentials again.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::diagnose;
use crate::localization::LanguageId;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    ClaudeCode,
    Codex,
    KimiCode,
}

impl Service {
    pub const ALL: [Service; 3] = [Service::ClaudeCode, Service::Codex, Service::KimiCode];

    /// Index used by the poll monitor (0 Claude, 1 Codex, 2 Antigravity, 3 Kimi).
    pub fn provider_id(self) -> usize {
        match self {
            Service::ClaudeCode => 0,
            Service::Codex => 1,
            Service::KimiCode => 3,
        }
    }

    pub fn from_provider_id(id: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.provider_id() == id)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Service::ClaudeCode => "Claude Code",
            Service::Codex => "Codex",
            Service::KimiCode => "Kimi Code",
        }
    }

    fn executable(self) -> &'static str {
        match self {
            Service::ClaudeCode => "claude",
            Service::Codex => "codex",
            Service::KimiCode => "kimi",
        }
    }

    fn login_args(self) -> &'static [&'static str] {
        match self {
            Service::ClaudeCode => &["auth", "login"],
            Service::Codex => &["login"],
            Service::KimiCode => &["login"],
        }
    }

    /// Install locations used by the official installers, checked when the
    /// CLI is not on the PATH inherited by the widget.
    fn known_locations(self) -> Vec<PathBuf> {
        let home = dirs::home_dir();
        let local = dirs::data_local_dir();
        let roaming = dirs::data_dir();
        let mut paths = Vec::new();
        match self {
            Service::ClaudeCode => {
                if let Some(home) = &home {
                    paths.push(home.join(".local").join("bin").join("claude.exe"));
                }
                if let Some(roaming) = &roaming {
                    paths.push(roaming.join("npm").join("claude.cmd"));
                }
            }
            Service::Codex => {
                if let Some(local) = &local {
                    paths.push(
                        local
                            .join("Programs")
                            .join("OpenAI")
                            .join("Codex")
                            .join("bin")
                            .join("codex.exe"),
                    );
                }
                if let Some(roaming) = &roaming {
                    paths.push(roaming.join("npm").join("codex.cmd"));
                }
            }
            Service::KimiCode => {
                if let Some(home) = &home {
                    paths.push(home.join(".local").join("bin").join("kimi.exe"));
                }
            }
        }
        paths
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LaunchError {
    CliNotFound,
    SpawnFailed,
}

/// Opens a console running the service's login command. `on_exit` runs on a
/// background thread once the console has closed.
pub fn launch_login<F>(service: Service, on_exit: F) -> Result<(), LaunchError>
where
    F: FnOnce() + Send + 'static,
{
    let cli = resolve_cli(service).ok_or(LaunchError::CliNotFound)?;
    let script = console_script(service, &cli);
    diagnose::log(format!(
        "launching {} sign-in via {}",
        service.display_name(),
        cli.display()
    ));

    let mut cmd = Command::new("cmd.exe");
    cmd.raw_arg(format!("/d /c \"{script}\""))
        // A sign-in started from inside a Claude Code session must not be
        // mistaken for a nested session.
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .creation_flags(CREATE_NEW_CONSOLE);

    let mut child = cmd.spawn().map_err(|error| {
        diagnose::log_error("unable to start sign-in console", error);
        LaunchError::SpawnFailed
    })?;

    let spawned = std::thread::Builder::new()
        .name("reauth-wait".into())
        .spawn(move || {
            let status = child.wait();
            diagnose::log(format!(
                "{} sign-in console closed status={status:?}",
                service.display_name()
            ));
            on_exit();
        });
    if let Err(error) = spawned {
        diagnose::log_error("unable to watch sign-in console", error);
    }
    Ok(())
}

/// Command line executed by `cmd.exe /c`. The console stays open after the
/// login command so the user can read its result before closing it.
fn console_script(service: Service, cli: &Path) -> String {
    let args = service.login_args().join(" ");
    let path = cli.display();
    let launch = if has_extension(cli, "ps1") {
        format!("powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{path}\" {args}")
    } else {
        format!("\"{path}\" {args}")
    };
    let title = format!("{} - sign in", service.display_name());
    format!("title {title} & {launch} & echo. & pause")
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

fn resolve_cli(service: Service) -> Option<PathBuf> {
    let from_path = Command::new("where.exe")
        .arg(service.executable())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| pick_where_result(&String::from_utf8_lossy(&output.stdout)));
    from_path.or_else(|| {
        service
            .known_locations()
            .into_iter()
            .find(|path| path.is_file())
    })
}

/// `where.exe` can list an extensionless npm shim first; only keep entries
/// Windows can actually start, preferring native executables.
fn pick_where_result(stdout: &str) -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    ["exe", "cmd", "bat", "ps1"].iter().find_map(|extension| {
        candidates
            .iter()
            .find(|path| has_extension(path, extension))
            .cloned()
    })
}

pub struct Labels {
    pub menu: &'static str,
    pub error_title: &'static str,
    pub cli_not_found: &'static str,
    pub kimi_cli_not_found: &'static str,
    pub spawn_failed: &'static str,
    pub needs_sign_in: &'static str,
}

pub fn labels(language: LanguageId) -> Labels {
    match language {
        LanguageId::French => Labels {
            menu: "Se reconnecter",
            error_title: "Reconnexion",
            cli_not_found: "Le CLI {service} (« {exe} ») est introuvable. Installez-le ou ajoutez-le au PATH, puis réessayez.",
            kimi_cli_not_found: "Le CLI Kimi Code (« kimi ») est introuvable.\n\nVoulez-vous plutôt configurer une clé API Kimi Code ?",
            spawn_failed: "Impossible d’ouvrir la console de connexion pour {service}.",
            needs_sign_in: "connexion requise",
        },
        LanguageId::SimplifiedChinese => Labels {
            menu: "重新登录",
            error_title: "重新登录",
            cli_not_found: "未找到 {service} CLI（“{exe}”）。请安装或将其加入 PATH 后重试。",
            kimi_cli_not_found: "未找到 Kimi Code CLI（“kimi”）。\n\n是否改为设置 Kimi Code API Key？",
            spawn_failed: "无法打开 {service} 的登录控制台。",
            needs_sign_in: "需要登录",
        },
        _ => Labels {
            menu: "Sign in again",
            error_title: "Sign in",
            cli_not_found: "The {service} CLI (\"{exe}\") was not found. Install it or add it to PATH, then try again.",
            kimi_cli_not_found: "The Kimi Code CLI (\"kimi\") was not found.\n\nDo you want to configure a Kimi Code API key instead?",
            spawn_failed: "Unable to open the sign-in console for {service}.",
            needs_sign_in: "sign-in required",
        },
    }
}

pub fn error_message(language: LanguageId, service: Service, error: &LaunchError) -> String {
    let text = labels(language);
    let template = match error {
        LaunchError::CliNotFound => text.cli_not_found,
        LaunchError::SpawnFailed => text.spawn_failed,
    };
    template
        .replace("{service}", service.display_name())
        .replace("{exe}", service.executable())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_round_trip() {
        for service in Service::ALL {
            assert_eq!(
                Service::from_provider_id(service.provider_id()),
                Some(service)
            );
        }
        assert_eq!(Service::from_provider_id(2), None);
    }

    #[test]
    fn where_result_prefers_runnable_entries() {
        let stdout = "C:\\npm\\codex\r\nC:\\npm\\codex.cmd\r\nC:\\bin\\codex.exe\r\n";
        assert_eq!(
            pick_where_result(stdout),
            Some(PathBuf::from("C:\\bin\\codex.exe"))
        );
        assert_eq!(
            pick_where_result("C:\\npm\\claude\r\nC:\\npm\\claude.cmd\r\n"),
            Some(PathBuf::from("C:\\npm\\claude.cmd"))
        );
        assert_eq!(pick_where_result("C:\\npm\\kimi\r\n"), None);
        assert_eq!(pick_where_result(""), None);
    }

    #[test]
    fn console_script_runs_official_login_commands() {
        let claude = console_script(
            Service::ClaudeCode,
            Path::new("C:\\Users\\A B\\.local\\bin\\claude.exe"),
        );
        assert!(claude.contains("\"C:\\Users\\A B\\.local\\bin\\claude.exe\" auth login"));
        assert!(claude.ends_with("& pause"));

        let codex = console_script(Service::Codex, Path::new("C:\\npm\\codex.ps1"));
        assert!(codex.contains("-File \"C:\\npm\\codex.ps1\" login"));

        let kimi = console_script(Service::KimiCode, Path::new("C:\\bin\\kimi.exe"));
        assert!(kimi.contains("\"C:\\bin\\kimi.exe\" login"));
    }

    #[test]
    fn error_message_names_the_service() {
        let message = error_message(
            LanguageId::French,
            Service::Codex,
            &LaunchError::CliNotFound,
        );
        assert!(message.contains("Codex") && message.contains("« codex »"));
    }
}
