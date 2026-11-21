//! Synchronous OpenShell 0.1.7 adapter for an operator-managed gateway.

mod backend;

use crate::agent::{self, AgentPreparationError, PreparedAgent};
use crate::config::{AgentRunnerConfig, Driver, ExecutionConfig, valid_environment_entry};
use crate::image::StagedImage;
use crate::review::RoundRecord;
use crate::sandbox_layout::{self, OPENSHELL_SANDBOX_NAME_LIMIT};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OpenShellError {
    #[error(transparent)]
    AgentImage(#[from] AgentPreparationError),
    #[error("local VM review execution is unsupported on this platform")]
    PlatformUnsupported,
    #[error("podman review execution is unsupported; configure the vm or kubernetes driver")]
    PodmanUnsupported,
    #[error("OpenShell CLI must be version 0.1.7")]
    IncompatibleCli,
    #[error("running OpenShell {step}: {source}")]
    Launch {
        step: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("OpenShell {0} failed; check gateway authentication, connectivity, and operator logs")]
    Failed(&'static str),
    #[error("OpenShell sandbox name exceeds the {OPENSHELL_SANDBOX_NAME_LIMIT}-character limit")]
    SandboxNameTooLong,
    #[error("review image does not match the configured execution driver")]
    ImageDriverMismatch,
    #[error("gateway does not report {0} driver support")]
    DriverUnavailable(&'static str),
    #[error("required provider {0} is unavailable")]
    ProviderUnavailable(String),
    #[error("GitHub provider profile must enforce read-only REST access to api.github.com:443")]
    GithubProfileNotReadOnly,
    #[error("model provider profile is not inference-capable")]
    ModelProviderNotInference,
    #[error("provider profile {0} is unavailable")]
    ProfileUnavailable(String),
    #[error("image and agent environments both define {0}")]
    EnvironmentCollision(String),
    #[error("combined sandbox environment exceeds 64 KiB")]
    EnvironmentTooLarge,
    #[error("policy file {0} must be a regular file")]
    PolicyNotRegular(PathBuf),
    #[error("policy file checksum differs from configuration")]
    PolicyChanged,
    #[error("reading policy file {path}: {source}")]
    PolicyRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("storing private policy file {path}: {source}")]
    PolicyStore {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid gateway response for {0}")]
    InvalidResponse(&'static str),
}

pub struct OpenShell<'a> {
    config: &'a ExecutionConfig,
}

/// Private host directories mounted or uploaded into one review sandbox.
pub struct SandboxMounts<'a> {
    checkout: &'a Path,
    instructions: &'a Path,
    output: &'a Path,
}

impl<'a> SandboxMounts<'a> {
    pub fn new(checkout: &'a Path, instructions: &'a Path, output: &'a Path) -> Self {
        Self {
            checkout,
            instructions,
            output,
        }
    }
}

impl<'a> OpenShell<'a> {
    pub fn new(config: &'a ExecutionConfig) -> Self {
        Self { config }
    }

    pub fn preflight_gateway(&self) -> Result<(), OpenShellError> {
        backend::for_driver(self.config.driver())?.check_platform()?;
        let version = self.command(&["--version"], "version")?;
        if !version.status.success()
            || std::str::from_utf8(&version.stdout).ok().map(str::trim) != Some("openshell 0.1.7")
        {
            return Err(OpenShellError::IncompatibleCli);
        }
        self.require_success(&["whoami", "--output", "json"], "whoami")?;
        self.require_success(&["status", "--output", "json"], "status")?;
        let info =
            self.require_success(&["gateway", "info", "--output", "json"], "gateway info")?;
        let data: Value = serde_json::from_slice(&info.stdout)
            .map_err(|_| OpenShellError::InvalidResponse("gateway info"))?;
        if !reports_driver(&data, self.config.driver()) {
            return Err(OpenShellError::DriverUnavailable(
                self.config.driver().as_str(),
            ));
        }
        let github = self.provider_profile(self.config.github_provider())?;
        if !github_profile_is_read_only(&github) {
            return Err(OpenShellError::GithubProfileNotReadOnly);
        }
        Ok(())
    }

    pub fn preflight_runner(&self, runner: &AgentRunnerConfig) -> Result<(), OpenShellError> {
        let model = self.provider_profile(runner.model_provider())?;
        if model.get("inference_capable") != Some(&Value::Bool(true)) {
            return Err(OpenShellError::ModelProviderNotInference);
        }
        self.verify_policy(runner)
    }

    fn provider_profile(&self, provider: &str) -> Result<Value, OpenShellError> {
        let details = self.command(&["provider", "get", provider], "provider get")?;
        if !details.status.success() {
            return Err(OpenShellError::ProviderUnavailable(provider.to_owned()));
        }
        let profile_type = provider_type(&details.stdout)
            .ok_or(OpenShellError::InvalidResponse("provider get"))?;
        let local = self.command(
            &["profile", "describe", "--output", "json", &profile_type],
            "profile describe",
        )?;
        let description = if local.status.success() {
            local
        } else {
            let global = self.command(
                &[
                    "profile",
                    "describe",
                    "--global",
                    "--output",
                    "json",
                    &profile_type,
                ],
                "profile describe",
            )?;
            if !global.status.success() {
                return Err(OpenShellError::ProfileUnavailable(profile_type));
            }
            global
        };
        serde_json::from_slice(&description.stdout)
            .map_err(|_| OpenShellError::InvalidResponse("profile describe"))
    }

    pub fn verify_policy(&self, runner: &AgentRunnerConfig) -> Result<(), OpenShellError> {
        let path = runner.policy();
        let metadata = fs::symlink_metadata(path).map_err(|source| OpenShellError::PolicyRead {
            path: path.to_owned(),
            source,
        })?;
        if !metadata.file_type().is_file() {
            return Err(OpenShellError::PolicyNotRegular(path.to_owned()));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file: File = options
            .open(path)
            .map_err(|source| OpenShellError::PolicyRead {
                path: path.to_owned(),
                source,
            })?;
        if !file
            .metadata()
            .map_err(|source| OpenShellError::PolicyRead {
                path: path.to_owned(),
                source,
            })?
            .file_type()
            .is_file()
        {
            return Err(OpenShellError::PolicyNotRegular(path.to_owned()));
        }
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 65536];
        loop {
            let size = file
                .read(&mut buffer)
                .map_err(|source| OpenShellError::PolicyRead {
                    path: path.to_owned(),
                    source,
                })?;
            if size == 0 {
                break;
            }
            digest.update(&buffer[..size]);
        }
        if format!("{:x}", digest.finalize()) != runner.policy_sha256().as_str() {
            return Err(OpenShellError::PolicyChanged);
        }
        Ok(())
    }

    /// Keep the exact verified policy bytes private for the sandbox creation call.
    pub fn stage_policy(
        &self,
        runner: &AgentRunnerConfig,
        destination: &Path,
    ) -> Result<(), OpenShellError> {
        self.verify_policy(runner)?;
        let mut source_options = OpenOptions::new();
        source_options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            source_options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            source_options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut source =
            source_options
                .open(runner.policy())
                .map_err(|source| OpenShellError::PolicyRead {
                    path: runner.policy().to_owned(),
                    source,
                })?;
        if !source
            .metadata()
            .map_err(|source| OpenShellError::PolicyRead {
                path: runner.policy().to_owned(),
                source,
            })?
            .file_type()
            .is_file()
        {
            return Err(OpenShellError::PolicyNotRegular(runner.policy().to_owned()));
        }
        let mut target_options = OpenOptions::new();
        target_options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            target_options.mode(0o600);
        }
        let mut target =
            target_options
                .open(destination)
                .map_err(|source| OpenShellError::PolicyStore {
                    path: destination.to_owned(),
                    source,
                })?;
        std::io::copy(&mut source, &mut target).map_err(|source| OpenShellError::PolicyStore {
            path: destination.to_owned(),
            source,
        })?;
        target
            .flush()
            .map_err(|source| OpenShellError::PolicyStore {
                path: destination.to_owned(),
                source,
            })?;
        target
            .sync_all()
            .map_err(|source| OpenShellError::PolicyStore {
                path: destination.to_owned(),
                source,
            })?;
        let staged_hash = {
            let mut file =
                File::open(destination).map_err(|source| OpenShellError::PolicyRead {
                    path: destination.to_owned(),
                    source,
                })?;
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 65536];
            loop {
                let size = file
                    .read(&mut buffer)
                    .map_err(|source| OpenShellError::PolicyRead {
                        path: destination.to_owned(),
                        source,
                    })?;
                if size == 0 {
                    break;
                }
                digest.update(&buffer[..size]);
            }
            format!("{:x}", digest.finalize())
        };
        if staged_hash != runner.policy_sha256().as_str() {
            return Err(OpenShellError::PolicyChanged);
        }
        Ok(())
    }

    pub fn create(
        &self,
        round: &RoundRecord,
        runner: &AgentRunnerConfig,
        image: &StagedImage,
        policy: &Path,
        mounts: &SandboxMounts<'_>,
    ) -> Result<(), OpenShellError> {
        let args = self.create_args(round, runner, image, policy, mounts)?;
        let result = self.command_os(&args, "sandbox create")?;
        if !result.status.success() {
            return Err(OpenShellError::Failed("sandbox create"));
        }
        backend::for_driver(self.config.driver())?.prepare_inputs(self, round, mounts)
    }

    pub fn create_args(
        &self,
        round: &RoundRecord,
        runner: &AgentRunnerConfig,
        image: &StagedImage,
        policy: &Path,
        mounts: &SandboxMounts<'_>,
    ) -> Result<Vec<OsString>, OpenShellError> {
        let backend = backend::for_driver(self.config.driver())?;
        backend.check_platform()?;
        if image.driver() != self.config.driver() {
            return Err(OpenShellError::ImageDriverMismatch);
        }
        if round.sandbox().len() > OPENSHELL_SANDBOX_NAME_LIMIT {
            return Err(OpenShellError::SandboxNameTooLong);
        }
        let mut args: Vec<OsString> = vec![
            "sandbox".into(),
            "create".into(),
            "--name".into(),
            round.sandbox().into(),
            "--from".into(),
            image.source().to_owned(),
            "--cpu".into(),
            self.config.cpu().into(),
            "--memory".into(),
            self.config.memory().into(),
            "--provider".into(),
            runner.model_provider().into(),
            "--provider".into(),
            self.config.github_provider().into(),
            "--policy".into(),
            policy.as_os_str().to_owned(),
            "--no-auto-providers".into(),
            "--no-tty".into(),
            "--detach".into(),
            "--restart-policy".into(),
            "never".into(),
        ];
        args.extend(backend.create_args(mounts)?);
        let mut environment = image.environment().clone();
        for (key, value) in runner.environment() {
            if environment.insert(key.clone(), value.clone()).is_some() {
                return Err(OpenShellError::EnvironmentCollision(key.clone()));
            }
        }
        let adapter = agent::adapter(runner.kind());
        if image.driver() == Driver::Vm {
            // VM metadata is available before creation; OCI defaults are inspected
            // inside the running Kubernetes sandbox before reviewer dispatch.
            let _prepared = adapter.prepare(image.environment(), runner)?;
        }
        for &(key, value) in adapter.environment() {
            if environment
                .insert(key.to_owned(), value.to_owned())
                .is_some()
            {
                return Err(OpenShellError::EnvironmentCollision(key.to_owned()));
            }
        }
        if environment
            .iter()
            .map(|(key, value)| key.len() + value.len() + 1)
            .sum::<usize>()
            > 64 * 1024
        {
            return Err(OpenShellError::EnvironmentTooLarge);
        }
        for (key, value) in &environment {
            args.push("--env".into());
            args.push(format!("{key}={value}").into());
        }
        args.push("--".into());
        args.extend(runner.hold_command().iter().map(OsString::from));
        Ok(args)
    }

    /// Resolve reviewer settings from verified VM metadata or the live OCI environment.
    pub fn prepare_agent(
        &self,
        round: &RoundRecord,
        runner: &AgentRunnerConfig,
        image: &StagedImage,
    ) -> Result<PreparedAgent, OpenShellError> {
        if image.driver() != self.config.driver() || round.driver() != image.driver().as_str() {
            return Err(OpenShellError::ImageDriverMismatch);
        }
        let adapter = agent::adapter(runner.kind());
        let environment = match image.driver() {
            Driver::Vm => image.environment().clone(),
            Driver::Kubernetes => {
                self.read_agent_environment(round, adapter.image_environment_keys())?
            }
            Driver::Podman => return Err(OpenShellError::PodmanUnsupported),
        };
        Ok(adapter.prepare(&environment, runner)?)
    }

    fn read_agent_environment(
        &self,
        round: &RoundRecord,
        keys: &'static [&'static str],
    ) -> Result<BTreeMap<String, String>, OpenShellError> {
        const STEP: &str = "agent environment check";
        const LIMIT: u64 = 64 * 1024;
        // Keys come only from the compiled adapter. Quoted expansions keep values
        // as data, and querying only these non-secret settings excludes credentials.
        let expansions = keys
            .iter()
            .map(|key| format!("\"${key}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!("printf '%s\\000' {expansions}");
        let mut child = self
            .exec_command(round, &["/bin/sh".into(), "-c".into(), script.into()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| OpenShellError::Launch { step: STEP, source })?;
        let read = (|| {
            let stdout = child
                .stdout
                .take()
                .ok_or(OpenShellError::InvalidResponse(STEP))?;
            let mut bytes = Vec::new();
            stdout
                .take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|source| OpenShellError::Launch { step: STEP, source })?;
            if bytes.len() as u64 > LIMIT {
                return Err(OpenShellError::EnvironmentTooLarge);
            }
            Ok(bytes)
        })();
        if read.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let bytes = read?;
        let status = child
            .wait()
            .map_err(|source| OpenShellError::Launch { step: STEP, source })?;
        if !status.success() {
            return Err(OpenShellError::Failed(STEP));
        }
        let values = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.strip_suffix('\0'))
            .ok_or(OpenShellError::InvalidResponse(STEP))?
            .split('\0')
            .collect::<Vec<_>>();
        if values.len() != keys.len() {
            return Err(OpenShellError::InvalidResponse(STEP));
        }
        keys.iter()
            .zip(values)
            .map(|(&key, value)| {
                if !valid_environment_entry(key, value) {
                    return Err(OpenShellError::InvalidResponse(STEP));
                }
                Ok((key.to_owned(), value.to_owned()))
            })
            .collect()
    }

    pub fn check_agent_executable(
        &self,
        round: &RoundRecord,
        executable: &str,
    ) -> Result<(), OpenShellError> {
        let result = self
            .exec_command(round, &[executable.into(), "--version".into()])
            .output()
            .map_err(|source| OpenShellError::Launch {
                step: "agent executable check",
                source,
            })?;
        if !result.status.success() {
            return Err(OpenShellError::Failed("agent executable check"));
        }
        Ok(())
    }

    pub fn stop(&self, round: &RoundRecord) -> Result<(), OpenShellError> {
        self.require_success_at(
            round.gateway(),
            round.workspace(),
            &["sandbox", "stop", round.sandbox()],
            "sandbox stop",
        )?;
        Ok(())
    }

    pub fn delete(&self, round: &RoundRecord) -> Result<(), OpenShellError> {
        let result = self.command_at(
            round.gateway(),
            round.workspace(),
            &["sandbox", "delete", round.sandbox()],
            "sandbox delete",
        )?;
        if result.status.success()
            || String::from_utf8_lossy(&result.stderr)
                .to_ascii_lowercase()
                .contains("not found")
        {
            Ok(())
        } else {
            Err(OpenShellError::Failed("sandbox delete"))
        }
    }

    pub fn exec_command(&self, round: &RoundRecord, command: &[OsString]) -> Command {
        self.exec_command_at(round, None, command)
    }

    pub fn exec_review_agent(&self, round: &RoundRecord, command: &[OsString]) -> Command {
        self.exec_command_at(round, Some(sandbox_layout::WORKDIR_TARGET), command)
    }

    fn exec_command_at(
        &self,
        round: &RoundRecord,
        workdir: Option<&str>,
        command: &[OsString],
    ) -> Command {
        let mut child = Command::new(self.config.openshell());
        self.base_args(&mut child, round.gateway(), round.workspace());
        child.args([
            "sandbox",
            "exec",
            round.sandbox(),
            "--no-tty",
            "--no-login-shell",
            "--timeout",
        ]);
        child.arg(self.config.timeout_seconds().to_string());
        if let Some(workdir) = workdir {
            child.args(["--workdir", workdir]);
        }
        child.arg("--");
        child.args(command);
        child
    }

    fn require_success(&self, args: &[&str], step: &'static str) -> Result<Output, OpenShellError> {
        self.require_success_at(self.config.gateway(), self.config.workspace(), args, step)
    }

    fn require_success_at(
        &self,
        gateway: &str,
        workspace: &str,
        args: &[&str],
        step: &'static str,
    ) -> Result<Output, OpenShellError> {
        let result = self.command_at(gateway, workspace, args, step)?;
        if !result.status.success() {
            return Err(OpenShellError::Failed(step));
        }
        Ok(result)
    }

    fn command(&self, args: &[&str], step: &'static str) -> Result<Output, OpenShellError> {
        self.command_at(self.config.gateway(), self.config.workspace(), args, step)
    }

    fn command_at(
        &self,
        gateway: &str,
        workspace: &str,
        args: &[&str],
        step: &'static str,
    ) -> Result<Output, OpenShellError> {
        let mut command = Command::new(self.config.openshell());
        self.base_args(&mut command, gateway, workspace);
        command.args(args);
        command
            .output()
            .map_err(|source| OpenShellError::Launch { step, source })
    }

    fn command_os(&self, args: &[OsString], step: &'static str) -> Result<Output, OpenShellError> {
        let mut command = Command::new(self.config.openshell());
        self.base_args(&mut command, self.config.gateway(), self.config.workspace());
        command.args(args);
        command
            .output()
            .map_err(|source| OpenShellError::Launch { step, source })
    }

    fn base_args(&self, command: &mut Command, gateway: &str, workspace: &str) {
        command.args([
            "--gateway",
            gateway,
            "--workspace",
            workspace,
            "--color",
            "never",
        ]);
        for key in [
            "OPENSHELL_GATEWAY",
            "OPENSHELL_GATEWAY_ENDPOINT",
            "OPENSHELL_GATEWAY_INSECURE",
            "OPENSHELL_WORKSPACE",
        ] {
            command.env_remove(key);
        }
    }
}

fn reports_driver(value: &Value, expected: Driver) -> bool {
    value
        .get("compute_drivers")
        .and_then(Value::as_array)
        .is_some_and(|drivers| {
            drivers
                .iter()
                .any(|driver| driver.get("name").and_then(Value::as_str) == Some(expected.as_str()))
        })
}

fn provider_type(output: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(output).ok()?;
    let value = text
        .lines()
        .filter_map(|line| line.trim().split_once(':'))
        .find_map(|(key, value)| (key == "Type").then_some(value.trim()))?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return None;
    }
    Some(value.to_owned())
}

fn github_profile_is_read_only(profile: &Value) -> bool {
    profile.get("inference_capable") == Some(&Value::Bool(false))
        && profile
            .get("endpoints")
            .and_then(Value::as_array)
            .is_some_and(|endpoints| {
                !endpoints.is_empty()
                    && endpoints.iter().all(|endpoint| {
                        endpoint.get("host").and_then(Value::as_str) == Some("api.github.com")
                            && endpoint.get("port").and_then(Value::as_u64) == Some(443)
                            && endpoint.get("protocol").and_then(Value::as_str) == Some("rest")
                            && endpoint.get("access").and_then(Value::as_str) == Some("read-only")
                            && endpoint.get("enforcement").and_then(Value::as_str)
                                == Some("enforce")
                    })
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::config::ReviewConfig;
    #[cfg(unix)]
    use crate::review::{RoundId, RoundRecord};
    #[cfg(unix)]
    use crate::{HeadSha, InputRevision, ObservedDiscussion, PrKey};
    #[cfg(unix)]
    use std::num::NonZeroU64;
    #[test]
    fn driver_info_must_explicitly_report_the_configured_driver() {
        assert!(reports_driver(
            &serde_json::json!({"compute_drivers":[{"name":"vm","capabilities":{"driver_name":"openshell-driver-vm"}}]}),
            Driver::Vm
        ));
        assert!(!reports_driver(
            &serde_json::json!({"compute_drivers":[{"name":"podman"}]}),
            Driver::Vm
        ));
        assert!(!reports_driver(
            &serde_json::json!({"status":"healthy"}),
            Driver::Vm
        ));
        let kubernetes = serde_json::json!({"compute_drivers":[{"name":"kubernetes"}]});
        assert!(reports_driver(&kubernetes, Driver::Kubernetes));
        assert!(!reports_driver(&kubernetes, Driver::Vm));
        let readonly = serde_json::json!({"inference_capable":false,"endpoints":[{
            "host":"api.github.com","port":443,"protocol":"rest","access":"read-only","enforcement":"enforce"
        }]});
        assert!(github_profile_is_read_only(&readonly));
        let mut writable = readonly;
        writable["endpoints"][0]["access"] = "read-write".into();
        assert!(!github_profile_is_read_only(&writable));
        assert_eq!(
            provider_type(b"Provider:\n  Type: github-pr-readonly\n"),
            Some("github-pr-readonly".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn kubernetes_prepares_reviewers_from_the_inherited_environment() {
        use crate::config::AgentKind;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("openshell");
        let calls = dir.path().join("calls");
        let response = dir.path().join("response");
        let failure = dir.path().join("failure");
        fs::write(
            &executable,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if test -e '{}'; then echo PRIVATE_ENV_DETAIL >&2; exit 1; fi
if test -e '{}'; then cat '{}'; exit; fi
while test "$#" -gt 0 && test "$1" != --; do shift; done
shift
export CLAUDE_CODE_EXECUTABLE='/opt/image/claude$(false)'
export CODEX_PATH='/opt/image/codex'
export OPENAI_BASE_URL='https://image.example/v1'
export OPENAI_API_KEY='PRIVATE_PROVIDER_CREDENTIAL'
exec "$@"
"#,
                calls.display(),
                failure.display(),
                response.display(),
                response.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut raw = serde_json::json!({
            "schema_version":7,
            "execution": crate::config::test_execution_json(),
            "models":[{"id":"model-alpha","coauthors":[]}],
            "agents":[{"id":"reviewer","models":["model-alpha"],"runner":crate::config::test_runner_json()}],
            "repositories":[{"name":"owner/repo","prompt_file":crate::config::test_prompt_file("review.md"),"review_models":["model-alpha"],"runners":[{
                "agent":"reviewer","oci_image":format!("registry.example.com/reviewer@sha256:{}","a".repeat(64))
            }]}]
        });
        raw["execution"]["openshell"] = serde_json::json!(executable);
        raw["execution"]["driver"] = "kubernetes".into();
        for (kind, expected) in [
            (AgentKind::Claude, "/opt/image/claude$(false)"),
            (AgentKind::Codex, "/opt/image/codex"),
        ] {
            raw["agents"][0]["runner"]["kind"] = kind.as_str().into();
            let config = ReviewConfig::from_json(
                &serde_json::to_vec(&raw).unwrap(),
                &dir.path().join("config.json"),
            )
            .unwrap();
            let key = PrKey::new("owner/repo", 42).unwrap();
            let choices = config.review_models_for(&key).unwrap();
            let choice = &choices[0];
            let runner = choice.agent().runner();
            let record = RoundRecord::new(
                RoundId::new("r-env").unwrap(),
                key,
                HeadSha::new("a".repeat(40)).unwrap(),
                InputRevision::new(2).unwrap(),
                ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap(),
                choice,
                1,
            );
            let image =
                crate::image::stage_image(choice.image(), &dir.path().join("unused")).unwrap();
            let shell = OpenShell::new(config.execution());
            let prepared = shell.prepare_agent(&record, runner, &image).unwrap();
            assert_eq!(prepared.executable(), expected);
            if kind == AgentKind::Codex {
                let command = agent::adapter(kind).command(&agent::AgentInvocation::new(
                    &prepared,
                    &record,
                    "/sandbox/instructions/system-prompt.txt",
                    "Review the PR.",
                ));
                assert!(command.iter().any(
                    |arg| arg == "model_providers.review.base_url=\"https://image.example/v1\""
                ));
            }
            let args = shell
                .create_args(
                    &record,
                    runner,
                    &image,
                    Path::new("policy.yaml"),
                    &SandboxMounts::new(dir.path(), dir.path(), dir.path()),
                )
                .unwrap();
            assert!(!args.iter().any(|arg| {
                arg.to_string_lossy().starts_with("CLAUDE_CODE_EXECUTABLE=")
                    || arg.to_string_lossy().starts_with("CODEX_PATH=")
                    || arg.to_string_lossy().starts_with("OPENAI_BASE_URL=")
            }));
            for &(key, value) in agent::adapter(kind).environment() {
                assert!(
                    args.windows(2)
                        .any(|pair| pair[0] == "--env"
                            && pair[1] == format!("{key}={value}").as_str())
                );
            }

            let values = match kind {
                AgentKind::Claude => b"\0".as_slice(),
                AgentKind::Codex => b"\0https://image.example/v1\0".as_slice(),
            };
            for path in ["", "bin/relative", "/usr/../bin/reviewer"] {
                let bytes = [path.as_bytes(), values].concat();
                fs::write(&response, bytes).unwrap();
                assert!(matches!(
                    shell.prepare_agent(&record, runner, &image),
                    Err(OpenShellError::AgentImage(
                        AgentPreparationError::MissingExecutable { .. }
                    ))
                ));
            }
            if kind == AgentKind::Codex {
                fs::write(
                    &response,
                    b"/usr/bin/codex\0https://credential@image.example/v1\0",
                )
                .unwrap();
                assert!(matches!(
                    shell.prepare_agent(&record, runner, &image),
                    Err(OpenShellError::AgentImage(
                        AgentPreparationError::InvalidCodexEndpoint
                    ))
                ));
            }
            for bytes in [
                b"unterminated".as_slice(),
                b"\xff\0",
                b"/usr/bin/reviewer\n\0",
                b"\0\0\0",
            ] {
                fs::write(&response, bytes).unwrap();
                assert!(matches!(
                    shell.prepare_agent(&record, runner, &image),
                    Err(OpenShellError::InvalidResponse("agent environment check"))
                ));
            }
            fs::write(&response, vec![b'x'; 64 * 1024 + 1]).unwrap();
            assert!(matches!(
                shell.prepare_agent(&record, runner, &image),
                Err(OpenShellError::EnvironmentTooLarge)
            ));
            fs::write(&failure, b"").unwrap();
            let error = match shell.prepare_agent(&record, runner, &image) {
                Ok(_) => panic!("failed environment check was accepted"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                OpenShellError::Failed("agent environment check")
            ));
            assert!(!error.to_string().contains("PRIVATE_ENV_DETAIL"));
            fs::remove_file(&failure).unwrap();
            fs::remove_file(&response).unwrap();
        }
        let recorded = fs::read_to_string(calls).unwrap();
        assert!(recorded.contains("--no-login-shell"));
        assert!(!recorded.contains("OPENAI_API_KEY"));
        assert!(!recorded.contains("PRIVATE_PROVIDER_CREDENTIAL"));
        assert!(!dir.path().join("unused").exists());
    }

    #[cfg(unix)]
    #[test]
    fn kubernetes_uploads_only_private_inputs_and_stops_on_transfer_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("openshell");
        let calls = dir.path().join("calls.txt");
        let fail_upload = dir.path().join("fail-upload");
        let gateway_info = dir.path().join("gateway.json");
        fs::write(
            &gateway_info,
            r#"{"compute_drivers":[{"name":"kubernetes"}]}"#,
        )
        .unwrap();
        fs::write(&executable, format!(
            "#!/bin/sh\nprintf '%s|%s\\n' \"$PWD\" \"$*\" >> '{}'\ncase \"$*\" in\n*--version*) echo 'openshell 0.1.7';;\n*'gateway info'*) cat '{}' ;;\n*'provider get'*) for last do :; done; echo \"Type: $last\";;\n*'profile describe'*test-model-provider*) echo '{{\"inference_capable\":true}}';;\n*'profile describe'*test-github-provider*) echo '{{\"inference_capable\":false,\"endpoints\":[{{\"host\":\"api.github.com\",\"port\":443,\"protocol\":\"rest\",\"access\":\"read-only\",\"enforcement\":\"enforce\"}}]}}';;\n*'sandbox upload'*) if test -e '{}'; then echo private-adapter-output >&2; exit 1; fi;;\nesac\n",
            calls.display(), gateway_info.display(), fail_upload.display(),
        )).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = dir.path().join("policy.yaml");
        fs::write(&policy, b"policy: test\n").unwrap();
        let mut raw = serde_json::json!({
            "schema_version":7,
            "execution": crate::config::test_execution_json(),
            "models":[{"id":"model-alpha","coauthors":[]}],
            "agents":[{"id":"claude","models":["model-alpha"],"runner":crate::config::test_runner_json()}],
            "repositories":[{"name":"owner/repo","prompt_file":crate::config::test_prompt_file("review.md"),"review_models":["model-alpha"],"runners":[{
                "agent":"claude","oci_image":format!("registry.example.com/reviewer@sha256:{}","a".repeat(64))
            }]}]
        });
        raw["execution"]["openshell"] = serde_json::json!(executable);
        raw["execution"]["driver"] = "kubernetes".into();
        raw["agents"][0]["runner"]["policy"] = serde_json::json!(policy);
        raw["agents"][0]["runner"]["policy_sha256"] =
            format!("{:x}", Sha256::digest(b"policy: test\n")).into();
        let config = ReviewConfig::from_json(
            &serde_json::to_vec(&raw).unwrap(),
            &dir.path().join("config.json"),
        )
        .unwrap();
        let shell = OpenShell::new(config.execution());
        shell.preflight_gateway().unwrap();
        let key = PrKey::new("owner/repo", 42).unwrap();
        let choices = config.review_models_for(&key).unwrap();
        let choice = &choices[0];
        shell.preflight_runner(choice.agent().runner()).unwrap();
        let record = RoundRecord::new(
            RoundId::new("r-kube").unwrap(),
            key,
            HeadSha::new("a".repeat(40)).unwrap(),
            InputRevision::new(2).unwrap(),
            ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap(),
            choice,
            1,
        );
        let image =
            crate::image::stage_image(choice.image(), &dir.path().join("archives")).unwrap();
        let checkout = dir.path().join("checkout");
        let instructions = dir.path().join("instructions");
        let output = dir.path().join("output");
        for path in [&checkout, &instructions, &output] {
            fs::create_dir(path).unwrap();
        }
        let inputs = SandboxMounts::new(&checkout, &instructions, &output);
        let args = shell
            .create_args(&record, choice.agent().runner(), &image, &policy, &inputs)
            .unwrap();
        assert!(!args.iter().any(|arg| arg == "--driver-config-json"));
        for path in [&checkout, &instructions, &output] {
            assert!(!args.iter().any(|arg| arg == path));
        }
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--from" && pair[1] == image.source())
        );
        shell
            .create(&record, choice.agent().runner(), &image, &policy, &inputs)
            .unwrap();
        let recorded = fs::read_to_string(&calls).unwrap();
        for (source, target) in [
            (&checkout, sandbox_layout::CHECKOUT_TARGET),
            (&instructions, sandbox_layout::INSTRUCTIONS_TARGET),
            (&output, sandbox_layout::OUTPUT_TARGET),
        ] {
            assert!(recorded.lines().any(|line| line == format!("{}|--gateway test-gateway --workspace default --color never sandbox upload {} . {target} --no-git-ignore",source.canonicalize().unwrap().display(),record.sandbox())));
        }
        assert!(recorded.contains("/bin/chmod -R a-w -- /sandbox/src /sandbox/instructions"));
        fs::write(&calls, b"").unwrap();
        fs::write(&fail_upload, b"").unwrap();
        let error = shell
            .create(&record, choice.agent().runner(), &image, &policy, &inputs)
            .unwrap_err();
        assert!(matches!(error, OpenShellError::Failed("input upload")));
        assert!(!error.to_string().contains("private-adapter-output"));
        let recorded = fs::read_to_string(&calls).unwrap();
        assert_eq!(recorded.lines().count(), 2);
        assert!(!recorded.contains("sandbox exec"));
        fs::write(&gateway_info, r#"{"compute_drivers":[{"name":"vm"}]}"#).unwrap();
        assert!(matches!(
            shell.preflight_gateway(),
            Err(OpenShellError::DriverUnavailable("kubernetes"))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fake_cli_preflight_and_command_use_explicit_gateway_providers_and_read_only_mounts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("openshell");
        let calls = dir.path().join("calls.txt");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  *--version*) echo 'openshell 0.1.7';;\n  *'gateway info'*) echo '{{\"compute_drivers\":[{{\"name\":\"vm\"}}]}}';;\n  *'provider get'*) for last do :; done; echo \"Type: $last\";;\n  *test-model-provider*) echo '{{\"inference_capable\":true}}';;\n  *test-github-provider*) echo '{{\"inference_capable\":false,\"endpoints\":[{{\"host\":\"api.github.com\",\"port\":443,\"protocol\":\"rest\",\"access\":\"read-only\",\"enforcement\":\"enforce\"}}]}}';;\nesac\n",
            calls.display()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = dir.path().join("policy.yaml");
        fs::write(&policy, b"policy: test\n").unwrap();
        let mut raw = serde_json::json!({
            "schema_version":7,
            "execution": crate::config::test_execution_json(),
            "models":[{"id":"model-alpha","coauthors":[]}],
            "agents":[{"id":"claude","models":["model-alpha"],"runner":crate::config::test_runner_json()}],
            "repositories":[{"name":"owner/repo", "prompt_file": crate::config::test_prompt_file("review.md"),"review_models":["model-alpha"],"runners":[{
                "agent":"claude","vm_archive":{"path":"/tmp/reviewer.tar","sha256":"a".repeat(64),"config_sha256":"c".repeat(64)}
            }]}]
        });
        raw["execution"]["openshell"] = executable.to_str().unwrap().into();
        raw["agents"][0]["runner"]["policy"] = policy.to_str().unwrap().into();
        raw["agents"][0]["runner"]["policy_sha256"] =
            format!("{:x}", Sha256::digest(b"policy: test\n")).into();
        let config = ReviewConfig::from_json(
            &serde_json::to_vec(&raw).unwrap(),
            std::path::Path::new("config.json"),
        )
        .unwrap();
        let shell = OpenShell::new(config.execution());
        shell.preflight_gateway().unwrap();
        shell.preflight_runner(config.agents()[0].runner()).unwrap();
        let key = PrKey::new("owner/repo", 42).unwrap();
        let choice = config.review_models_for(&key).unwrap();
        let record = RoundRecord::new(
            RoundId::new("r-VgdQi9").unwrap(),
            key,
            HeadSha::new("a".repeat(40)).unwrap(),
            InputRevision::new(2).unwrap(),
            ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap(),
            &choice[0],
            1,
        );
        let review_command = shell.exec_review_agent(&record, &[OsString::from("claude")]);
        let review_args = review_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            review_args.windows(2).any(|args| {
                args[0] == "--workdir" && args[1] == sandbox_layout::WORKDIR_TARGET
            })
        );
        let args = shell
            .create_args(
                &record,
                choice[0].agent().runner(),
                &StagedImage::for_test(
                    PathBuf::from("/tmp/archive.tar"),
                    std::collections::BTreeMap::from([
                        ("PATH".into(), "/usr/local/bin:/usr/bin".into()),
                        (
                            "CLAUDE_CODE_EXECUTABLE".into(),
                            "/usr/local/bin/claude".into(),
                        ),
                    ]),
                ),
                &policy,
                &SandboxMounts::new(
                    Path::new("/tmp/src"),
                    Path::new("/tmp/instructions"),
                    Path::new("/tmp/output"),
                ),
            )
            .unwrap();
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2)
                .any(|items| { items[0] == "--name" && items[1] == "luthiai-kztwiuljhe" })
        );
        assert!(args.contains(&"--no-auto-providers".to_owned()));
        assert!(!args.contains(&"--auto-providers".to_owned()));
        assert_eq!(
            args.iter()
                .filter(|arg| arg.as_str() == "--provider")
                .count(),
            2
        );
        assert!(args.windows(2).any(|items| {
            items[0] == "--env" && items[1] == "CLAUDE_CODE_EXECUTABLE=/usr/local/bin/claude"
        }));
        assert!(
            args.windows(2)
                .any(|items| { items[0] == "--env" && items[1] == "PATH=/usr/local/bin:/usr/bin" })
        );
        assert!(args.windows(2).any(|items| {
            items[0] == "--env" && items[1] == "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1"
        }));
        assert!(
            args.windows(2)
                .any(|items| { items[0] == "--provider" && items[1] == "test-model-provider" })
        );
        let mount_json = args
            .get(
                args.iter()
                    .position(|arg| arg == "--driver-config-json")
                    .unwrap()
                    + 1,
            )
            .unwrap();
        let mounts: Value = serde_json::from_str(mount_json).unwrap();
        assert_eq!(mounts["vm"]["mounts"][0]["read_only"], true);
        assert_eq!(mounts["vm"]["mounts"][1]["read_only"], true);
        assert_eq!(mounts["vm"]["mounts"][2]["read_only"], false);
        assert_eq!(
            mounts["vm"]["mounts"][0]["target"],
            sandbox_layout::CHECKOUT_TARGET
        );
        assert_eq!(
            mounts["vm"]["mounts"][1]["target"],
            sandbox_layout::INSTRUCTIONS_TARGET
        );
        assert_eq!(
            mounts["vm"]["mounts"][2]["target"],
            sandbox_layout::OUTPUT_TARGET
        );
        let oversized = RoundRecord::new(
            RoundId::new("r-abcdefghijklmn").unwrap(),
            record.key().clone(),
            record.head_sha().clone(),
            record.revision(),
            record.discussion().clone(),
            &choice[0],
            1,
        );
        let before = fs::read(&calls).unwrap();
        assert!(matches!(
            shell.create(
                &oversized,
                choice[0].agent().runner(),
                &StagedImage::for_test(
                    PathBuf::from("/tmp/archive.tar"),
                    std::collections::BTreeMap::new(),
                ),
                &policy,
                &SandboxMounts::new(
                    Path::new("/tmp/src"),
                    Path::new("/tmp/instructions"),
                    Path::new("/tmp/output"),
                ),
            ),
            Err(OpenShellError::SandboxNameTooLong)
        ));
        assert_eq!(fs::read(&calls).unwrap(), before);
        let mut conflicting = raw.clone();
        conflicting["agents"][0]["runner"]["environment"] = serde_json::json!({"PATH":"/bin"});
        let conflicting = ReviewConfig::from_json(
            &serde_json::to_vec(&conflicting).unwrap(),
            std::path::Path::new("config.json"),
        )
        .unwrap();
        let conflicting_choice = conflicting.review_models_for(record.key()).unwrap();
        assert!(matches!(
            OpenShell::new(conflicting.execution()).create_args(
                &record,
                conflicting_choice[0].agent().runner(),
                &StagedImage::for_test(
                    PathBuf::from("/tmp/archive.tar"),
                    std::collections::BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
                ),
                &policy,
                &SandboxMounts::new(
                    Path::new("/tmp/src"),
                    Path::new("/tmp/instructions"),
                    Path::new("/tmp/output"),
                ),
            ),
            Err(OpenShellError::EnvironmentCollision(key)) if key == "PATH"
        ));
        let calls = fs::read_to_string(&calls).unwrap();
        for line in calls.lines() {
            assert!(line.contains("--gateway test-gateway --workspace default"));
        }
        raw["execution"]["driver"] = "podman".into();
        let podman = ReviewConfig::from_json(
            &serde_json::to_vec(&raw).unwrap(),
            std::path::Path::new("config.json"),
        )
        .unwrap();
        let before = fs::read(dir.path().join("calls.txt")).unwrap();
        assert!(matches!(
            OpenShell::new(podman.execution()).preflight_gateway(),
            Err(OpenShellError::PodmanUnsupported)
        ));
        assert_eq!(fs::read(dir.path().join("calls.txt")).unwrap(), before);
        raw["execution"]["driver"] = "unknown".into();
        assert!(
            ReviewConfig::from_json(
                &serde_json::to_vec(&raw).unwrap(),
                std::path::Path::new("config.json")
            )
            .is_err()
        );
    }
}
