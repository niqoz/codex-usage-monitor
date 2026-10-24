use std::time::SystemTime;

#[derive(Clone, Debug, Default)]
pub struct UsageSection {
    pub percentage: f64,
    pub resets_at: Option<SystemTime>,
    /// Preserve a reported zero even when the provider omits its reset time.
    pub reported: bool,
}

#[derive(Clone, Debug, Default)]
pub struct UsageData {
    pub session: UsageSection,
    /// Long-period row: weekly for other providers, monthly total for Kimi Code.
    pub weekly: UsageSection,
}

#[derive(Clone, Debug, Default)]
pub struct AppUsageData {
    pub claude_code: Option<UsageData>,
    pub codex: Option<UsageData>,
    pub antigravity: Option<UsageData>,
    pub kimi_code: Option<UsageData>,
}
