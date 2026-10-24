use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::windows::process::CommandExt;

use crate::diagnose;
use crate::kimi_credentials;
use crate::localization::Strings;
use crate::models::{UsageData, UsageSection};
use crate::native_interop;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const KIMI_CODE_DEFAULT_BASE_URL: &str = "https://api.kimi.com/coding/v1";
const KIMI_CODE_DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
const KIMI_CODE_DEFAULT_CREDENTIAL: &str = "kimi-code";
const ANTIGRAVITY_CREDENTIAL_TARGET: &str = "gemini:antigravity";
const ANTIGRAVITY_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const CREATE_NO_WINDOW: u32 = 0x08000000;

const MODEL_FALLBACK_CHAIN: &[&str] = &["claude-3-haiku-20240307", "claude-haiku-4-5-20251001"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollError {
    AuthRequired,
    NoCredentials,
    TokenExpired,
    NetworkUnavailable,
    RateLimited(Option<u32>),
    ServerError,
    RequestFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageWindowKind {
    Session,
    Weekly,
}

impl PollError {
    pub fn category(self) -> &'static str {
        match self {
            Self::AuthRequired => "auth_required",
            Self::NoCredentials => "no_credentials",
            Self::TokenExpired => "token_expired",
            Self::NetworkUnavailable => "network_unavailable",
            Self::RateLimited(_) => "rate_limited",
            Self::ServerError => "server_error",
            Self::RequestFailed => "invalid_response",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialWatchMode {
    ActiveSource,
    AllSources,
    Antigravity,
    Codex,
    KimiCode,
}

pub type CredentialWatchSnapshot = Vec<String>;

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageBucket>,
    seven_day: Option<UsageBucket>,
}

#[derive(Deserialize)]
struct UsageBucket {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct CodexAuthFile {
    tokens: Option<CodexTokenData>,
}

#[derive(Clone, Deserialize)]
struct CodexTokenData {
    access_token: String,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct CodexUsageResponse {
    rate_limit: Option<Option<Box<CodexRateLimitDetails>>>,
}

#[derive(Deserialize)]
struct CodexRateLimitDetails {
    primary_window: Option<Option<Box<CodexRateLimitWindow>>>,
    secondary_window: Option<Option<Box<CodexRateLimitWindow>>>,
}

#[derive(Deserialize)]
struct CodexRateLimitWindow {
    used_percent: f64,
    reset_at: i64,
    limit_window_seconds: Option<u64>,
}

const CODEX_SESSION_WINDOW_MAX_SECONDS: u64 = 24 * 60 * 60;

#[derive(Default, Deserialize)]
struct KimiCodeConfig {
    #[serde(default)]
    providers: HashMap<String, KimiCodeProviderConfig>,
}

#[derive(Default, Deserialize)]
struct KimiCodeProviderConfig {
    base_url: Option<String>,
    api_key: Option<String>,
    oauth: Option<KimiCodeOAuthConfig>,
}

#[derive(Default, Deserialize)]
struct KimiCodeOAuthConfig {
    storage: Option<String>,
    key: Option<String>,
    #[serde(rename = "oauthHost", alias = "oauth_host")]
    oauth_host: Option<String>,
}

#[derive(Clone, Default)]
struct KimiCodeEnvironment {
    api_key: Option<String>,
    base_url: Option<String>,
    oauth_host: Option<String>,
}

enum KimiCodeTokenSource {
    ApiKey(String),
    OAuth(PathBuf),
}

struct KimiCodeAuthPlan {
    base_url: String,
    source: KimiCodeTokenSource,
    config_path: PathBuf,
}

struct KimiCodeCredentials {
    access_token: String,
    base_url: String,
}

#[derive(Deserialize)]
struct KimiCodeTokenFile {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct KimiCodeUsageResponse {
    usage: Option<KimiCodeUsageDetail>,
    usages: Option<KimiCodeUsagePools>,
    limits: Option<Vec<KimiCodeRateLimit>>,
}

#[derive(Deserialize)]
struct KimiCodeUsagePools {
    limit_5h: Option<KimiCodeRatioPool>,
    limit_7d: Option<KimiCodeRatioPool>,
}

#[derive(Deserialize)]
struct KimiCodeRatioPool {
    used_ratio: Option<serde_json::Value>,
    reset_time: Option<String>,
}

#[derive(Deserialize)]
struct KimiCodeUsageDetail {
    limit: Option<serde_json::Value>,
    used: Option<serde_json::Value>,
    remaining: Option<serde_json::Value>,
    #[serde(
        rename = "resetTime",
        alias = "reset_time",
        alias = "resetAt",
        alias = "reset_at"
    )]
    reset_time: Option<String>,
}

#[derive(Deserialize)]
struct KimiCodeRateLimit {
    window: KimiCodeWindow,
    detail: KimiCodeUsageDetail,
}

#[derive(Deserialize)]
struct KimiCodeWindow {
    duration: Option<u64>,
    #[serde(rename = "timeUnit")]
    time_unit: Option<String>,
}

#[derive(Deserialize)]
struct AntigravityAuthFile {
    token: AntigravityTokenData,
}

#[derive(Deserialize)]
struct AntigravityTokenData {
    access_token: String,
}

#[derive(Deserialize)]
struct AntigravityLoadResponse {
    #[serde(rename = "cloudaicompanionProject")]
    project: Option<String>,
}

#[derive(Deserialize)]
struct AntigravityModelsResponse {
    models: HashMap<String, AntigravityModelInfo>,
}

#[derive(Deserialize)]
struct AntigravityModelInfo {
    #[serde(rename = "quotaInfo")]
    quota_info: Option<AntigravityQuotaInfo>,
}

#[derive(Deserialize)]
struct AntigravityQuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[derive(Deserialize)]
struct AntigravityQuotaSummaryResponse {
    groups: Option<Vec<AntigravityQuotaSummaryGroup>>,
}

#[derive(Deserialize)]
struct AntigravityQuotaSummaryGroup {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    description: Option<String>,
    buckets: Option<Vec<AntigravityQuotaSummaryBucket>>,
}

#[derive(Clone, Deserialize)]
struct AntigravityQuotaSummaryBucket {
    #[serde(rename = "bucketId")]
    bucket_id: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    window: Option<String>,
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[repr(C)]
struct CredentialW {
    flags: u32,
    type_: u32,
    target_name: *mut u16,
    comment: *mut u16,
    last_written: u64,
    credential_blob_size: u32,
    credential_blob: *mut u8,
    persist: u32,
    attribute_count: u32,
    attributes: *mut c_void,
    target_alias: *mut u16,
    user_name: *mut u16,
}

#[link(name = "Advapi32")]
extern "system" {
    fn CredReadW(
        target_name: *const u16,
        type_: u32,
        reserved_flags: u32,
        credential: *mut *mut CredentialW,
    ) -> i32;
    fn CredFree(buffer: *mut c_void);
}

pub fn poll_provider(id: usize) -> Result<UsageData, PollError> {
    match id {
        0 => poll_claude_code(),
        1 => poll_codex(),
        2 => poll_antigravity(),
        3 => poll_kimi_code(),
        _ => Err(PollError::RequestFailed),
    }
}

/// Whether Claude Code CLI credentials are available from a supported source.
/// Claude Desktop authentication is intentionally not treated as CLI access.
pub fn claude_code_credentials_available() -> bool {
    read_first_credentials().is_some()
}

fn poll_claude_code() -> Result<UsageData, PollError> {
    let creds = match read_first_credentials() {
        Some(c) => c,
        None => {
            diagnose::log("poll failed: no Claude credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    let creds = refresh_or_fallback(creds)?;

    fetch_usage_with_fallback(&creds.access_token)
}

fn poll_codex() -> Result<UsageData, PollError> {
    let creds = match read_codex_credentials() {
        Some(creds) => creds,
        None => {
            diagnose::log("Codex usage poll failed: no Codex credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    match fetch_codex_usage(&creds.access_token, creds.account_id.as_deref()) {
        Ok(data) => Ok(data),
        Err(PollError::AuthRequired) => {
            cli_refresh_codex_token();
            let refreshed = read_codex_credentials().ok_or(PollError::TokenExpired)?;
            fetch_codex_usage(&refreshed.access_token, refreshed.account_id.as_deref())
        }
        Err(error) => Err(error),
    }
}

fn poll_antigravity() -> Result<UsageData, PollError> {
    let creds = match read_antigravity_credentials() {
        Some(creds) => creds,
        None => {
            diagnose::log("Antigravity usage poll failed: no Antigravity credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    fetch_antigravity_usage(&creds.access_token)
}

/// Whether Kimi Code API-key or OAuth credentials are available locally.
pub fn kimi_code_credentials_available() -> bool {
    match kimi_credentials::load() {
        Ok(Some(_)) => return true,
        Err(_) => return false,
        Ok(None) => {}
    }
    let Some(plan) = kimi_code_auth_plan() else {
        return false;
    };
    match plan.source {
        KimiCodeTokenSource::ApiKey(_) => true,
        KimiCodeTokenSource::OAuth(path) => read_kimi_code_token_file(&path).is_some_and(|token| {
            token
                .access_token
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || token
                    .refresh_token
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
        }),
    }
}

fn poll_kimi_code() -> Result<UsageData, PollError> {
    let creds = read_kimi_code_credentials()?;
    fetch_kimi_code_usage(&creds.access_token, &creds.base_url)
}

fn refresh_or_fallback(mut creds: Credentials) -> Result<Credentials, PollError> {
    loop {
        if !is_token_expired(creds.expires_at) {
            return Ok(creds);
        }

        let source = creds.source.clone();
        cli_refresh_token(&source);

        match read_credentials_from_source(&source) {
            Some(refreshed) if !is_token_expired(refreshed.expires_at) => return Ok(refreshed),
            Some(_) => diagnose::log(format!(
                "credentials from {source:?} still expired after refresh attempt"
            )),
            None => diagnose::log(format!(
                "credentials from {source:?} unavailable after refresh attempt"
            )),
        }

        match read_next_credentials_after(&source) {
            Some(next) => creds = next,
            None => return Err(PollError::TokenExpired),
        }
    }
}

/// Invoke the Claude CLI with a minimal prompt to force its internal
/// OAuth token refresh.
fn cli_refresh_token(source: &CredentialSource) {
    match source {
        CredentialSource::Windows(_) => cli_refresh_windows_token(),
        CredentialSource::Wsl { distro } => cli_refresh_wsl_token(distro),
    }
}

fn cli_refresh_windows_token() {
    let claude_path = resolve_windows_claude_path();
    let is_cmd = claude_path.to_lowercase().ends_with(".cmd");
    diagnose::log(format!(
        "attempting Windows Claude token refresh via {claude_path}"
    ));

    let args: &[&str] = &["-p", "."];

    let mut cmd = if is_cmd {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(&claude_path).args(args);
        c
    } else {
        let mut c = Command::new(&claude_path);
        c.args(args);
        c
    };
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn Windows Claude token refresh", error);
            return;
        }
    };

    // Wait up to 30 seconds — don't block the poll thread forever
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => break,
        }
    }
}

fn cli_refresh_wsl_token(distro: &str) {
    diagnose::log(format!(
        "attempting WSL Claude token refresh in distro {distro}"
    ));
    let mut cmd = Command::new("wsl.exe");
    cmd.arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-lic")
        .arg("if command -v claude >/dev/null 2>&1; then claude -p .; elif [ -x \"$HOME/.local/bin/claude\" ]; then \"$HOME/.local/bin/claude\" -p .; else exit 127; fi")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn WSL Claude token refresh", error);
            return;
        }
    };

    wait_for_refresh(&mut child);
}

fn cli_refresh_codex_token() {
    let codex_path = resolve_windows_codex_path();
    let is_cmd = codex_path.to_lowercase().ends_with(".cmd");
    let is_ps1 = codex_path.to_lowercase().ends_with(".ps1");
    diagnose::log(format!(
        "attempting Windows Codex token refresh via {codex_path}"
    ));

    let args: &[&str] = &["exec", "."];

    let mut cmd = if is_cmd {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(&codex_path).args(args);
        c
    } else if is_ps1 {
        let mut c = Command::new("powershell.exe");
        c.arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&codex_path)
            .args(args);
        c
    } else {
        let mut c = Command::new(&codex_path);
        c.args(args);
        c
    };
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn Windows Codex token refresh", error);
            return;
        }
    };

    wait_for_refresh(&mut child);
}

/// Spawn a command and wait up to `timeout` for it to finish.
/// Returns None if the process fails to start or exceeds the deadline.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<std::process::Output> {
    let mut child = cmd.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

fn wait_for_refresh(child: &mut std::process::Child) {
    // Wait up to 30 seconds; don't block the poll thread forever.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => break,
        }
    }
}

/// Resolve the full path to the `claude` CLI executable.
fn resolve_windows_claude_path() -> String {
    for name in &["claude.cmd", "claude"] {
        if Command::new(name)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return name.to_string();
        }
    }

    for name in &["claude.cmd", "claude"] {
        if let Ok(output) = Command::new("where.exe")
            .arg(name)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = stdout.lines().next() {
                    let path = first_line.trim().to_string();
                    if !path.is_empty() {
                        return path;
                    }
                }
            }
        }
    }

    "claude.cmd".to_string()
}

fn resolve_windows_codex_path() -> String {
    for name in &["codex.cmd", "codex.ps1", "codex.exe", "codex"] {
        if Command::new(name)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return name.to_string();
        }
    }

    for name in &["codex.cmd", "codex.ps1", "codex.exe", "codex"] {
        if let Ok(output) = Command::new("where.exe")
            .arg(name)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = stdout.lines().next() {
                    let path = first_line.trim().to_string();
                    if !path.is_empty() {
                        return path;
                    }
                }
            }
        }
    }

    "codex.cmd".to_string()
}

fn build_agent() -> Result<ureq::Agent, PollError> {
    build_agent_with_redirects(5)
}

fn build_agent_with_redirects(redirects: u32) -> Result<ureq::Agent, PollError> {
    let tls = native_tls::TlsConnector::new().map_err(|_| PollError::RequestFailed)?;
    Ok(ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .redirects(redirects)
        .tls_connector(std::sync::Arc::new(tls))
        .build())
}

fn classify_http_status(status: u16) -> PollError {
    match status {
        401 | 403 => PollError::AuthRequired,
        429 => PollError::RateLimited(None),
        500..=599 => PollError::ServerError,
        _ => PollError::RequestFailed,
    }
}

fn classify_ureq_error(error: &ureq::Error) -> PollError {
    match error {
        ureq::Error::Status(429, response) => PollError::RateLimited(
            response
                .header("Retry-After")
                .and_then(|value| crate::retry_after::delay_ms(value, SystemTime::now())),
        ),
        ureq::Error::Status(status, _) => classify_http_status(*status),
        ureq::Error::Transport(_) => PollError::NetworkUnavailable,
    }
}

pub fn credential_watch_snapshot(mode: CredentialWatchMode) -> CredentialWatchSnapshot {
    if mode == CredentialWatchMode::Codex {
        return codex_auth_path()
            .map(|p| vec![windows_credential_watch_signature(&p)])
            .unwrap_or_default();
    }
    if mode == CredentialWatchMode::Antigravity {
        return vec![antigravity_credential_watch_signature()];
    }
    if mode == CredentialWatchMode::KimiCode {
        return kimi_code_credential_watch_snapshot();
    }

    let sources = match mode {
        CredentialWatchMode::ActiveSource => read_first_credentials()
            .map(|creds| vec![creds.source])
            .unwrap_or_else(all_known_credential_sources),
        CredentialWatchMode::AllSources => all_known_credential_sources(),
        CredentialWatchMode::Antigravity
        | CredentialWatchMode::Codex
        | CredentialWatchMode::KimiCode => unreachable!(),
    };

    let mut snapshot: CredentialWatchSnapshot = sources
        .into_iter()
        .filter_map(|source| credential_watch_signature(&source))
        .collect();
    snapshot.sort();
    snapshot.dedup();
    snapshot
}

fn all_known_credential_sources() -> Vec<CredentialSource> {
    let mut sources = Vec::new();
    if let Some(source) = windows_credential_source() {
        sources.push(source);
    }
    for distro in list_wsl_distros() {
        sources.push(CredentialSource::Wsl { distro });
    }
    sources
}

fn windows_credential_source() -> Option<CredentialSource> {
    Some(CredentialSource::Windows(windows_credentials_path_from(
        std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
        dirs::home_dir(),
    )?))
}

fn windows_credentials_path_from(
    config_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let config_dir = config_dir.filter(|path| !path.as_os_str().is_empty());
    let directory = config_dir.or_else(|| home.map(|path| path.join(".claude")))?;
    Some(directory.join(".credentials.json"))
}

fn credential_watch_signature(source: &CredentialSource) -> Option<String> {
    match source {
        CredentialSource::Windows(path) => Some(windows_credential_watch_signature(path)),
        CredentialSource::Wsl { distro } => wsl_credential_watch_signature(distro),
    }
}

fn windows_credential_watch_signature(path: &PathBuf) -> String {
    let key = format!("win:{}", path.display());
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_nanos())
                .unwrap_or(0);
            format!("{key}|present|{}|{modified}", metadata.len())
        }
        Err(_) => format!("{key}|missing"),
    }
}

fn wsl_credential_watch_signature(distro: &str) -> Option<String> {
    let output = run_with_timeout(
        Command::new("wsl.exe")
            .arg("-d")
            .arg(distro)
            .arg("--")
            .arg("sh")
            .arg("-lc")
            .arg(
                "if [ -f ~/.claude/.credentials.json ]; then \
                 stat -c 'present|%s|%Y' ~/.claude/.credentials.json; \
                 else echo missing; fi",
            )
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    )?;

    let state = if output.status.success() {
        decode_wsl_text(&output.stdout).trim().to_string()
    } else {
        format!("status-{}", output.status)
    };

    Some(format!("wsl:{distro}|{state}"))
}

fn fetch_usage_with_fallback(token: &str) -> Result<UsageData, PollError> {
    // Try the dedicated usage endpoint first
    match try_usage_endpoint(token)? {
        Some(data) => {
            // If reset timers are missing, fill them in from the Messages API
            if data.session.resets_at.is_none() || data.weekly.resets_at.is_none() {
                if let Ok(fallback) = fetch_usage_via_messages(token) {
                    let mut merged = data;
                    if merged.session.resets_at.is_none() {
                        merged.session.resets_at = fallback.session.resets_at;
                    }
                    if merged.weekly.resets_at.is_none() {
                        merged.weekly.resets_at = fallback.weekly.resets_at;
                    }
                    return Ok(merged);
                }
            }
            return Ok(data);
        }
        None => {}
    }

    // Fall back to Messages API with rate limit headers
    let result = fetch_usage_via_messages(token);
    if result.is_err() {
        diagnose::log("usage endpoint and Messages API fallback both failed");
    }
    result
}

fn try_usage_endpoint(token: &str) -> Result<Option<UsageData>, PollError> {
    let agent = build_agent()?;

    let resp = match agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call()
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            diagnose::log(format!(
                "usage endpoint returned auth error status {code}; re-login required"
            ));
            return Err(PollError::AuthRequired);
        }
        Err(error @ ureq::Error::Status(429, _)) => return Err(classify_ureq_error(&error)),
        Err(_) => return Ok(None),
    };

    let response: UsageResponse = match resp.into_json() {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    let mut data = UsageData::default();

    if let Some(bucket) = &response.five_hour {
        data.session.percentage = bucket.utilization;
        data.session.resets_at = parse_iso8601(bucket.resets_at.as_deref());
    }

    if let Some(bucket) = &response.seven_day {
        data.weekly.percentage = bucket.utilization;
        data.weekly.resets_at = parse_iso8601(bucket.resets_at.as_deref());
    }

    Ok(Some(data))
}

fn fetch_usage_via_messages(token: &str) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let mut last_error = PollError::RequestFailed;

    for model in MODEL_FALLBACK_CHAIN {
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "."}]
        });

        let response = match agent
            .post(MESSAGES_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .set("anthropic-version", "2023-06-01")
            .set("anthropic-beta", "oauth-2025-04-20")
            .send_json(&body)
        {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
                diagnose::log(format!(
                    "messages endpoint returned auth error status {code}; re-login required"
                ));
                return Err(PollError::AuthRequired);
            }
            Err(ureq::Error::Status(code, resp)) => {
                last_error =
                    if code == 429 {
                        PollError::RateLimited(resp.header("Retry-After").and_then(|value| {
                            crate::retry_after::delay_ms(value, SystemTime::now())
                        }))
                    } else {
                        classify_http_status(code)
                    };
                if matches!(last_error, PollError::RateLimited(_)) {
                    return Err(last_error);
                }
                resp
            }
            Err(error) => {
                last_error = classify_ureq_error(&error);
                continue;
            }
        };

        let h5 = response.header("anthropic-ratelimit-unified-5h-utilization");
        let h7 = response.header("anthropic-ratelimit-unified-7d-utilization");
        let hs = response.header("anthropic-ratelimit-unified-status");

        if h5.is_some() || h7.is_some() || hs.is_some() {
            return Ok(parse_rate_limit_headers(&response));
        }
    }

    Err(last_error)
}

fn parse_rate_limit_headers(response: &ureq::Response) -> UsageData {
    let mut data = UsageData::default();

    data.session.percentage =
        get_header_f64(response, "anthropic-ratelimit-unified-5h-utilization") * 100.0;
    data.session.resets_at = unix_to_system_time(get_header_i64(
        response,
        "anthropic-ratelimit-unified-5h-reset",
    ));

    data.weekly.percentage =
        get_header_f64(response, "anthropic-ratelimit-unified-7d-utilization") * 100.0;
    data.weekly.resets_at = unix_to_system_time(get_header_i64(
        response,
        "anthropic-ratelimit-unified-7d-reset",
    ));

    let overall_reset = get_header_i64(response, "anthropic-ratelimit-unified-reset");

    if data.session.percentage == 0.0 && data.weekly.percentage == 0.0 {
        let status = response.header("anthropic-ratelimit-unified-status");
        if status == Some("rejected") {
            let claim = response.header("anthropic-ratelimit-unified-representative-claim");
            match claim {
                Some("five_hour") => data.session.percentage = 100.0,
                Some("seven_day") => data.weekly.percentage = 100.0,
                _ => {}
            }
        }

        if data.session.resets_at.is_none() && overall_reset.is_some() {
            data.session.resets_at = unix_to_system_time(overall_reset);
        }
    }

    data
}

fn fetch_codex_usage(token: &str, account_id: Option<&str>) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let mut request = agent
        .get(CODEX_USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "codex-cli");

    if let Some(account_id) = account_id.filter(|value| !value.is_empty()) {
        request = request.set("ChatGPT-Account-Id", account_id);
    }

    let resp = match request.call() {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Codex usage endpoint request failed", error);
            return Err(classified);
        }
    };

    let response: CodexUsageResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error("unable to parse Codex usage response", error);
            return Err(PollError::RequestFailed);
        }
    };

    codex_usage_from_response(response).ok_or(PollError::RequestFailed)
}

fn codex_usage_from_response(response: CodexUsageResponse) -> Option<UsageData> {
    let details = *response.rate_limit.flatten()?;
    let mut data = UsageData::default();
    let mut has_session = false;
    let mut has_weekly = false;

    let primary = details.primary_window.flatten();
    let secondary = details.secondary_window.flatten();

    for window in [primary.as_deref(), secondary.as_deref()]
        .into_iter()
        .flatten()
    {
        match codex_window_kind(window) {
            Some(UsageWindowKind::Session) if !has_session => {
                data.session = codex_section_from_window(window);
                has_session = true;
            }
            Some(UsageWindowKind::Weekly) if !has_weekly => {
                data.weekly = codex_section_from_window(window);
                has_weekly = true;
            }
            _ => {}
        }
    }

    // Older responses did not include a window duration. Preserve their
    // positional meaning while avoiding overwriting a duration-classified
    // window from the newer response shape.
    if let Some(window) = primary
        .as_deref()
        .filter(|window| window.limit_window_seconds.is_none() && !has_session)
    {
        data.session = codex_section_from_window(window);
    }
    if let Some(window) = secondary
        .as_deref()
        .filter(|window| window.limit_window_seconds.is_none() && !has_weekly)
    {
        data.weekly = codex_section_from_window(window);
    }

    Some(data)
}

fn codex_window_kind(window: &CodexRateLimitWindow) -> Option<UsageWindowKind> {
    window.limit_window_seconds.map(|seconds| {
        if seconds <= CODEX_SESSION_WINDOW_MAX_SECONDS {
            UsageWindowKind::Session
        } else {
            UsageWindowKind::Weekly
        }
    })
}

fn codex_section_from_window(window: &CodexRateLimitWindow) -> UsageSection {
    UsageSection {
        percentage: window.used_percent,
        resets_at: unix_to_system_time(Some(window.reset_at)),
        reported: true,
    }
}

fn fetch_kimi_code_usage(token: &str, base_url: &str) -> Result<UsageData, PollError> {
    // Never forward a CLI credential through an HTTP redirect.
    let agent = build_agent_with_redirects(0)?;
    fetch_kimi_code_usage_with_agent(token, base_url, &agent)
}

fn fetch_kimi_code_usage_with_agent(
    token: &str,
    base_url: &str,
    agent: &ureq::Agent,
) -> Result<UsageData, PollError> {
    let url = format!("{}/usages", base_url.trim_end_matches('/'));
    let resp = match agent
        .get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .call()
    {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Kimi Code usage endpoint request failed", error);
            return Err(classified);
        }
    };

    if !(200..300).contains(&resp.status()) {
        return Err(classify_http_status(resp.status()));
    }
    let response: KimiCodeUsageResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error("unable to parse Kimi Code usage response", error);
            return Err(PollError::RequestFailed);
        }
    };
    kimi_code_usage_from_response(response).ok_or(PollError::RequestFailed)
}

fn kimi_code_ratio_section(pool: &KimiCodeRatioPool) -> Option<UsageSection> {
    let ratio = pool
        .used_ratio
        .as_ref()
        .and_then(kimi_code_numeric_value)
        .filter(|ratio| *ratio >= 0.0)?;
    Some(UsageSection {
        percentage: ratio.clamp(0.0, 1.0) * 100.0,
        resets_at: parse_iso8601(pool.reset_time.as_deref()),
        reported: true,
    })
}

fn kimi_code_count_section(detail: &KimiCodeUsageDetail) -> Option<UsageSection> {
    let limit = detail
        .limit
        .as_ref()
        .and_then(kimi_code_numeric_value)
        .filter(|limit| *limit > 0.0)?;
    let used = detail
        .used
        .as_ref()
        .and_then(kimi_code_numeric_value)
        .or_else(|| {
            detail
                .remaining
                .as_ref()
                .and_then(kimi_code_numeric_value)
                .map(|remaining| limit - remaining)
        })
        .filter(|used| *used >= 0.0)?;
    Some(UsageSection {
        percentage: (used / limit * 100.0).clamp(0.0, 100.0),
        resets_at: parse_iso8601(detail.reset_time.as_deref()),
        reported: true,
    })
}

fn kimi_code_is_session_window(window: &KimiCodeWindow) -> bool {
    let seconds_per_unit = match window.time_unit.as_deref() {
        Some("TIME_UNIT_MINUTE") => 60,
        Some("TIME_UNIT_HOUR") => 60 * 60,
        Some("TIME_UNIT_DAY") => 24 * 60 * 60,
        _ => return false,
    };
    window
        .duration
        .and_then(|duration| duration.checked_mul(seconds_per_unit))
        == Some(5 * 60 * 60)
}

fn kimi_code_reset_times_match(left: Option<SystemTime>, right: Option<SystemTime>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left
            .duration_since(right)
            .or_else(|_| right.duration_since(left))
            .is_ok_and(|difference| difference <= Duration::from_secs(2)),
        _ => false,
    }
}

fn kimi_code_merge_section(
    ratio: Option<UsageSection>,
    count: Option<UsageSection>,
) -> Option<UsageSection> {
    match (ratio, count) {
        (Some(mut ratio), Some(count)) => {
            // Some regional responses temporarily include a zero ratio beside
            // authoritative legacy counters. Reconcile only matching windows.
            if kimi_code_reset_times_match(ratio.resets_at, count.resets_at) {
                ratio.percentage = ratio.percentage.max(count.percentage);
            }
            Some(ratio)
        }
        (Some(ratio), None) => Some(ratio),
        (None, count) => count,
    }
}

fn kimi_code_usage_from_response(response: KimiCodeUsageResponse) -> Option<UsageData> {
    let KimiCodeUsageResponse {
        usage,
        usages,
        limits,
    } = response;
    let (ratio_session, ratio_weekly) = match usages {
        Some(usages) => (
            usages.limit_5h.as_ref().and_then(kimi_code_ratio_section),
            usages.limit_7d.as_ref().and_then(kimi_code_ratio_section),
        ),
        None => (None, None),
    };
    let count_weekly = usage.as_ref().and_then(kimi_code_count_section);
    let count_session = limits.unwrap_or_default().iter().find_map(|limit| {
        kimi_code_is_session_window(&limit.window)
            .then(|| kimi_code_count_section(&limit.detail))
            .flatten()
    });
    let session = kimi_code_merge_section(ratio_session, count_session);
    let weekly = kimi_code_merge_section(ratio_weekly, count_weekly);
    if session.is_none() && weekly.is_none() {
        return None;
    }
    Some(UsageData {
        session: session.unwrap_or_default(),
        weekly: weekly.unwrap_or_default(),
    })
}

fn antigravity_credential_watch_signature() -> String {
    let Some(content) = read_windows_generic_credential(ANTIGRAVITY_CREDENTIAL_TARGET) else {
        return format!("{ANTIGRAVITY_CREDENTIAL_TARGET}|missing");
    };

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!(
        "{ANTIGRAVITY_CREDENTIAL_TARGET}|present|{}|{}",
        content.len(),
        hasher.finish()
    )
}

fn fetch_antigravity_usage(token: &str) -> Result<UsageData, PollError> {
    let mut auth_error = false;
    let mut last_error = PollError::RequestFailed;

    for base_url in ANTIGRAVITY_ENDPOINTS {
        match fetch_antigravity_usage_from_endpoint(base_url, token) {
            Ok(data) => return Ok(data),
            Err(PollError::AuthRequired) => auth_error = true,
            Err(error) => last_error = error,
        }
    }

    if auth_error {
        Err(PollError::AuthRequired)
    } else {
        Err(last_error)
    }
}

fn fetch_antigravity_usage_from_endpoint(
    base_url: &str,
    token: &str,
) -> Result<UsageData, PollError> {
    let project = fetch_antigravity_project(base_url, token)?;
    if let Some(project) = project.as_deref() {
        match fetch_antigravity_quota_summary(base_url, token, project) {
            Ok(data) => return Ok(data),
            Err(PollError::AuthRequired) => return Err(PollError::AuthRequired),
            Err(error) => diagnose::log(format!(
                "Antigravity retrieveUserQuotaSummary failed, falling back to model quota: {error:?}"
            )),
        }
    }

    let session = fetch_antigravity_model_quota(base_url, token, project.as_deref())?;
    let weekly = UsageSection::default();

    Ok(UsageData { session, weekly })
}

fn fetch_antigravity_project(base_url: &str, token: &str) -> Result<Option<String>, PollError> {
    let agent = build_agent()?;
    let body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY"
        }
    });

    let resp = match agent
        .post(&format!("{base_url}/v1internal:loadCodeAssist"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Antigravity loadCodeAssist request failed", error);
            return Err(classified);
        }
    };

    let response: AntigravityLoadResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error("unable to parse Antigravity loadCodeAssist response", error);
            return Err(PollError::RequestFailed);
        }
    };

    Ok(response.project.filter(|project| !project.is_empty()))
}

fn fetch_antigravity_model_quota(
    base_url: &str,
    token: &str,
    project: Option<&str>,
) -> Result<UsageSection, PollError> {
    let agent = build_agent()?;
    let body = match project {
        Some(project) => serde_json::json!({ "project": project }),
        None => serde_json::json!({}),
    };

    let resp = match agent
        .post(&format!("{base_url}/v1internal:fetchAvailableModels"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Antigravity fetchAvailableModels request failed", error);
            return Err(classified);
        }
    };

    let response: AntigravityModelsResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error(
                "unable to parse Antigravity fetchAvailableModels response",
                error,
            );
            return Err(PollError::RequestFailed);
        }
    };

    best_antigravity_section(response.models.into_iter().filter_map(|(model, info)| {
        let quota = info.quota_info?;
        if !is_antigravity_display_model(&model) {
            return None;
        }
        antigravity_section_from_quota(quota)
    }))
    .ok_or(PollError::RequestFailed)
}

fn fetch_antigravity_quota_summary(
    base_url: &str,
    token: &str,
    project: &str,
) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let body = serde_json::json!({ "project": project });

    let resp = match agent
        .post(&format!("{base_url}/v1internal:retrieveUserQuotaSummary"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(error) => {
            let classified = classify_ureq_error(&error);
            diagnose::log_error("Antigravity retrieveUserQuotaSummary request failed", error);
            return Err(classified);
        }
    };

    let response: AntigravityQuotaSummaryResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error(
                "unable to parse Antigravity retrieveUserQuotaSummary response",
                error,
            );
            return Err(PollError::RequestFailed);
        }
    };

    antigravity_usage_from_summary(response).ok_or(PollError::RequestFailed)
}

fn antigravity_section_from_quota(quota: AntigravityQuotaInfo) -> Option<UsageSection> {
    let remaining = quota.remaining_fraction?.clamp(0.0, 1.0);
    Some(UsageSection {
        percentage: (1.0 - remaining) * 100.0,
        resets_at: parse_iso8601(quota.reset_time.as_deref()),
        reported: true,
    })
}

fn antigravity_section_from_summary_bucket(
    bucket: &AntigravityQuotaSummaryBucket,
) -> Option<UsageSection> {
    let remaining = bucket.remaining_fraction?.clamp(0.0, 1.0);
    Some(UsageSection {
        percentage: (1.0 - remaining) * 100.0,
        resets_at: parse_iso8601(bucket.reset_time.as_deref()),
        reported: true,
    })
}

fn antigravity_usage_from_summary(response: AntigravityQuotaSummaryResponse) -> Option<UsageData> {
    let mut fallback = None;

    for group in response.groups.unwrap_or_default() {
        let is_gemini = is_antigravity_gemini_summary_group(&group);
        let usage = antigravity_usage_from_summary_group(group);

        if is_gemini && usage.is_some() {
            return usage;
        }

        if fallback.is_none() {
            fallback = usage;
        }
    }

    fallback
}

fn antigravity_usage_from_summary_group(group: AntigravityQuotaSummaryGroup) -> Option<UsageData> {
    let mut data = UsageData::default();
    let mut has_quota = false;

    for bucket in group.buckets.unwrap_or_default() {
        let Some(section) = antigravity_section_from_summary_bucket(&bucket) else {
            continue;
        };

        match bucket.window.as_deref() {
            Some(window) if window.eq_ignore_ascii_case("5h") => {
                data.session = section;
                has_quota = true;
            }
            Some(window) if window.eq_ignore_ascii_case("weekly") => {
                data.weekly = section;
                has_quota = true;
            }
            _ => {}
        }
    }

    has_quota.then_some(data)
}

fn is_antigravity_gemini_summary_group(group: &AntigravityQuotaSummaryGroup) -> bool {
    group
        .display_name
        .as_deref()
        .is_some_and(|name| name.to_ascii_lowercase().contains("gemini"))
        || group
            .description
            .as_deref()
            .is_some_and(|description| description.to_ascii_lowercase().contains("gemini"))
        || group.buckets.as_ref().is_some_and(|buckets| {
            buckets.iter().any(|bucket| {
                bucket
                    .bucket_id
                    .as_deref()
                    .is_some_and(|id| id.to_ascii_lowercase().starts_with("gemini-"))
                    || bucket
                        .display_name
                        .as_deref()
                        .is_some_and(|name| name.to_ascii_lowercase().contains("gemini"))
            })
        })
}

fn best_antigravity_section<I>(sections: I) -> Option<UsageSection>
where
    I: IntoIterator<Item = UsageSection>,
{
    sections.into_iter().max_by(|a, b| {
        a.percentage
            .partial_cmp(&b.percentage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.resets_at.cmp(&b.resets_at))
    })
}

fn is_antigravity_display_model(model: &str) -> bool {
    model.starts_with("gemini")
        || model.starts_with("claude")
        || model.starts_with("gpt")
        || model.starts_with("image")
        || model.starts_with("imagen")
}

fn get_header_f64(response: &ureq::Response, name: &str) -> f64 {
    response
        .header(name)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn get_header_i64(response: &ureq::Response, name: &str) -> Option<i64> {
    response.header(name).and_then(|s| s.parse::<i64>().ok())
}

fn unix_to_system_time(unix_secs: Option<i64>) -> Option<SystemTime> {
    let secs = unix_secs?;
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

struct Credentials {
    access_token: String,
    expires_at: Option<i64>,
    source: CredentialSource,
}

#[derive(Clone, Debug)]
enum CredentialSource {
    Windows(PathBuf),
    Wsl { distro: String },
}

fn read_first_credentials() -> Option<Credentials> {
    if let Some(creds) = read_windows_credentials() {
        return Some(creds);
    }

    for distro in list_wsl_distros() {
        if let Some(creds) = read_wsl_credentials(&distro) {
            return Some(creds);
        }
    }

    None
}

fn read_windows_credentials() -> Option<Credentials> {
    let CredentialSource::Windows(cred_path) = windows_credential_source()? else {
        return None;
    };
    let content = match std::fs::read_to_string(&cred_path) {
        Ok(content) => content,
        Err(error) => {
            if diagnose::is_enabled() {
                diagnose::log_error(
                    &format!(
                        "unable to read Windows credentials at {}",
                        cred_path.display()
                    ),
                    error,
                );
            }
            return None;
        }
    };
    parse_credentials(&content, CredentialSource::Windows(cred_path))
}

fn read_credentials_from_source(source: &CredentialSource) -> Option<Credentials> {
    match source {
        CredentialSource::Windows(path) => {
            let content = std::fs::read_to_string(path).ok()?;
            parse_credentials(&content, source.clone())
        }
        CredentialSource::Wsl { distro } => read_wsl_credentials(distro),
    }
}

fn codex_auth_path() -> Option<PathBuf> {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME").map(PathBuf::from) {
        return Some(codex_home.join("auth.json"));
    }

    Some(dirs::home_dir()?.join(".codex").join("auth.json"))
}

fn read_codex_credentials() -> Option<CodexTokenData> {
    let auth_path = codex_auth_path()?;
    let content = match std::fs::read_to_string(&auth_path) {
        Ok(content) => content,
        Err(error) => {
            diagnose::log_error(
                &format!(
                    "unable to read Codex credentials at {}",
                    auth_path.display()
                ),
                error,
            );
            return None;
        }
    };

    let auth: CodexAuthFile = serde_json::from_str(&content).ok()?;
    auth.tokens.filter(|tokens| !tokens.access_token.is_empty())
}

fn cleaned_kimi_code_value(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn kimi_code_environment() -> KimiCodeEnvironment {
    KimiCodeEnvironment {
        api_key: cleaned_kimi_code_value(std::env::var("KIMI_CODE_API_KEY").ok()),
        base_url: cleaned_kimi_code_value(std::env::var("KIMI_CODE_BASE_URL").ok()),
        oauth_host: cleaned_kimi_code_value(
            std::env::var("KIMI_CODE_OAUTH_HOST")
                .ok()
                .or_else(|| std::env::var("KIMI_OAUTH_HOST").ok()),
        ),
    }
}

fn kimi_code_home() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KIMI_CODE_HOME") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    Some(dirs::home_dir()?.join(".kimi-code"))
}

fn normalize_kimi_code_base_url(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('/');
    let parsed = url::Url::parse(value).ok()?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || value.contains('\\')
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return None;
    }
    Some(value.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KimiCodeOAuthEnvironment<'a> {
    oauth_host: &'a str,
    base_url: &'a str,
}

fn kimi_code_credential_name(oauth_host: &str, base_url: &str) -> String {
    let oauth_host = oauth_host.trim().trim_end_matches('/');
    let base_url = base_url.trim().trim_end_matches('/');
    if oauth_host == KIMI_CODE_DEFAULT_OAUTH_HOST && base_url == KIMI_CODE_DEFAULT_BASE_URL {
        return KIMI_CODE_DEFAULT_CREDENTIAL.to_string();
    }
    let environment = KimiCodeOAuthEnvironment {
        oauth_host,
        base_url,
    };
    let serialized = serde_json::to_string(&environment).unwrap_or_default();
    let digest = Sha256::digest(serialized.as_bytes());
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("kimi-code-env-{suffix}")
}

fn kimi_code_storage_name(key: &str) -> Option<String> {
    let name = key.strip_prefix("oauth/").unwrap_or(key);
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) || name == ".." {
        return None;
    }
    Some(name.to_string())
}

fn kimi_code_auth_plan() -> Option<KimiCodeAuthPlan> {
    let home = kimi_code_home()?;
    kimi_code_auth_plan_from(&home, &kimi_code_environment())
}

fn kimi_code_auth_plan_from(
    home: &std::path::Path,
    environment: &KimiCodeEnvironment,
) -> Option<KimiCodeAuthPlan> {
    let config_path = home.join("config.toml");
    let mut config = match std::fs::read_to_string(&config_path) {
        Ok(content) => toml::from_str::<KimiCodeConfig>(&content).ok()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => KimiCodeConfig::default(),
        Err(_) => return None,
    };
    let provider = config
        .providers
        .remove("managed:kimi-code")
        .unwrap_or_default();
    let oauth = provider.oauth.unwrap_or_default();
    let has_environment_override =
        environment.base_url.is_some() || environment.oauth_host.is_some();
    let oauth_host = if has_environment_override {
        environment.oauth_host.as_deref()
    } else {
        environment
            .oauth_host
            .as_deref()
            .or(oauth.oauth_host.as_deref())
    }
    .unwrap_or(KIMI_CODE_DEFAULT_OAUTH_HOST)
    .trim()
    .trim_end_matches('/')
    .to_string();
    let default_base_url = if oauth_host == "https://auth.kimi.ai" {
        "https://api.kimi.ai/coding/v1"
    } else {
        KIMI_CODE_DEFAULT_BASE_URL
    };
    let configured_base_url = provider.base_url.as_deref().unwrap_or(default_base_url);
    let base_url = normalize_kimi_code_base_url(
        environment
            .base_url
            .as_deref()
            .unwrap_or(configured_base_url),
    )?;
    // An endpoint override must not carry an API key from an unrelated config.
    let can_reuse_config_key = environment.base_url.as_deref().is_none_or(|value| {
        normalize_kimi_code_base_url(value) == normalize_kimi_code_base_url(configured_base_url)
    });

    if let Some(api_key) = environment.api_key.clone().or_else(|| {
        can_reuse_config_key
            .then(|| cleaned_kimi_code_value(provider.api_key))
            .flatten()
    }) {
        return Some(KimiCodeAuthPlan {
            base_url,
            source: KimiCodeTokenSource::ApiKey(api_key),
            config_path,
        });
    }

    let expected_name = kimi_code_credential_name(&oauth_host, &base_url);
    if !has_environment_override {
        if oauth
            .storage
            .as_deref()
            .is_some_and(|storage| storage != "file")
        {
            return None;
        }
        if let Some(key) = oauth.key.as_deref() {
            // Fail closed rather than silently selecting an old/default account.
            if kimi_code_storage_name(key).as_deref() != Some(expected_name.as_str()) {
                return None;
            }
        }
    }
    let credential_name = expected_name;
    Some(KimiCodeAuthPlan {
        base_url,
        source: KimiCodeTokenSource::OAuth(
            home.join("credentials")
                .join(format!("{credential_name}.json")),
        ),
        config_path,
    })
}

fn read_kimi_code_token_file(path: &std::path::Path) -> Option<KimiCodeTokenFile> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn kimi_code_numeric_value(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

fn read_kimi_code_credentials() -> Result<KimiCodeCredentials, PollError> {
    // An in-app choice is authoritative, including its region. Never apply an
    // environment endpoint override to a key stored through the native editor.
    if let Some(key) = kimi_credentials::load().map_err(|_| PollError::NoCredentials)? {
        return Ok(KimiCodeCredentials {
            access_token: key.api_key,
            base_url: key.region.base_url().to_string(),
        });
    }
    let home = kimi_code_home().ok_or(PollError::NoCredentials)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    read_kimi_code_credentials_from(&home, &kimi_code_environment(), now)
}

fn read_kimi_code_credentials_from(
    home: &std::path::Path,
    environment: &KimiCodeEnvironment,
    now: f64,
) -> Result<KimiCodeCredentials, PollError> {
    let plan = kimi_code_auth_plan_from(home, environment).ok_or(PollError::NoCredentials)?;
    let access_token = match plan.source {
        KimiCodeTokenSource::ApiKey(token) => token,
        KimiCodeTokenSource::OAuth(path) => {
            let token = read_kimi_code_token_file(&path).ok_or(PollError::NoCredentials)?;
            let access_token =
                cleaned_kimi_code_value(token.access_token).ok_or(PollError::NoCredentials)?;
            let expires_at = token
                .expires_at
                .as_ref()
                .and_then(kimi_code_numeric_value)
                .ok_or(PollError::TokenExpired)?;
            if expires_at <= now + 60.0 {
                return Err(PollError::TokenExpired);
            }
            access_token
        }
    };
    Ok(KimiCodeCredentials {
        access_token,
        base_url: plan.base_url,
    })
}

fn kimi_code_credential_watch_snapshot() -> CredentialWatchSnapshot {
    let signature = kimi_credentials::watch_signature();
    if signature != "kimi-key|missing" {
        return vec![signature];
    }
    let Some(home) = kimi_code_home() else {
        return vec![signature];
    };
    let environment = kimi_code_environment();
    let mut snapshot = vec![signature];
    if let Some(plan) = kimi_code_auth_plan_from(&home, &environment) {
        snapshot.push(windows_credential_watch_signature(&plan.config_path));
        if let KimiCodeTokenSource::OAuth(path) = plan.source {
            snapshot.push(windows_credential_watch_signature(&path));
        }
    } else {
        snapshot.push(windows_credential_watch_signature(
            &home.join("config.toml"),
        ));
    }
    snapshot.sort();
    snapshot.dedup();
    snapshot
}

fn read_antigravity_credentials() -> Option<AntigravityTokenData> {
    let content = read_windows_generic_credential(ANTIGRAVITY_CREDENTIAL_TARGET)?;
    let auth: AntigravityAuthFile = serde_json::from_str(&content).ok()?;
    if auth.token.access_token.is_empty() {
        None
    } else {
        Some(auth.token)
    }
}

fn read_windows_generic_credential(target: &str) -> Option<String> {
    const CRED_TYPE_GENERIC: u32 = 1;

    let mut target_wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let mut credential: *mut CredentialW = std::ptr::null_mut();

    let ok = unsafe {
        CredReadW(
            target_wide.as_mut_ptr(),
            CRED_TYPE_GENERIC,
            0,
            &mut credential,
        )
    };

    if ok == 0 || credential.is_null() {
        diagnose::log(format!(
            "unable to read Windows generic credential target {target}"
        ));
        return None;
    }

    let result = unsafe {
        let cred = &*credential;
        if cred.credential_blob_size == 0 || cred.credential_blob.is_null() {
            CredFree(credential as *mut c_void);
            return None;
        }
        let bytes =
            std::slice::from_raw_parts(cred.credential_blob, cred.credential_blob_size as usize);
        let text = String::from_utf8(bytes.to_vec()).ok();
        CredFree(credential as *mut c_void);
        text
    };

    result
}

fn read_wsl_credentials(distro: &str) -> Option<Credentials> {
    let output = run_with_timeout(
        Command::new("wsl.exe")
            .arg("-d")
            .arg(distro)
            .arg("--")
            .arg("sh")
            .arg("-lc")
            .arg("cat ~/.claude/.credentials.json")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    )?;

    if !output.status.success() {
        diagnose::log(format!(
            "WSL credentials probe failed for distro {distro} with status {}",
            output.status
        ));
        return None;
    }

    let content = String::from_utf8(output.stdout).ok()?;
    parse_credentials(
        &content,
        CredentialSource::Wsl {
            distro: distro.to_string(),
        },
    )
}

fn parse_credentials(content: &str, source: CredentialSource) -> Option<Credentials> {
    let json: serde_json::Value = serde_json::from_str(content).ok()?;

    let oauth = json.get("claudeAiOauth")?;
    let access_token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())?
        .to_string();
    let expires_at = oauth.get("expiresAt").and_then(|v| v.as_i64());

    Some(Credentials {
        access_token,
        expires_at,
        source,
    })
}

fn read_next_credentials_after(source: &CredentialSource) -> Option<Credentials> {
    match source {
        CredentialSource::Windows(_) => {
            for distro in list_wsl_distros() {
                if let Some(creds) = read_wsl_credentials(&distro) {
                    return Some(creds);
                }
            }
        }
        CredentialSource::Wsl { distro } => {
            let mut past_current = false;
            for candidate_distro in list_wsl_distros() {
                if !past_current {
                    past_current = candidate_distro == *distro;
                    continue;
                }
                if let Some(creds) = read_wsl_credentials(&candidate_distro) {
                    return Some(creds);
                }
            }
        }
    }

    None
}

fn list_wsl_distros() -> Vec<String> {
    let output = match run_with_timeout(
        Command::new("wsl.exe")
            .args(["-l", "-q"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    ) {
        Some(output) if output.status.success() => output,
        _ => {
            diagnose::log("unable to enumerate WSL distros");
            return Vec::new();
        }
    };

    let stdout = decode_wsl_text(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn decode_wsl_text(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    if let Some(decoded) = decode_utf16le(bytes) {
        return decoded;
    }

    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16le(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 || bytes.len() % 2 != 0 {
        return None;
    }

    let body = if bytes.starts_with(&[0xFF, 0xFE]) {
        &bytes[2..]
    } else if looks_like_utf16le(bytes) {
        bytes
    } else {
        return None;
    };

    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    Some(String::from_utf16_lossy(&units))
}

fn looks_like_utf16le(bytes: &[u8]) -> bool {
    let sample_len = bytes.len().min(128);
    let units = sample_len / 2;
    if units == 0 {
        return false;
    }

    let nul_high_bytes = bytes[..sample_len]
        .chunks_exact(2)
        .filter(|chunk| chunk[1] == 0)
        .count();

    nul_high_bytes * 2 >= units
}

fn is_token_expired(expires_at: Option<i64>) -> bool {
    let Some(exp) = expires_at else { return false };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now >= exp
}

/// Parse an ISO 8601 / RFC 3339 timestamp into an absolute SystemTime.
fn parse_iso8601(s: Option<&str>) -> Option<SystemTime> {
    let s = s?.trim();
    let time_start = s.find('T')?;
    let offset_start = s
        .char_indices()
        .skip(time_start + 1)
        .find(|(_, character)| matches!(character, '+' | '-'))
        .map(|(index, _)| index);
    let (datetime_part, offset_seconds) = if let Some(index) = offset_start {
        let (datetime, offset) = s.split_at(index);
        (datetime, parse_iso8601_offset(offset)?)
    } else if let Some(datetime) = s.strip_suffix('Z').or_else(|| s.strip_suffix('z')) {
        (datetime, 0)
    } else {
        (s, 0)
    };
    let local_seconds: i64 = parse_datetime_to_unix(datetime_part, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()?
        .try_into()
        .ok()?;
    unix_to_system_time(Some(local_seconds.checked_sub(offset_seconds)?))
}

fn parse_iso8601_offset(value: &str) -> Option<i64> {
    let (sign, value) = match value.as_bytes().first()? {
        b'+' => (1i64, &value[1..]),
        b'-' => (-1i64, &value[1..]),
        _ => return None,
    };
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 2 || byte.is_ascii_digit())
    {
        return None;
    }
    let hours = i64::from(bytes[0] - b'0') * 10 + i64::from(bytes[1] - b'0');
    let minutes = i64::from(bytes[3] - b'0') * 10 + i64::from(bytes[4] - b'0');
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Minimal datetime parser — avoids pulling in chrono/time crates.
fn parse_datetime_to_unix(s: &str, _fmt: &str) -> Result<u64, ()> {
    // Extract date and time parts from "YYYY-MM-DDTHH:MM:SS[.frac]"
    let (date_str, time_str) = s.split_once('T').ok_or(())?;
    let date_parts: Vec<&str> = date_str.split('-').collect();
    if date_parts.len() != 3 {
        return Err(());
    }

    let year: u64 = date_parts[0].parse().map_err(|_| ())?;
    let month: u64 = date_parts[1].parse().map_err(|_| ())?;
    let day: u64 = date_parts[2].parse().map_err(|_| ())?;

    // Strip fractional seconds
    let time_base = time_str.split('.').next().unwrap_or(time_str);
    let time_parts: Vec<&str> = time_base.split(':').collect();
    if time_parts.len() != 3 {
        return Err(());
    }

    let hour: u64 = time_parts[0].parse().map_err(|_| ())?;
    let min: u64 = time_parts[1].parse().map_err(|_| ())?;
    let sec: u64 = time_parts[2].parse().map_err(|_| ())?;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || hour > 23
        || min > 59
        || sec > 59
    {
        return Err(());
    }
    let mut days_in_month = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if is_leap(year) {
        days_in_month[1] = 29;
    }
    if day == 0 || day > days_in_month[(month - 1) as usize] {
        return Err(());
    }

    // Days from year (using a simplified calculation for dates after 1970)
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }

    let month_days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[m as usize];
        if m == 2 && is_leap(year) {
            days += 1;
        }
    }
    days += day - 1;

    Ok(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Format optional Codex/Kimi quota windows without inventing missing quotas.
pub fn format_codex_line(
    section: &UsageSection,
    strings: Strings,
    show_remaining_in_chinese: bool,
    window: UsageWindowKind,
) -> String {
    if !section.reported && section.percentage == 0.0 && section.resets_at.is_none() {
        return "--".to_string();
    }
    format_line(section, strings, show_remaining_in_chinese, window)
}

/// Format a usage section for the compact taskbar display.
pub fn format_line(
    section: &UsageSection,
    strings: Strings,
    show_remaining_in_chinese: bool,
    window: UsageWindowKind,
) -> String {
    if show_remaining_in_chinese {
        return format_simplified_chinese_line(section, window);
    }

    let pct = format!("{:.0}%", section.percentage);
    let cd = format_countdown(section.resets_at, strings);
    if cd.is_empty() {
        pct
    } else {
        format!("{pct} \u{00b7} {cd}")
    }
}

fn format_simplified_chinese_line(section: &UsageSection, window: UsageWindowKind) -> String {
    let remaining = remaining_percentage(section.percentage);
    let reset = section
        .resets_at
        .and_then(native_interop::system_time_to_local);
    format_simplified_chinese_values(remaining, reset, window)
}

fn format_simplified_chinese_values(
    remaining: f64,
    reset: Option<windows::Win32::Foundation::SYSTEMTIME>,
    window: UsageWindowKind,
) -> String {
    let Some(reset) = reset else {
        return format!("{remaining:.0}%");
    };
    match window {
        UsageWindowKind::Session => {
            format!(
                "{remaining:.0}%  {:02}:{:02}重置",
                reset.wHour, reset.wMinute
            )
        }
        UsageWindowKind::Weekly => {
            format!("{remaining:.0}%  {:02}/{:02}重置", reset.wMonth, reset.wDay)
        }
    }
}

pub fn remaining_percentage(used_percentage: f64) -> f64 {
    (100.0 - used_percentage).clamp(0.0, 100.0)
}

fn format_countdown(resets_at: Option<SystemTime>, strings: Strings) -> String {
    let reset = match resets_at {
        Some(t) => t,
        None => return String::new(),
    };

    let remaining = match reset.duration_since(SystemTime::now()) {
        Ok(d) => d,
        Err(_) => return strings.now.to_string(),
    };

    format_countdown_from_secs(remaining.as_secs(), strings)
}

/// Calculate how long until the display text would change
pub fn time_until_display_change(resets_at: Option<SystemTime>) -> Option<Duration> {
    let reset = resets_at?;
    let remaining = reset.duration_since(SystemTime::now()).ok()?;
    Some(time_until_display_change_from_secs(remaining.as_secs()))
}

fn format_countdown_from_secs(total_secs: u64, strings: Strings) -> String {
    let total_mins = total_secs / 60;
    let total_hours = total_secs / 3600;
    let total_days = total_secs / 86400;

    if total_days >= 1 {
        format!("{total_days}{}", strings.day_suffix)
    } else if total_hours >= 1 {
        format!("{total_hours}{}", strings.hour_suffix)
    } else if total_mins >= 1 {
        format!("{total_mins}{}", strings.minute_suffix)
    } else {
        format!("{total_secs}{}", strings.second_suffix)
    }
}

fn time_until_display_change_from_secs(total_secs: u64) -> Duration {
    let total_mins = total_secs / 60;
    let total_hours = total_secs / 3600;
    let total_days = total_secs / 86400;

    let current_bucket_start = if total_days >= 1 {
        total_days * 86400
    } else if total_hours >= 1 {
        total_hours * 3600
    } else if total_mins >= 1 {
        total_mins * 60
    } else {
        total_secs
    };

    Duration::from_secs(total_secs.saturating_sub(current_bucket_start) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_signature_detects_same_length_rewrites_within_one_second() {
        let path = std::env::temp_dir().join(format!(
            "quota-credential-signature-{}.json",
            std::process::id()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        use std::io::Write;
        file.write_all(b"fixture").unwrap();
        let time = UNIX_EPOCH + Duration::from_secs(1700000000);
        file.set_modified(time + Duration::from_millis(100))
            .unwrap();
        let before = windows_credential_watch_signature(&path);
        file.set_modified(time + Duration::from_millis(200))
            .unwrap();
        let after = windows_credential_watch_signature(&path);
        assert_ne!(before, after);
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn claude_credentials_path_honors_custom_config_directory() {
        assert_eq!(
            windows_credentials_path_from(
                Some(PathBuf::from(r"D:\claude-config")),
                Some(PathBuf::from(r"C:\Users\Ray")),
            ),
            Some(PathBuf::from(r"D:\claude-config\.credentials.json"))
        );
        assert_eq!(
            windows_credentials_path_from(None, Some(PathBuf::from(r"C:\Users\Ray"))),
            Some(PathBuf::from(r"C:\Users\Ray\.claude\.credentials.json"))
        );
    }

    #[test]
    fn remaining_percentage_is_clamped() {
        assert_eq!(remaining_percentage(30.0), 70.0);
        assert_eq!(remaining_percentage(-5.0), 100.0);
        assert_eq!(remaining_percentage(120.0), 0.0);
    }

    #[test]
    fn codex_weekly_only_window_is_not_misreported_as_session_usage() {
        let response: CodexUsageResponse = serde_json::from_str(
            r#"{
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 21,
                        "limit_window_seconds": 604800,
                        "reset_at": 1784500338
                    },
                    "secondary_window": null
                }
            }"#,
        )
        .expect("weekly-only response should deserialize");

        let usage = codex_usage_from_response(response).expect("rate limit should be available");

        assert_eq!(usage.session.percentage, 0.0);
        assert!(usage.session.resets_at.is_none());
        assert_eq!(usage.weekly.percentage, 21.0);
        assert!(usage.weekly.resets_at.is_some());
        for language in [
            crate::localization::LanguageId::SimplifiedChinese,
            crate::localization::LanguageId::English,
        ] {
            assert_eq!(
                format_codex_line(
                    &usage.session,
                    language.strings(),
                    language == crate::localization::LanguageId::SimplifiedChinese,
                    UsageWindowKind::Session,
                ),
                "--"
            );
        }
    }

    #[test]
    fn codex_legacy_windows_keep_positional_mapping_without_durations() {
        let response: CodexUsageResponse = serde_json::from_str(
            r#"{
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 18,
                        "reset_at": 1784500338
                    },
                    "secondary_window": {
                        "used_percent": 33,
                        "reset_at": 1785000000
                    }
                }
            }"#,
        )
        .expect("legacy response should deserialize");

        let usage = codex_usage_from_response(response).expect("rate limit should be available");

        assert_eq!(usage.session.percentage, 18.0);
        assert_eq!(usage.weekly.percentage, 33.0);
    }

    #[test]
    fn codex_session_only_zero_usage_keeps_real_quota_visible() {
        let response: CodexUsageResponse = serde_json::from_str(
            r#"{
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 0,
                        "limit_window_seconds": 18000,
                        "reset_at": 1784500338
                    },
                    "secondary_window": null
                }
            }"#,
        )
        .unwrap();
        let usage = codex_usage_from_response(response).unwrap();
        let strings = crate::localization::LanguageId::SimplifiedChinese.strings();
        assert!(
            format_codex_line(&usage.session, strings, true, UsageWindowKind::Session)
                .starts_with("100%")
        );
        assert_eq!(
            format_codex_line(&usage.weekly, strings, true, UsageWindowKind::Weekly),
            "--"
        );
    }

    #[test]
    fn rate_limit_response_preserves_valid_retry_hints() {
        for (header, expected) in [("120", Some(120_000)), ("invalid", None)] {
            let response: ureq::Response =
                format!("HTTP/1.1 429 Too Many Requests\r\nRetry-After: {header}\r\n\r\n")
                    .parse()
                    .unwrap();
            assert_eq!(
                classify_ureq_error(&ureq::Error::Status(429, response)),
                PollError::RateLimited(expected)
            );
        }
    }

    #[test]
    fn classifies_http_failures_for_user_visible_recovery() {
        assert_eq!(classify_http_status(401), PollError::AuthRequired);
        assert_eq!(classify_http_status(403), PollError::AuthRequired);
        assert_eq!(classify_http_status(429), PollError::RateLimited(None));
        assert_eq!(classify_http_status(500), PollError::ServerError);
        assert_eq!(classify_http_status(503), PollError::ServerError);
        assert_eq!(classify_http_status(404), PollError::RequestFailed);
        assert_eq!(
            PollError::NetworkUnavailable.category(),
            "network_unavailable"
        );
    }

    #[test]
    fn simplified_chinese_line_labels_remaining_usage() {
        let strings = crate::localization::LanguageId::SimplifiedChinese.strings();
        assert_eq!(strings.session_window, "5h");
        assert_eq!(strings.weekly_window, "7d");
        let section = UsageSection {
            percentage: 30.0,
            resets_at: None,
            reported: true,
        };
        assert_eq!(
            format_line(&section, strings, true, UsageWindowKind::Session),
            "70%"
        );
        let session_reset = windows::Win32::Foundation::SYSTEMTIME {
            wHour: 18,
            wMinute: 30,
            ..Default::default()
        };
        assert_eq!(
            format_simplified_chinese_values(82.0, Some(session_reset), UsageWindowKind::Session,),
            "82%  18:30重置"
        );
        let weekly_reset = windows::Win32::Foundation::SYSTEMTIME {
            wMonth: 7,
            wDay: 17,
            ..Default::default()
        };
        assert_eq!(
            format_simplified_chinese_values(97.0, Some(weekly_reset), UsageWindowKind::Weekly,),
            "97%  07/17重置"
        );
    }

    #[test]
    fn antigravity_summary_prefers_gemini_group() {
        let response: AntigravityQuotaSummaryResponse = serde_json::from_str(
            r#"{
                "groups": [
                    {
                        "displayName": "Claude and GPT models",
                        "buckets": [
                            {
                                "bucketId": "3p-weekly",
                                "window": "weekly",
                                "resetTime": "2026-06-20T18:32:02Z",
                                "remainingFraction": 1
                            },
                            {
                                "bucketId": "3p-5h",
                                "window": "5h",
                                "resetTime": "2026-06-13T23:32:02Z",
                                "remainingFraction": 1
                            }
                        ]
                    },
                    {
                        "displayName": "Gemini Models",
                        "description": "Models within this group: Gemini Flash, Gemini Pro",
                        "buckets": [
                            {
                                "bucketId": "gemini-weekly",
                                "displayName": "Weekly Limit",
                                "window": "weekly",
                                "resetTime": "2026-06-20T17:08:54Z",
                                "remainingFraction": 0.99304295
                            },
                            {
                                "bucketId": "gemini-5h",
                                "displayName": "Five Hour Limit",
                                "window": "5h",
                                "resetTime": "2026-06-13T22:08:54Z",
                                "remainingFraction": 0.9582575
                            }
                        ]
                    }
                ]
            }"#,
        )
        .expect("summary response should deserialize");

        let usage =
            antigravity_usage_from_summary(response).expect("Gemini quota should be selected");

        assert!((usage.weekly.percentage - 0.695705).abs() < 0.000001);
        assert!((usage.session.percentage - 4.17425).abs() < 0.000001);
        assert!(usage.weekly.resets_at.is_some());
        assert!(usage.session.resets_at.is_some());
    }

    #[test]
    fn iso8601_offsets_are_converted_to_utc() {
        let utc = parse_iso8601(Some("2026-01-01T00:30:00Z"));
        assert_eq!(parse_iso8601(Some("2026-01-01T01:30:00+01:00")), utc);
        assert_eq!(parse_iso8601(Some("2025-12-31T19:30:00-05:00")), utc);
        assert!(parse_iso8601(Some("2026-13-01T00:30:00Z")).is_none());
        assert!(parse_iso8601(Some("2026-01-01T00:30:00+0é:00")).is_none());
    }

    fn kimi_usage(json: &str) -> Option<UsageData> {
        serde_json::from_str::<KimiCodeUsageResponse>(json)
            .ok()
            .and_then(kimi_code_usage_from_response)
    }

    fn kimi_test_home(name: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("codex-usage-kimi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn kimi_ratio_pools_parse_session_weekly_and_missing_windows() {
        let usage = kimi_usage(
            r#"{
                "usages": {
                    "limit_5h": {"used_ratio": 0.625, "reset_time": "2026-09-19T14:45:59.449979235Z"},
                    "limit_7d": {"used_ratio": "0.125", "reset_time": "2026-09-20T00:00:00Z"}
                }
            }"#,
        )
        .expect("ratio pools should be usable");
        assert_eq!(usage.session.percentage, 62.5);
        assert_eq!(usage.weekly.percentage, 12.5);
        assert!(usage.session.resets_at.is_some());
        assert!(usage.weekly.resets_at.is_some());

        let session_only = kimi_usage(
            r#"{"usages":{"limit_5h":{"used_ratio":0.4,"reset_time":"2026-09-19T14:45:59Z"}}}"#,
        )
        .expect("new plans can omit the weekly window");
        assert_eq!(session_only.session.percentage, 40.0);
        assert_eq!(session_only.weekly.percentage, 0.0);
        assert!(session_only.weekly.resets_at.is_none());
        assert_eq!(
            format_codex_line(
                &session_only.weekly,
                crate::localization::LanguageId::English.strings(),
                false,
                UsageWindowKind::Weekly,
            ),
            "--"
        );
    }

    #[test]
    fn kimi_zero_usage_without_reset_is_not_a_missing_window() {
        let usage = kimi_usage(r#"{"usages":{"limit_5h":{"used_ratio":0}}}"#).unwrap();
        assert!(usage.session.reported);
        assert!(!usage.weekly.reported);
        let strings = crate::localization::LanguageId::English.strings();
        assert_eq!(
            format_codex_line(&usage.session, strings, false, UsageWindowKind::Session),
            "0%"
        );
        assert_eq!(
            format_codex_line(&usage.session, strings, true, UsageWindowKind::Session),
            "100%"
        );
        assert_eq!(
            format_codex_line(&usage.weekly, strings, false, UsageWindowKind::Weekly),
            "--"
        );
    }

    #[test]
    fn kimi_legacy_counts_support_both_windows_and_remaining_fallback() {
        let usage = kimi_usage(
            r#"{
                "usage": {"limit": "100", "used": "19", "resetTime": "2026-09-19T16:45:59Z"},
                "limits": [{
                    "window": {"duration": 5, "timeUnit": "TIME_UNIT_HOUR"},
                    "detail": {"limit": 100, "remaining": 75, "resetTime": "2026-09-19T14:45:59Z"}
                }]
            }"#,
        )
        .expect("legacy counters should be usable");
        assert_eq!(usage.session.percentage, 25.0);
        assert_eq!(usage.weekly.percentage, 19.0);
        assert!(usage.session.resets_at.is_some());
        assert!(usage.weekly.resets_at.is_some());
    }

    #[test]
    fn kimi_matching_ratio_and_count_windows_use_the_more_exhausted_reading() {
        let usage = kimi_usage(
            r#"{
                "usage": {"limit": "100", "used": "19", "resetTime": "2026-09-19T16:45:59.449979Z"},
                "limits": [{
                    "window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
                    "detail": {"limit": "100", "used": "1", "resetTime": "2026-09-19T14:45:59.449979Z"}
                }],
                "usages": {
                    "limit_5h": {"used_ratio": 0, "reset_time": "2026-09-19T14:45:58Z"},
                    "limit_7d": {"used_ratio": 0.1, "reset_time": "2026-09-19T16:45:59Z"}
                }
            }"#,
        )
        .expect("mixed response should be usable");
        assert_eq!(usage.session.percentage, 1.0);
        assert_eq!(usage.weekly.percentage, 19.0);
    }

    #[test]
    fn kimi_invalid_or_unrelated_quotas_are_not_reported_as_unused() {
        for json in [
            "{}",
            r#"{"usages":{}}"#,
            r#"{"usages":{"limit_month_total":{"used_ratio":0.5}}}"#,
            r#"{"usages":{"limit_5h":{"used_ratio":-0.5}}}"#,
        ] {
            assert!(kimi_usage(json).is_none(), "unexpected quota for {json}");
        }
    }

    #[test]
    fn kimi_global_config_uses_the_scoped_oauth_credential() {
        let home = kimi_test_home("global");
        std::fs::create_dir_all(home.join("credentials")).unwrap();
        std::fs::write(
            home.join("config.toml"),
            r#"
[providers."managed:kimi-code"]
base_url = "https://api.kimi.ai/coding/v1"
api_key = ""
[providers."managed:kimi-code".oauth]
storage = "file"
key = "oauth/kimi-code-env-0e4f99c69cc27850"
oauthHost = "https://auth.kimi.ai"
"#,
        )
        .unwrap();
        assert_eq!(
            kimi_code_credential_name("https://auth.kimi.ai", "https://api.kimi.ai/coding/v1",),
            "kimi-code-env-0e4f99c69cc27850"
        );
        std::fs::write(
            home.join("credentials")
                .join("kimi-code-env-0e4f99c69cc27850.json"),
            r#"{"access_token":"global-token","refresh_token":"refresh","expires_at":2000}"#,
        )
        .unwrap();

        let credentials =
            read_kimi_code_credentials_from(&home, &KimiCodeEnvironment::default(), 1000.0)
                .expect("global OAuth credential should be accepted");
        assert_eq!(credentials.access_token, "global-token");
        assert_eq!(credentials.base_url, "https://api.kimi.ai/coding/v1");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn kimi_api_key_environment_takes_precedence_without_a_credential_file() {
        let home = kimi_test_home("api-key");
        std::fs::write(
            home.join("config.toml"),
            r#"
[providers."managed:kimi-code"]
base_url = "https://api.kimi.com/coding/v1"
api_key = "config-key"
"#,
        )
        .unwrap();
        let environment = KimiCodeEnvironment {
            api_key: Some("environment-key".into()),
            ..KimiCodeEnvironment::default()
        };
        let credentials = read_kimi_code_credentials_from(&home, &environment, 1000.0)
            .expect("explicit API key should not need OAuth files");
        assert_eq!(credentials.access_token, "environment-key");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn kimi_expired_oauth_token_requires_cli_renewal() {
        let home = kimi_test_home("expired");
        std::fs::create_dir_all(home.join("credentials")).unwrap();
        std::fs::write(
            home.join("credentials").join("kimi-code.json"),
            r#"{"access_token":"expired-token","refresh_token":"refresh","expires_at":1000}"#,
        )
        .unwrap();
        assert_eq!(
            read_kimi_code_credentials_from(&home, &KimiCodeEnvironment::default(), 1000.0).err(),
            Some(PollError::TokenExpired)
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn kimi_default_and_unsafe_credential_endpoints_are_validated() {
        let home = kimi_test_home("endpoints");
        let plan = kimi_code_auth_plan_from(&home, &KimiCodeEnvironment::default())
            .expect("default plan should be available");
        assert_eq!(plan.base_url, KIMI_CODE_DEFAULT_BASE_URL);
        match plan.source {
            KimiCodeTokenSource::OAuth(path) => {
                assert_eq!(path, home.join("credentials").join("kimi-code.json"));
            }
            KimiCodeTokenSource::ApiKey(_) => panic!("OAuth should be the default source"),
        }
        let insecure = KimiCodeEnvironment {
            base_url: Some("http://api.kimi.com/coding/v1".into()),
            ..KimiCodeEnvironment::default()
        };
        assert!(kimi_code_auth_plan_from(&home, &insecure).is_none());
        for url in [
            "http://api.kimi.com/coding/v1",
            "https://user:password@api.kimi.com/coding/v1",
            "https://api.kimi.com/coding/v1?token=value",
            "https://api.kimi.com/coding/v1#fragment",
            "https://api.kimi.com\\other/path",
            "https://api.kimi.com/with space",
        ] {
            assert!(
                normalize_kimi_code_base_url(url).is_none(),
                "accepted unsafe URL: {url}"
            );
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn kimi_endpoint_override_never_reuses_a_config_api_key_or_default_oauth_token() {
        let home = kimi_test_home("override");
        std::fs::write(
            home.join("config.toml"),
            r#"
[providers."managed:kimi-code"]
base_url = "https://api.kimi.com/coding/v1"
api_key = "config-key"
"#,
        )
        .unwrap();
        let override_env = KimiCodeEnvironment {
            base_url: Some("https://gateway.example.test/coding/v1".into()),
            ..KimiCodeEnvironment::default()
        };
        let plan = kimi_code_auth_plan_from(&home, &override_env).unwrap();
        match plan.source {
            KimiCodeTokenSource::OAuth(path) => {
                assert_ne!(path.file_name().unwrap(), "kimi-code.json")
            }
            KimiCodeTokenSource::ApiKey(_) => {
                panic!("must not forward a config key to another endpoint")
            }
        }
        let explicit_key = KimiCodeEnvironment {
            api_key: Some("explicit-gateway-key".into()),
            ..override_env
        };
        let credentials = read_kimi_code_credentials_from(&home, &explicit_key, 1000.0).unwrap();
        assert_eq!(credentials.access_token, "explicit-gateway-key");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn kimi_invalid_config_and_unknown_expiry_cannot_reuse_old_credentials() {
        let home = kimi_test_home("invalid-config");
        std::fs::create_dir_all(home.join("credentials")).unwrap();
        std::fs::write(
            home.join("credentials/kimi-code.json"),
            r#"{"access_token":"old-token","expires_at":2000}"#,
        )
        .unwrap();
        for config in [
            "invalid TOML [",
            "[providers.\"managed:kimi-code\".oauth]\nkey = \"oauth/../other\"",
            "[providers.\"managed:kimi-code\".oauth]\nstorage = \"keyring\"",
        ] {
            std::fs::write(home.join("config.toml"), config).unwrap();
            assert!(kimi_code_auth_plan_from(&home, &KimiCodeEnvironment::default()).is_none());
        }
        std::fs::remove_file(home.join("config.toml")).unwrap();
        for token in [
            r#"{"access_token":"token"}"#,
            r#"{"access_token":"token","expires_at":"invalid"}"#,
        ] {
            std::fs::write(home.join("credentials/kimi-code.json"), token).unwrap();
            assert_eq!(
                read_kimi_code_credentials_from(&home, &KimiCodeEnvironment::default(), 1000.0)
                    .err(),
                Some(PollError::TokenExpired)
            );
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn kimi_usage_http_request_is_read_only_and_does_not_follow_redirects() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .timeout(Duration::from_secs(5))
            .redirects(0)
            .build();
        for (status, headers, body, expected) in [
            (
                "200 OK",
                "",
                r#"{"usages":{"limit_5h":{"used_ratio":0.25}}}"#,
                None,
            ),
            ("401 Unauthorized", "", "{}", Some(PollError::AuthRequired)),
            (
                "429 Too Many Requests",
                "Retry-After: 120\r\n",
                "{}",
                Some(PollError::RateLimited(Some(120_000))),
            ),
            ("503 Unavailable", "", "{}", Some(PollError::ServerError)),
            (
                "302 Found",
                "Location: http://127.0.0.1:1/never-forward-token\r\n",
                "{}",
                Some(PollError::RequestFailed),
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut buffer = [0u8; 1024];
                    let length = socket.read(&mut buffer).unwrap();
                    assert_ne!(length, 0);
                    request.extend_from_slice(&buffer[..length]);
                }
                write!(socket, "HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                String::from_utf8(request).unwrap()
            });
            let result = fetch_kimi_code_usage_with_agent(
                "fixture-token",
                &format!("http://{address}/coding/v1"),
                &agent,
            );
            let request = server.join().unwrap().to_ascii_lowercase();
            assert!(request.starts_with("get /coding/v1/usages http/1.1\r\n"));
            assert!(request.contains("authorization: bearer fixture-token\r\n"));
            assert_eq!(result.as_ref().err().copied(), expected, "status: {status}");
            if let Ok(usage) = result {
                assert_eq!(usage.session.percentage, 25.0);
            }
        }
    }
}
