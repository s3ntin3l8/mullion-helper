use std::{ffi::OsStr, process::Command};

/// Creates a child command that stays hidden when launched by the Windows GUI app.
///
/// Mullion's bundled Node SEA is a console-subsystem executable. Without this flag,
/// Windows gives every invocation its own console window even though its standard
/// streams are piped back to the tray app.
pub fn command<S: AsRef<OsStr>>(program: S) -> Command {
    configure(Command::new(program))
}

#[cfg(windows)]
fn configure(mut command: Command) -> Command {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW from WinBase.h. Keeping the documented value here avoids
    // a Windows bindings dependency for a single process-creation constant.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(not(windows))]
fn configure(command: Command) -> Command {
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_the_requested_program() {
        assert_eq!(command("mullion-worker").get_program(), "mullion-worker");
    }
}
