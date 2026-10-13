//! Single connect helper (copied from oh-my-docker).
use anyhow::Result;
use bollard::Docker;

pub fn connect() -> Result<Docker> {
    let docker = Docker::connect_with_local_defaults()?;
    Ok(docker)
}
