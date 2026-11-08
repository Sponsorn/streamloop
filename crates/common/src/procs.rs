//! The one place a child process is built. Nothing else calls `Command::new` (`repo_guards.rs`).

use std::process::{Command, Stdio};

/// Children start outside the install folder (2.x spawn-cwd rule).
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.current_dir(std::env::temp_dir()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_starts_in_the_temp_folder() {
        // Arrange
        let c = command("whatever");
        // Act
        let cwd = c.get_current_dir().map(|p| p.to_path_buf());
        // Assert
        assert_eq!(cwd, Some(std::env::temp_dir()));
    }
}
