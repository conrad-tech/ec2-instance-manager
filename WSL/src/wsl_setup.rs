use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::config::AppConfig;

/// Result of checking WSL prerequisites.
#[derive(Clone, Debug)]
pub struct WslSetupStatus {
    pub wsl_available: bool,
    pub aws_cli_installed: bool,
    pub ssm_plugin_installed: bool,
    pub aws_credentials_linked: bool,
    pub setup_log: Vec<String>,
}

impl WslSetupStatus {
    pub fn is_ready(&self) -> bool {
        self.wsl_available
            && self.aws_cli_installed
            && self.ssm_plugin_installed
            && self.aws_credentials_linked
    }
}

/// Path to the cached setup-complete marker file.
fn cache_path() -> Option<PathBuf> {
    AppConfig::config_path().map(|p| p.with_file_name("wsl_setup_done"))
}

/// Check if we've already cached a successful setup.
pub fn is_setup_cached() -> bool {
    cache_path().map(|p| p.exists()).unwrap_or(false)
}

/// Write the cache marker so future launches skip the check.
pub fn mark_setup_done() {
    if let Some(path) = cache_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, "ok");
    }
}

/// Clear the cache so the next launch re-checks.
pub fn clear_setup_cache() {
    if let Some(path) = cache_path() {
        let _ = fs::remove_file(&path);
    }
}

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Run a command inside WSL and return (success, stdout+stderr).
fn wsl_run(args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let result = cmd.output();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = format!("{stdout}{stderr}");
            (output.status.success(), combined.trim().to_string())
        }
        Err(e) => (false, format!("failed to run wsl.exe: {e}")),
    }
}

/// Check if wsl.exe is available at all.
fn check_wsl_available() -> bool {
    let mut cmd = Command::new("wsl.exe");
    cmd.arg("--status")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check all WSL prerequisites without installing anything.
pub fn check_wsl_setup() -> WslSetupStatus {
    let mut log = Vec::new();

    let wsl_available = check_wsl_available();
    log.push(format!(
        "WSL available: {}",
        if wsl_available { "yes" } else { "no" }
    ));

    if !wsl_available {
        return WslSetupStatus {
            wsl_available: false,
            aws_cli_installed: false,
            ssm_plugin_installed: false,
            aws_credentials_linked: false,
            setup_log: log,
        };
    }

    let (aws_ok, aws_out) = wsl_run(&["-e", "bash", "-lc", "aws --version"]);
    if aws_ok {
        log.push(format!("AWS CLI: {aws_out}"));
    } else {
        log.push("AWS CLI: not found".to_string());
        log.push(format!("  detail: {aws_out}"));
    }

    let (ssm_ok, ssm_out) = wsl_run(&["-e", "bash", "-lc", "session-manager-plugin --version"]);
    if ssm_ok {
        log.push(format!("Session Manager Plugin: {ssm_out}"));
    } else {
        log.push("Session Manager Plugin: not found".to_string());
        log.push(format!("  detail: {ssm_out}"));
    }

    let (creds_ok, creds_out) = wsl_run(&["-e", "bash", "-lc", "test -d ~/.aws && echo ok || echo missing"]);
    log.push(format!("AWS credentials (~/.aws): {}", if creds_ok { "found" } else { "not found" }));
    if !creds_ok {
        log.push(format!("  detail: {creds_out}"));
    }

    // Extra debug info
    let (_, distro) = wsl_run(&["-e", "bash", "-c", "cat /etc/os-release 2>/dev/null | head -2"]);
    log.push(format!("WSL distro: {distro}"));
    let (_, user) = wsl_run(&["-e", "bash", "-c", "whoami"]);
    log.push(format!("WSL user: {user}"));
    let (_, sudo_check) = wsl_run(&["-e", "bash", "-c", "command -v sudo && echo ok || echo missing"]);
    log.push(format!("sudo: {sudo_check}"));
    let (_, unzip_check) = wsl_run(&["-e", "bash", "-c", "command -v unzip && echo ok || echo missing"]);
    log.push(format!("unzip: {unzip_check}"));

    WslSetupStatus {
        wsl_available,
        aws_cli_installed: aws_ok,
        ssm_plugin_installed: ssm_ok,
        aws_credentials_linked: creds_ok,
        setup_log: log,
    }
}

/// Build the setup script that runs inside a visible WSL terminal.
fn build_setup_script() -> String {
    let win_user = std::env::var("USERNAME").unwrap_or_default();

    let mut script = String::from("#!/bin/bash\nset -e\necho '=== EC2 Manager WSL Setup ==='\necho ''\n");

    // Install prerequisites (sudo, unzip, curl) if missing
    script.push_str(r#"
NEED_APT=""
command -v sudo &>/dev/null || NEED_APT="$NEED_APT sudo"
command -v unzip &>/dev/null || NEED_APT="$NEED_APT unzip"
command -v curl &>/dev/null || NEED_APT="$NEED_APT curl"
if [ -n "$NEED_APT" ]; then
    echo "Installing missing system packages:$NEED_APT"
    # First install may need to run without sudo if sudo itself is missing
    if command -v sudo &>/dev/null; then
        sudo apt-get update -qq && sudo apt-get install -y -qq $NEED_APT
    else
        apt-get update -qq && apt-get install -y -qq $NEED_APT
    fi
    echo "System packages installed."
fi
echo ''
"#);

    // AWS CLI v2
    script.push_str(r#"
if command -v aws &>/dev/null; then
    echo "AWS CLI: already installed ($(aws --version))"
else
    echo "Installing AWS CLI v2..."
    cd /tmp
    curl -s 'https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip' -o awscliv2.zip
    unzip -qo awscliv2.zip
    sudo ./aws/install --update
    rm -rf aws awscliv2.zip
    echo "AWS CLI v2 installed."
fi
echo ''
"#);

    // Session Manager Plugin
    script.push_str(r#"
if command -v session-manager-plugin &>/dev/null; then
    echo "Session Manager Plugin: already installed ($(session-manager-plugin --version 2>&1 || true))"
else
    echo "Installing Session Manager Plugin..."
    cd /tmp
    curl -s 'https://s3.amazonaws.com/session-manager-downloads/plugin/latest/ubuntu_64bit/session-manager-plugin.deb' -o session-manager-plugin.deb
    sudo dpkg -i session-manager-plugin.deb
    rm -f session-manager-plugin.deb
    echo "Session Manager Plugin installed."
fi
echo ''
"#);

    // AWS credentials symlink
    if !win_user.is_empty() {
        script.push_str(&format!(r#"
if [ -d ~/.aws ]; then
    echo "AWS credentials: already linked"
else
    echo "Linking AWS credentials from Windows user '{win_user}'..."
    ln -s '/mnt/c/Users/{win_user}/.aws' ~/.aws
    echo "AWS credentials linked."
fi
echo ''
"#));
    } else {
        script.push_str("echo 'WARNING: Could not determine Windows username for credential linking.'\n");
        script.push_str("echo 'Run manually: ln -s /mnt/c/Users/YOUR_USER/.aws ~/.aws'\necho ''\n");
    }

    // Summary
    script.push_str(r#"
echo '=== Setup Complete ==='
echo 'You can close this window now.'
echo ''
read -p "Press Enter to close..."
"#);

    script
}

/// Run the full WSL setup in a visible terminal window.
/// Opens a real wsl.exe window so the user can see output and type sudo password.
/// Blocks until the terminal closes, then re-checks prerequisites.
pub fn run_wsl_setup() -> WslSetupStatus {
    let mut log = Vec::new();

    let wsl_available = check_wsl_available();
    if !wsl_available {
        log.push("ERROR: WSL is not available. Install WSL first.".to_string());
        return WslSetupStatus {
            wsl_available: false,
            aws_cli_installed: false,
            ssm_plugin_installed: false,
            aws_credentials_linked: false,
            setup_log: log,
        };
    }

    let script = build_setup_script();

    // Write the script to a temp file accessible from WSL
    let tmp_dir = std::env::temp_dir();
    let script_path = tmp_dir.join("ec2_manager_wsl_setup.sh");
    if let Err(e) = fs::write(&script_path, &script) {
        log.push(format!("Failed to write setup script: {e}"));
        return check_wsl_setup();
    }

    // Convert Windows path to WSL path: C:\Users\... -> /mnt/c/Users/...
    let win_path = script_path.to_string_lossy().replace('\\', "/");
    let wsl_path = if let Some(rest) = win_path.strip_prefix("//") {
        // UNC-style path from Git Bash: //c/Users/... -> /mnt/c/Users/...
        format!("/mnt/{rest}")
    } else if win_path.len() >= 2 && win_path.as_bytes()[1] == b':' {
        // Standard Windows path: C:/Users/... -> /mnt/c/Users/...
        let drive = win_path.as_bytes()[0].to_ascii_lowercase() as char;
        format!("/mnt/{drive}{}", &win_path[2..])
    } else {
        win_path.to_string()
    };

    log.push(format!("Script written to: {}", script_path.display()));
    log.push(format!("WSL path: {wsl_path}"));
    log.push(format!("Windows user: {}", std::env::var("USERNAME").unwrap_or_default()));
    log.push("Opening WSL setup terminal...".to_string());

    // Use "cmd /C start /wait ..." to force a new visible console window.
    // CREATE_NEW_CONSOLE alone doesn't work from a GUI app with windows_subsystem="windows".
    // The start command requires the title in quotes as the first arg.
    let start_cmd = format!(
        "start \"WSL Setup\" /wait wsl.exe -e bash \"{}\"",
        wsl_path
    );
    log.push(format!("launch cmd: cmd.exe /C {start_cmd}"));
    let result = Command::new("cmd.exe")
        .args(["/C", &start_cmd])
        .status();

    match result {
        Ok(status) => {
            if status.success() {
                log.push("Setup script completed successfully.".to_string());
            } else {
                log.push(format!("Setup script exited with: {status}"));
            }
        }
        Err(e) => {
            log.push(format!("Failed to launch WSL terminal: {e}"));
        }
    }

    // Clean up temp script
    let _ = fs::remove_file(&script_path);

    // Re-check everything
    let final_status = check_wsl_setup();
    let mut combined_log = log;
    combined_log.push(String::new());
    combined_log.push("--- Final status ---".to_string());
    combined_log.extend(final_status.setup_log.clone());

    if final_status.is_ready() {
        mark_setup_done();
        combined_log.push("Setup complete! Cached for future launches.".to_string());
    }

    WslSetupStatus {
        setup_log: combined_log,
        ..final_status
    }
}

/// Run a command inside WSL with sudo, piping the password via stdin.
/// Returns (success, output).
fn wsl_sudo_run(password: &str, cmd_str: &str) -> (bool, String) {
    let full_cmd = format!("echo '{}' | sudo -S bash -c '{}'", password, cmd_str);
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-e", "bash", "-c", &full_cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let result = cmd.output();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            // Filter out the "[sudo] password for..." prompt from stderr
            let filtered_stderr: String = stderr
                .lines()
                .filter(|l| !l.contains("[sudo] password"))
                .collect::<Vec<_>>()
                .join("\n");
            let combined = format!("{stdout}{filtered_stderr}");
            (output.status.success(), combined.trim().to_string())
        }
        Err(e) => (false, format!("failed to run wsl.exe: {e}")),
    }
}

/// Run the full WSL setup using a sudo password (piped, no terminal needed).
pub fn run_wsl_setup_with_password(password: &str) -> WslSetupStatus {
    let mut log = Vec::new();

    let wsl_available = check_wsl_available();
    if !wsl_available {
        log.push("ERROR: WSL is not available.".to_string());
        return WslSetupStatus {
            wsl_available: false,
            aws_cli_installed: false,
            ssm_plugin_installed: false,
            aws_credentials_linked: false,
            setup_log: log,
        };
    }

    // Verify the password works
    let (pwd_ok, pwd_out) = wsl_sudo_run(password, "echo ok");
    if !pwd_ok {
        log.push(format!("Sudo password verification failed: {pwd_out}"));
        log.push("Check your password and try again.".to_string());
        let status = check_wsl_setup();
        return WslSetupStatus {
            setup_log: log,
            ..status
        };
    }
    log.push("Sudo password verified.".to_string());

    // Install unzip and curl if missing
    let (_, unzip_check) = wsl_run(&["-e", "bash", "-c", "command -v unzip"]);
    if unzip_check.is_empty() {
        log.push("Installing unzip and curl...".to_string());
        let (ok, out) = wsl_sudo_run(password, "apt-get update -qq && apt-get install -y -qq unzip curl");
        if ok {
            log.push("unzip and curl installed.".to_string());
        } else {
            log.push(format!("Failed to install unzip/curl: {out}"));
        }
    }

    // Install AWS CLI v2
    let (aws_ok, _) = wsl_run(&["-e", "bash", "-lc", "aws --version"]);
    if aws_ok {
        log.push("AWS CLI: already installed.".to_string());
    } else {
        log.push("Installing AWS CLI v2...".to_string());
        let (ok, out) = wsl_sudo_run(
            password,
            "cd /tmp && curl -s 'https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip' -o awscliv2.zip && unzip -qo awscliv2.zip && ./aws/install --update && rm -rf aws awscliv2.zip",
        );
        if ok {
            log.push("AWS CLI v2 installed.".to_string());
        } else {
            log.push(format!("AWS CLI install failed: {out}"));
        }
    }

    // Install Session Manager Plugin
    let (ssm_ok, _) = wsl_run(&["-e", "bash", "-lc", "session-manager-plugin --version"]);
    if ssm_ok {
        log.push("Session Manager Plugin: already installed.".to_string());
    } else {
        log.push("Installing Session Manager Plugin...".to_string());
        let (ok, out) = wsl_sudo_run(
            password,
            "cd /tmp && curl -s 'https://s3.amazonaws.com/session-manager-downloads/plugin/latest/ubuntu_64bit/session-manager-plugin.deb' -o ssm.deb && dpkg -i ssm.deb && rm -f ssm.deb",
        );
        if ok {
            log.push("Session Manager Plugin installed.".to_string());
        } else {
            log.push(format!("Session Manager Plugin install failed: {out}"));
        }
    }

    // Link AWS credentials
    let (creds_ok, _) = wsl_run(&["-e", "bash", "-lc", "test -d ~/.aws && echo ok"]);
    if creds_ok {
        log.push("AWS credentials: already linked.".to_string());
    } else {
        let win_user = std::env::var("USERNAME").unwrap_or_default();
        if !win_user.is_empty() {
            log.push("Linking AWS credentials...".to_string());
            let link_cmd = format!("ln -s '/mnt/c/Users/{win_user}/.aws' ~/.aws");
            let (ok, out) = wsl_run(&["-e", "bash", "-c", &link_cmd]);
            if ok {
                log.push("AWS credentials linked.".to_string());
            } else {
                log.push(format!("Credential linking failed: {out}"));
            }
        } else {
            log.push("WARNING: Could not determine Windows username for credential linking.".to_string());
        }
    }

    // Re-check
    let final_status = check_wsl_setup();
    log.push(String::new());
    log.push("--- Final status ---".to_string());
    log.extend(final_status.setup_log.clone());

    if final_status.is_ready() {
        mark_setup_done();
        log.push("Setup complete! Cached for future launches.".to_string());
    }

    WslSetupStatus {
        setup_log: log,
        ..final_status
    }
}

/// Commands to uninstall everything that was installed by WSL setup.
pub fn uninstall_commands() -> String {
    "sudo rm -rf /usr/local/aws-cli /usr/local/bin/aws /usr/local/bin/aws_completer && \
     sudo dpkg -r session-manager-plugin && \
     rm ~/.aws"
        .to_string()
}
