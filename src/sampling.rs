use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::ssh::RemoteCommandExecutor;
use crate::utils::shell_quote;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SamplingMode {
    First,
    Last,
    Both,
}

impl SamplingMode {
    pub fn from_env(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "first" => Some(Self::First),
            "last" => Some(Self::Last),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SamplingMode::First => "first",
            SamplingMode::Last => "last",
            SamplingMode::Both => "both",
        }
    }
}

pub trait Sampler: Send + Sync {
    fn mode(&self) -> SamplingMode;
    // Sampling stays behind a trait so new strategies can plug in without changing the
    // SSH transport or the persistence code.
    fn sample(
        &self,
        executor: &dyn RemoteCommandExecutor,
        path: &str,
        line_count: usize,
    ) -> Result<String, AppError>;
}

pub fn build_sampler(mode: SamplingMode) -> Box<dyn Sampler> {
    match mode {
        SamplingMode::First => Box::new(HeadSampler),
        SamplingMode::Last => Box::new(TailSampler),
        SamplingMode::Both => Box::new(HeadTailSampler),
    }
}

struct HeadSampler;
struct TailSampler;
struct HeadTailSampler;

impl Sampler for HeadSampler {
    fn mode(&self) -> SamplingMode {
        SamplingMode::First
    }

    fn sample(
        &self,
        executor: &dyn RemoteCommandExecutor,
        path: &str,
        line_count: usize,
    ) -> Result<String, AppError> {
        let command = format!("head -n {line_count} {}", shell_quote(path));
        executor.run_stdout(&command)
    }
}

impl Sampler for TailSampler {
    fn mode(&self) -> SamplingMode {
        SamplingMode::Last
    }

    fn sample(
        &self,
        executor: &dyn RemoteCommandExecutor,
        path: &str,
        line_count: usize,
    ) -> Result<String, AppError> {
        let command = format!("tail -n {line_count} {}", shell_quote(path));
        executor.run_stdout(&command)
    }
}

impl Sampler for HeadTailSampler {
    fn mode(&self) -> SamplingMode {
        SamplingMode::Both
    }

    fn sample(
        &self,
        executor: &dyn RemoteCommandExecutor,
        path: &str,
        line_count: usize,
    ) -> Result<String, AppError> {
        let quoted_path = shell_quote(path);
        // Markers make the persisted payload self-describing when both ends of a file
        // are sampled into a single document.
        let command = format!(
            "printf '%s\n' '--- BEGIN HEAD ---'; \
             head -n {line_count} {quoted_path}; \
             printf '%s\n' '--- END HEAD ---' ''; \
             printf '%s\n' '--- BEGIN TAIL ---'; \
             tail -n {line_count} {quoted_path}; \
             printf '%s\n' '--- END TAIL ---'"
        );

        executor.run_stdout(&command)
    }
}

#[cfg(test)]
mod tests {
    use super::SamplingMode;

    #[test]
    fn parses_sampling_mode() {
        assert_eq!(SamplingMode::from_env("first"), Some(SamplingMode::First));
        assert_eq!(SamplingMode::from_env("last"), Some(SamplingMode::Last));
        assert_eq!(SamplingMode::from_env("both"), Some(SamplingMode::Both));
        assert_eq!(SamplingMode::from_env("unknown"), None);
    }
}
