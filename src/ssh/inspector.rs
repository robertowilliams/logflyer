use std::io::Read;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use ssh2::Session;
use tracing::warn;

use crate::config::{DiscoveryConfig, SamplingConfig};
use crate::error::AppError;
use crate::models::{AuthMethod, ProcessingStatus, SampleDraft, ValidatedTarget};
use crate::sampling::{build_sampler, Sampler};
use crate::utils::shell_quote;

#[derive(Clone)]
pub struct SshLogInspector {
    sampling: SamplingConfig,
    discovery: DiscoveryConfig,
    timeout: Duration,
}

pub trait RemoteCommandExecutor {
    fn run_stdout(&self, command: &str) -> Result<String, AppError>;
}

impl SshLogInspector {
    pub fn new(sampling: SamplingConfig, discovery: DiscoveryConfig, timeout: Duration) -> Self {
        Self {
            sampling,
            discovery,
            timeout,
        }
    }

    pub async fn collect_samples(
        &self,
        target: ValidatedTarget,
    ) -> Result<Vec<SampleDraft>, AppError> {
        let sampling = self.sampling.clone();
        let discovery = self.discovery.clone();
        let timeout = self.timeout;

        tokio::task::spawn_blocking(move || inspect_target(target, sampling, discovery, timeout))
            .await
            .map_err(|error| AppError::Join(error.to_string()))?
    }
}

fn inspect_target(
    target: ValidatedTarget,
    sampling: SamplingConfig,
    discovery: DiscoveryConfig,
    timeout: Duration,
) -> Result<Vec<SampleDraft>, AppError> {
    // SSH work runs on a blocking thread because the ssh2 crate is synchronous. The
    // async runtime stays free to schedule MongoDB and orchestration tasks.
    let session = match connect_session(&target, timeout) {
        Ok(session) => session,
        Err(error) => {
            return Ok(vec![SampleDraft {
                target_id: target.target_id.clone(),
                source_file: "__target__".to_string(),
                sample_content: String::new(),
                host: target.host.clone(),
                path: "__target__".to_string(),
                sampling_mode: sampling.mode,
                line_count: None,
                file_size_bytes: None,
                processing_status: ProcessingStatus::Error,
                error_details: Some(error.to_string()),
            }]);
        }
    };

    let executor = SessionExecutor { session: &session };
    let sampler = build_sampler(sampling.mode);
    let mut results = Vec::new();

    // Per-target overrides take precedence over global config values.
    let effective_line_count = target.sample_line_count.unwrap_or(sampling.line_count);
    let effective_max_files = target.max_files.unwrap_or(discovery.max_files_per_target);

    for directory in &target.log_paths {
        if !directory_exists(&executor, directory)? {
            results.push(SampleDraft {
                target_id: target.target_id.clone(),
                source_file: directory.clone(),
                sample_content: String::new(),
                host: target.host.clone(),
                path: directory.clone(),
                sampling_mode: sampling.mode,
                line_count: None,
                file_size_bytes: None,
                processing_status: ProcessingStatus::MissingDirectory,
                error_details: Some("remote directory does not exist".to_string()),
            });
            continue;
        }

        let files = match find_files(&executor, directory, &discovery, effective_max_files) {
            Ok(files) => files,
            Err(error) => {
                results.push(SampleDraft {
                    target_id: target.target_id.clone(),
                    source_file: directory.clone(),
                    sample_content: String::new(),
                    host: target.host.clone(),
                    path: directory.clone(),
                    sampling_mode: sampling.mode,
                    line_count: None,
                    file_size_bytes: None,
                    processing_status: ProcessingStatus::Error,
                    error_details: Some(error.to_string()),
                });
                continue;
            }
        };

        if files.is_empty() {
            results.push(SampleDraft {
                target_id: target.target_id.clone(),
                source_file: directory.clone(),
                sample_content: String::new(),
                host: target.host.clone(),
                path: directory.clone(),
                sampling_mode: sampling.mode,
                line_count: None,
                file_size_bytes: None,
                processing_status: ProcessingStatus::NoFilesFound,
                error_details: Some("no matching files found".to_string()),
            });
            continue;
        }

        for file in files {
            results.push(sample_file(
                &executor,
                &*sampler,
                &target,
                &file,
                effective_line_count,
                sampling.mode,
            ));
        }
    }

    Ok(results)
}

fn sample_file(
    executor: &dyn RemoteCommandExecutor,
    sampler: &dyn Sampler,
    target: &ValidatedTarget,
    file: &str,
    line_count: usize,
    mode: crate::sampling::SamplingMode,
) -> SampleDraft {
    // Metadata is collected separately from the sample body so failures still carry
    // useful context such as file size or line count when available.
    let file_size_bytes = query_u64(executor, &format!("stat -c %s {}", shell_quote(file)));
    let remote_line_count = query_u64(executor, &format!("wc -l < {}", shell_quote(file)));

    match sampler.sample(executor, file, line_count) {
        Ok(sample_content) => {
            let processing_status =
                if file_size_bytes == Some(0) || sample_content.trim().is_empty() {
                    ProcessingStatus::Empty
                } else {
                    ProcessingStatus::Stored
                };

            SampleDraft {
                target_id: target.target_id.clone(),
                source_file: file.to_string(),
                sample_content,
                host: target.host.clone(),
                path: file.to_string(),
                sampling_mode: mode,
                line_count: remote_line_count,
                file_size_bytes,
                processing_status,
                error_details: None,
            }
        }
        Err(error) => SampleDraft {
            target_id: target.target_id.clone(),
            source_file: file.to_string(),
            sample_content: String::new(),
            host: target.host.clone(),
            path: file.to_string(),
            sampling_mode: mode,
            line_count: remote_line_count,
            file_size_bytes,
            processing_status: ProcessingStatus::Error,
            error_details: Some(error.to_string()),
        },
    }
}

fn connect_session(target: &ValidatedTarget, timeout: Duration) -> Result<Session, AppError> {
    let socket = resolve_socket(&target.host, target.port)?;
    // Timeouts are applied to the TCP stream before the SSH handshake so network stalls
    // fail fast instead of pinning a worker thread indefinitely.
    let tcp = TcpStream::connect_timeout(&socket, timeout).map_err(|error| {
        AppError::Ssh(format!(
            "failed to connect to {}:{}: {error}",
            target.host, target.port
        ))
    })?;
    tcp.set_read_timeout(Some(timeout)).ok();
    tcp.set_write_timeout(Some(timeout)).ok();

    let mut session = Session::new()
        .map_err(|error| AppError::Ssh(format!("failed to create SSH session: {error}")))?;
    session.set_tcp_stream(tcp);
    session.handshake().map_err(|error| {
        AppError::Ssh(format!("SSH handshake failed for {}: {error}", target.host))
    })?;

    match &target.auth {
        AuthMethod::Password { password } => session
            .userauth_password(&target.username, password)
            .map_err(|error| AppError::Ssh(format!("password authentication failed: {error}")))?,
        AuthMethod::PrivateKeyPath {
            private_key_path,
            passphrase,
        } => session
            .userauth_pubkey_file(
                &target.username,
                None,
                PathBuf::from(private_key_path).as_path(),
                passphrase.as_deref(),
            )
            .map_err(|error| {
                AppError::Ssh(format!("private key authentication failed: {error}"))
            })?,
        AuthMethod::PrivateKeyInline {
            private_key,
            passphrase,
        } => session
            .userauth_pubkey_memory(&target.username, None, private_key, passphrase.as_deref())
            .map_err(|error| {
                AppError::Ssh(format!("inline private key authentication failed: {error}"))
            })?,
        AuthMethod::None => {
            // No credentials configured — try the SSH agent (picks up any keys
            // loaded in the running agent). If the agent is unavailable or the
            // server rejects it, the authenticated() check below will surface a
            // clear error instead of silently failing.
            let _ = session.userauth_agent(&target.username);
        }
    }

    if !session.authenticated() {
        return Err(AppError::Ssh(format!(
            "SSH authentication did not succeed for target {}",
            target.target_id
        )));
    }

    Ok(session)
}

fn resolve_socket(host: &str, port: u16) -> Result<SocketAddr, AppError> {
    (host, port)
        .to_socket_addrs()
        .map_err(|error| AppError::Ssh(format!("failed to resolve {host}:{port}: {error}")))?
        .next()
        .ok_or_else(|| AppError::Ssh(format!("no socket addresses resolved for {host}:{port}")))
}

fn directory_exists(
    executor: &dyn RemoteCommandExecutor,
    directory: &str,
) -> Result<bool, AppError> {
    let command = format!(
        "if test -d {}; then printf true; else printf false; fi",
        shell_quote(directory)
    );
    Ok(executor.run_stdout(&command)?.trim() == "true")
}

fn find_files(
    executor: &dyn RemoteCommandExecutor,
    directory: &str,
    discovery: &DiscoveryConfig,
    max_files: usize,
) -> Result<Vec<String>, AppError> {
    // Remote discovery is intentionally delegated to `find` because the target systems
    // are Linux hosts and this keeps network transfer limited to the final sample set.
    let filter = if discovery.find_patterns.is_empty() {
        String::new()
    } else {
        let predicates = discovery
            .find_patterns
            .iter()
            .map(|pattern| format!("-name {}", shell_quote(pattern)))
            .collect::<Vec<_>>()
            .join(" -o ");
        format!("\\( {predicates} \\)")
    };

    let command = if filter.is_empty() {
        format!(
            "find {} -maxdepth {} -type f | head -n {}",
            shell_quote(directory),
            discovery.max_depth,
            max_files,
        )
    } else {
        format!(
            "find {} -maxdepth {} -type f {} | head -n {}",
            shell_quote(directory),
            discovery.max_depth,
            filter,
            max_files,
        )
    };

    let output = executor.run_stdout(&command)?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

fn query_u64(executor: &dyn RemoteCommandExecutor, command: &str) -> Option<u64> {
    match executor.run_stdout(command) {
        Ok(output) => output.trim().parse::<u64>().ok(),
        Err(error) => {
            warn!(error = %error, command = %command, "failed to fetch remote metadata");
            None
        }
    }
}

struct SessionExecutor<'a> {
    session: &'a Session,
}

impl RemoteCommandExecutor for SessionExecutor<'_> {
    fn run_stdout(&self, command: &str) -> Result<String, AppError> {
        let mut channel = self
            .session
            .channel_session()
            .map_err(|error| AppError::Ssh(format!("failed to open SSH channel: {error}")))?;

        // Every command is executed in its own channel so partial failures do not poison
        // the whole session state for later file operations.
        channel.exec(command).map_err(|error| {
            AppError::Ssh(format!(
                "failed to execute remote command `{command}`: {error}"
            ))
        })?;

        let mut stdout = String::new();
        channel
            .read_to_string(&mut stdout)
            .map_err(|error| AppError::Ssh(format!("failed reading command stdout: {error}")))?;

        let mut stderr = String::new();
        channel
            .stderr()
            .read_to_string(&mut stderr)
            .map_err(|error| AppError::Ssh(format!("failed reading command stderr: {error}")))?;

        channel
            .wait_close()
            .map_err(|error| AppError::Ssh(format!("failed to close SSH channel: {error}")))?;

        let exit_code = channel.exit_status().map_err(|error| {
            AppError::Ssh(format!("failed to inspect remote exit status: {error}"))
        })?;

        if exit_code != 0 {
            return Err(AppError::Ssh(format!(
                "remote command failed with exit code {exit_code}: `{command}` stderr=`{}`",
                stderr.trim()
            )));
        }

        Ok(stdout)
    }
}
