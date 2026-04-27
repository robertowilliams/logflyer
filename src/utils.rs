use sha2::{Digest, Sha256};

use crate::models::SampleDraft;

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub fn compute_sample_hash(sample: &SampleDraft) -> String {
    // The hash is deterministic across polling runs, which makes repeated sampling
    // idempotent as long as the sampled content and processing outcome have not changed.
    let mut hasher = Sha256::new();
    hasher.update(sample.target_id.as_bytes());
    hasher.update([0]);
    hasher.update(sample.source_file.as_bytes());
    hasher.update([0]);
    hasher.update(sample.sampling_mode.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(sample.sample_content.as_bytes());
    hasher.update([0]);
    hasher.update(sample.processing_status.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(
        sample
            .error_details
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );

    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use crate::models::{ProcessingStatus, SampleDraft};
    use crate::sampling::SamplingMode;

    use super::compute_sample_hash;

    #[test]
    fn sample_hash_is_deterministic() {
        let sample = SampleDraft {
            target_id: "target-1".to_string(),
            source_file: "/var/log/app.log".to_string(),
            sample_content: "line-1\nline-2".to_string(),
            host: "example.org".to_string(),
            path: "/var/log/app.log".to_string(),
            sampling_mode: SamplingMode::Both,
            line_count: Some(2),
            file_size_bytes: Some(20),
            processing_status: ProcessingStatus::Stored,
            error_details: None,
        };

        assert_eq!(compute_sample_hash(&sample), compute_sample_hash(&sample));
    }
}
