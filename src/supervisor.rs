use anyhow::{Context, Result};
use nix::fcntl::OFlag;
use nix::sched::{CloneFlags, clone};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::wait::waitpid;
use nix::unistd::pipe2;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};

static CHILD_PID: AtomicI32 = AtomicI32::new(0);

extern "C" fn handle_signal(sig: libc::c_int) {
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        let _ = signal::kill(nix::unistd::Pid::from_raw(pid), Signal::SIGKILL);
    }
    unsafe { nix::libc::_exit(128 + sig) };
}

pub fn start_sandbox(
    config: &crate::config::Config,
    exe: PathBuf,
    args: Vec<String>,
    dropzone: Option<PathBuf>,
) -> Result<()> {
    let (setup_read, setup_write) = pipe2(OFlag::O_CLOEXEC)?;
    let (error_read, error_write) = pipe2(OFlag::O_CLOEXEC)?;

    let setup_read_fd = setup_read.as_raw_fd();
    let error_write_fd = error_write.as_raw_fd();

    let mut setup_write_file: std::fs::File = setup_write.into();
    let mut error_read_file: std::fs::File = error_read.into();

    let mut flags = CloneFlags::CLONE_NEWUSER
        | CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWIPC
        | CloneFlags::CLONE_NEWUTS;

    if !config.network.allow_network {
        flags |= CloneFlags::CLONE_NEWNET;
    }

    let mut stack = vec![0u8; 4 * 1024 * 1024];

    let config_clone = config.clone();
    let exe_clone = exe.clone();
    let args_clone = args.clone();
    let dropzone_clone = dropzone.clone();

    let child_pid = unsafe {
        clone(
            Box::new(move || {
                crate::container::entrypoint(
                    setup_read_fd,
                    error_write_fd,
                    config_clone.clone(),
                    exe_clone.clone(),
                    args_clone.clone(),
                    dropzone_clone.clone(),
                )
            }),
            &mut stack,
            flags,
            Some(libc::SIGCHLD),
        )
    }
    .context("Failed to clone child process")?;

    drop(setup_read);
    drop(error_write);

    println!("Supervisor: Launched child with PID {}", child_pid);

    CHILD_PID.store(child_pid.as_raw(), Ordering::SeqCst);

    let handler = SigHandler::Handler(handle_signal);
    let action = SigAction::new(handler, SaFlags::SA_RESTART, SigSet::empty());
    unsafe {
        let _ = signal::sigaction(Signal::SIGINT, &action);
        let _ = signal::sigaction(Signal::SIGTERM, &action);
    }

    let host_uid = nix::unistd::getuid();
    let host_gid = nix::unistd::getgid();

    let uid_map_path = format!("/proc/{}/uid_map", child_pid);
    let gid_map_path = format!("/proc/{}/gid_map", child_pid);

    let uid_map = format!("0 {} 1\n", host_uid);
    let gid_map = format!("0 {} 1\n", host_gid);

    std::fs::write(&uid_map_path, uid_map).context("Failed to write uid_map")?;

    let setgroups_path = format!("/proc/{}/setgroups", child_pid);
    let _ = std::fs::write(&setgroups_path, "deny\n");
    std::fs::write(&gid_map_path, gid_map).context("Failed to write gid_map")?;

    crate::cgroup::enforce_limits(&config.resources, child_pid)?;

    println!("Supervisor: Wrote UID/GID maps and Cgroups. Unblocking child...");

    setup_write_file
        .write_all(&[0x00])
        .context("Failed to signal child")?;
    drop(setup_write_file);

    let status = waitpid(child_pid, None).context("Failed to wait for child")?;

    let mut err_buf = [0u8; 4];

    if let Ok(4) = error_read_file.read(&mut err_buf) {
        let exit_code = i32::from_le_bytes(err_buf);
        eprintln!("Supervisor: Child handoff failed with errno: {}", exit_code);
    } else {
        match status {
            nix::sys::wait::WaitStatus::Exited(_, code) => {
                if code == 0 {
                    println!("Supervisor: Child process exited cleanly.");
                } else {
                    eprintln!("Supervisor: Child process exited with code {}", code);
                }
            }
            nix::sys::wait::WaitStatus::Signaled(_, signal, core_dumped) => {
                eprintln!(
                    "Supervisor: Child process was killed by signal {} (core dumped: {})",
                    signal, core_dumped
                );
            }
            _ => {
                eprintln!(
                    "Supervisor: Child process terminated with unknown status: {:?}",
                    status
                );
            }
        }
    }

    Ok(())
}
