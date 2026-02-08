use std::path::PathBuf;
use std::process::Command;

/// Returns the platform-appropriate default shell.
#[cfg(unix)]
pub fn get_default_shell() -> PathBuf {
    PathBuf::from(std::env::var("SHELL").unwrap_or_else(|_| {
        log::warn!("Cannot read SHELL env, falling back to use /bin/sh");
        "/bin/sh".to_string()
    }))
}

#[cfg(windows)]
pub fn get_default_shell() -> PathBuf {
    PathBuf::from(std::env::var("COMSPEC").unwrap_or_else(|_| {
        log::warn!("Cannot read COMSPEC env, falling back to use cmd.exe");
        "cmd.exe".to_string()
    }))
}

#[cfg(not(any(unix, windows)))]
pub fn get_default_shell() -> PathBuf {
    PathBuf::from(std::env::var("SHELL").unwrap_or_else(|_| {
        log::warn!("Cannot read SHELL env on unsupported platform, falling back to /bin/sh");
        "/bin/sh".to_string()
    }))
}

/// Run a script with the platform-appropriate shell interpreter.
/// Returns stdout as a trimmed string. Fails if the command exits non-zero.
#[cfg(unix)]
pub fn run_shell_command(
    script: &str,
    env_vars: &[(&str, &str)],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(format!("Hook failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

#[cfg(windows)]
pub fn run_shell_command(
    script: &str,
    env_vars: &[(&str, &str)],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(script);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(format!("Hook failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

#[cfg(not(any(unix, windows)))]
pub fn run_shell_command(
    script: &str,
    _env_vars: &[(&str, &str)],
) -> Result<String, Box<dyn std::error::Error>> {
    Err(format!(
        "run_shell_command not implemented on this platform (script: {})",
        script
    )
    .into())
}
