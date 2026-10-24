# Troubleshooting Codex Usage

## Taskbar error labels

Codex Usage keeps authentication failures separate from transient service failures:

| Simplified Chinese | Other languages | Meaning | Recommended action |
|---|---|---|---|
| `!` | `!` | Enabled service credentials are missing or expired | Sign in with the relevant CLI/app, then refresh Codex Usage. Claude Desktop login does not count as Claude Code CLI login. |
| `网络` | `NET` | Network or TLS connection failed | Check connectivity, VPN, proxy, and firewall settings |
| `限流` | `429` | Provider rate limit | Wait for the provider retry window; Codex Usage retries with backoff |
| `服务` | `5XX` | Provider service failure | Wait and retry; check provider status if it persists |
| `错误` | `ERR` | Invalid or unsupported response | Enable diagnostics and inspect the log |

Authentication failures pause provider polling until the credential source changes, preventing repeated login notifications. Transient failures use exponential backoff up to the configured refresh interval.

## Kimi Code sign-in

Kimi Code's second row is **monthly total usage**, not a weekly quota. It reads `usages.limit_month_total`, matching the console (for example, `used_ratio: 0.0572` means 5.72% used). `limit_month_code`, `limit_7d`, and legacy weekly counters are deliberately not used as monthly substitutes. If the API omits monthly total, `--` means this API did not report that period; it does not mean the subscription has no monthly quota.

Kimi Code monitors membership quotas, not a Kimi Open Platform pay-as-you-go balance. The easiest setup is **Configure Kimi Code API key…** in the right-click menu. Choose the region that issued the key and save. Kimi Code is enabled and refreshed immediately. The key is stored in Windows Credential Manager under `CodexUsage:kimi-code`, not in `settings.json`, logs, or CLI credential files.

The saved key is authoritative: invalid saved keys do not silently fall back to a different account. Reopen the dialog to replace it, or use **Remove key** to return to environment/CLI authentication. You can also run `kimi` in a terminal and use `/login`, then reopen **Monitored services** and enable **Kimi Code**.

The monitor reads `$KIMI_CODE_HOME/config.toml` or `~/.kimi-code/config.toml` and the OAuth credential selected by that configuration. The default credential is `~/.kimi-code/credentials/kimi-code.json`; international and custom environments use their scoped credential filename. A Kimi Code API key from the managed provider configuration or `KIMI_CODE_API_KEY` is also accepted.

OAuth tokens are used read-only. If a token expires, let the official CLI renew it, then choose **Refresh** or wait for the credential file change to be detected. Codex Usage never rewrites the Kimi configuration, access token, or refresh token.

For CLI authentication, an unknown token expiry, invalid TOML configuration, unsupported credential storage, or mismatched OAuth reference requires signing in again or correcting the configuration. The monitor will not fall back to another saved account. To use a different `KIMI_CODE_BASE_URL`, remove any in-app key and supply an explicit `KIMI_CODE_API_KEY` or CLI credentials scoped to that endpoint; keys from unrelated configuration are not forwarded.

## Diagnostic log

Run:

```powershell
codex-usage.exe --diagnose
```

The log is written to `%TEMP%\codex-usage.log`. It includes:

- application version and executable path
- install channel (direct or portable)
- provider failure category and retry delay
- window creation, taskbar placement, and relaunch events

The log does not include access tokens, refresh tokens, credential file contents, or API response bodies.

## Update failures

Direct installations and portable copies download only the exact `codex-usage.exe` asset and verify it against `codex-usage.exe.sha256` from the same GitHub Release. The updater keeps the previous EXE until the downloaded version has been installed and restarted successfully. If restart fails, the old EXE is restored.

## Reset local position without deleting settings

Right-click the taskbar component and choose **Settings > Reset Position**. Settings are stored at `%APPDATA%\CodexUsage\settings.json`.

## Widget disappears after switching displays

Switching **Settings > Display** must preserve the widget's layered rendering mode. Older builds could remove this mode while moving an already embedded widget between taskbars, leaving the app running but its content invisible. Restarting restores the display temporarily; install the fixed build to keep it visible across repeated monitor changes. Do not delete settings or saved API keys. Rendering failures are recorded in the diagnostic log when `--diagnose` is enabled.

## Reinstall while preserving settings

Normal uninstall keeps the settings file. Reinstalling restores the saved language, refresh interval, provider selection, widget visibility, and taskbar position. Use `uninstall.ps1 -RemoveSettings` only when a full reset is intended.
