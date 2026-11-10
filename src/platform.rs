//! What differs between unix and Windows: the home directory, which files
//! are programs, how a shell command line is run, and the signal that ended
//! a child. Everything else in hledgers is plain std and builds anywhere.

/// The user's home directory (getHomeDirectory): HOME, or USERPROFILE on
/// Windows when HOME is unset.
pub fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|e| {
            if cfg!(windows) {
                std::env::var("USERPROFILE")
            } else {
                Err(e)
            }
        })
        .unwrap_or_default()
}

/// Whether a path is a program that can be run (executable): on unix a
/// regular file with an execute bit, on Windows a regular file with a
/// program extension, or the path with `.exe` added.
#[cfg(unix)]
pub fn is_executable(f: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    f.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub fn is_executable(f: &std::path::Path) -> bool {
    match f.extension().and_then(|e| e.to_str()) {
        Some(e) => {
            ["exe", "bat", "cmd", "com"].contains(&e.to_ascii_lowercase().as_str()) && f.is_file()
        }
        None => f.with_extension("exe").is_file(),
    }
}

/// A command that runs a shell command line (shell): `/bin/sh -c` on unix,
/// `cmd /C` on Windows.
pub fn shell_command(cmd: &str) -> std::process::Command {
    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("/bin/sh", "-c")
    };
    let mut command = std::process::Command::new(shell);
    command.arg(flag).arg(cmd);
    command
}

/// The signal that ended a child process, if one did; never on Windows.
#[cfg(unix)]
pub fn exit_signal(status: std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}

#[cfg(not(unix))]
pub fn exit_signal(_status: std::process::ExitStatus) -> Option<i32> {
    None
}

/// Options that create or empty a file only its owner may read (on unix;
/// Windows files inherit their directory's permissions).
pub fn private_file_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}
