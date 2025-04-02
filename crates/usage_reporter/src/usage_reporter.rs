use anyhow::Result;

pub struct Usage {
    pub completion_tokens: u64,
    pub prompt_tokens: u64,
    pub total_tokens: u64,
}

pub struct UsageReport {
    pub model: String,
    pub usage: Usage,
}

pub trait UsageReporter: Send + Sync {
    fn report(&self, usage: UsageReport) -> Result<()>;
}

pub struct NoopUsageReporter;

impl UsageReporter for NoopUsageReporter {
    fn report(&self, _usage: UsageReport) -> Result<()> {
        Ok(())
    }
}
