//! Driver-specific platform checks and delivery of isolated review inputs.

use super::{OpenShell, OpenShellError, SandboxMounts};
use crate::config::Driver;
use crate::review::RoundRecord;
use crate::sandbox_layout;
use std::ffi::OsString;

pub(super) trait SandboxBackend: Sync {
    fn check_platform(&self) -> Result<(), OpenShellError>;

    fn create_args(&self, inputs: &SandboxMounts<'_>) -> Result<Vec<OsString>, OpenShellError>;

    fn prepare_inputs(
        &self,
        shell: &OpenShell<'_>,
        round: &RoundRecord,
        inputs: &SandboxMounts<'_>,
    ) -> Result<(), OpenShellError>;
}

struct VmBackend;
struct KubernetesBackend;

pub(super) fn for_driver(driver: Driver) -> Result<&'static dyn SandboxBackend, OpenShellError> {
    match driver {
        Driver::Vm => Ok(&VmBackend),
        Driver::Kubernetes => Ok(&KubernetesBackend),
        Driver::Podman => Err(OpenShellError::PodmanUnsupported),
    }
}

impl SandboxBackend for VmBackend {
    fn check_platform(&self) -> Result<(), OpenShellError> {
        if cfg!(unix) {
            Ok(())
        } else {
            Err(OpenShellError::PlatformUnsupported)
        }
    }

    fn create_args(&self, inputs: &SandboxMounts<'_>) -> Result<Vec<OsString>, OpenShellError> {
        let driver = serde_json::json!({"vm":{"mounts":[
            {"type":"bind","source":inputs.checkout,"target":sandbox_layout::CHECKOUT_TARGET,"read_only":true},
            {"type":"bind","source":inputs.instructions,"target":sandbox_layout::INSTRUCTIONS_TARGET,"read_only":true},
            {"type":"bind","source":inputs.output,"target":sandbox_layout::OUTPUT_TARGET,"read_only":false}
        ]}});
        let driver = serde_json::to_string(&driver)
            .map_err(|_| OpenShellError::InvalidResponse("mount configuration"))?;
        Ok(vec!["--driver-config-json".into(), driver.into()])
    }

    fn prepare_inputs(
        &self,
        _shell: &OpenShell<'_>,
        _round: &RoundRecord,
        _inputs: &SandboxMounts<'_>,
    ) -> Result<(), OpenShellError> {
        Ok(())
    }
}

impl SandboxBackend for KubernetesBackend {
    fn check_platform(&self) -> Result<(), OpenShellError> {
        Ok(())
    }

    fn create_args(&self, _inputs: &SandboxMounts<'_>) -> Result<Vec<OsString>, OpenShellError> {
        // Remote gateways do not receive local paths or require driver overrides.
        Ok(Vec::new())
    }

    fn prepare_inputs(
        &self,
        shell: &OpenShell<'_>,
        round: &RoundRecord,
        inputs: &SandboxMounts<'_>,
    ) -> Result<(), OpenShellError> {
        for (source, target) in [
            (inputs.checkout, sandbox_layout::CHECKOUT_TARGET),
            (inputs.instructions, sandbox_layout::INSTRUCTIONS_TARGET),
            (inputs.output, sandbox_layout::OUTPUT_TARGET),
        ] {
            let mut command = std::process::Command::new(shell.config.openshell());
            shell.base_args(&mut command, round.gateway(), round.workspace());
            // Upload `.` from each directory so the CLI does not add a basename.
            // Preserve .git and ignored input files; only these private inputs
            // are sent, never the enclosing state or configuration directories.
            command.current_dir(source).args([
                "sandbox",
                "upload",
                round.sandbox(),
                ".",
                target,
                "--no-git-ignore",
            ]);
            let result = command.output().map_err(|source| OpenShellError::Launch {
                step: "input upload",
                source,
            })?;
            if !result.status.success() {
                return Err(OpenShellError::Failed("input upload"));
            }
        }
        // These disposable copies cannot change the operator's inputs. File
        // modes discourage accidental writes; they are not an isolation boundary
        // because the sandbox user owns the uploaded files.
        let result = shell
            .exec_command(
                round,
                &[
                    "/bin/chmod".into(),
                    "-R".into(),
                    "a-w".into(),
                    "--".into(),
                    sandbox_layout::CHECKOUT_TARGET.into(),
                    sandbox_layout::INSTRUCTIONS_TARGET.into(),
                ],
            )
            .output()
            .map_err(|source| OpenShellError::Launch {
                step: "input permissions",
                source,
            })?;
        if !result.status.success() {
            return Err(OpenShellError::Failed("input permissions"));
        }
        Ok(())
    }
}
