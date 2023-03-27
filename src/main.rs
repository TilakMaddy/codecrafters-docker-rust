#[cfg(target_os = "linux")]
use libc;

use std::env::set_current_dir;
use std::fs::{copy, create_dir_all, File, Permissions};
use std::os::unix::fs::{chroot, PermissionsExt};
use std::process::{exit, Command};
use anyhow::{Context, Result};
use tempfile::tempdir;

// Usage: your_docker.sh run <image> <command> <arg1> <arg2> ...
fn main() -> Result<()> {

    let args: Vec<_> = std::env::args().collect();
    let command = &args[3];
    let command_args = &args[4..];

    // 1. Create a temporary directory
    let temp_dir = tempdir()
        .expect("failed to create temporary directory");

    // 2. Copy the command binary into it
    let dest = temp_dir.path().join(command.trim_start_matches("/"));
    create_dir_all(dest.parent().unwrap())
        .expect("failed to create sub directories inside temp directory");

    copy(command, dest.clone())?;

    // 3. chroot the temporary directory
    chroot(temp_dir.path())
        .expect("failed to chroot !");

    // 4. set the current directory to '/' (root)
    set_current_dir("/")
        .expect("failed to change current directory !");

    // 5. create /dev/null file inside chroot-ed dir 666
    create_dir_all("/dev")
        .expect("failed to create /dev");

    File::create("/dev/null")
            .expect("failed to create /dev/null")
        .set_permissions(Permissions::from_mode(0o666))
            .expect("failed to set permission");

    #[cfg(target_os = "linux")]
    unsafe {
        libc::unshare(libc::CLONE_NEWPID)
    };

    // 5. Execute the binary
    let mut output = Command::new(command)
        .args(command_args)
        .spawn()
        .with_context(|| {
            format!(
                "Tried to run '{}' with arguments {:?}",
                command, command_args
            )
        })?;
    let code = output.wait()?.code().unwrap_or(1);
    drop(temp_dir);
    exit(code);

}


