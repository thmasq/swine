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
static SIGNAL_RECEIVED: AtomicI32 = AtomicI32::new(0);

extern "C" fn handle_signal(sig: libc::c_int) {
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        let _ = signal::kill(nix::unistd::Pid::from_raw(pid), Signal::SIGKILL);
    }
    SIGNAL_RECEIVED.store(sig, Ordering::SeqCst);
}

struct ChildCleanup {
    pid: nix::unistd::Pid,
    cgroup_path: Option<PathBuf>,
    reaped: bool,
}

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        if !self.reaped {
            if let Some(cg) = &self.cgroup_path {
                let _ = std::fs::write(cg.join("cgroup.kill"), "1\n");
            }
            let _ = signal::kill(self.pid, Signal::SIGKILL);
            let _ = waitpid(self.pid, None);
        }

        if let Some(cg) = &self.cgroup_path {
            if cg.exists() {
                let mut attempts = 0;
                while std::fs::remove_dir(cg).is_err() && attempts < 5 {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                if cg.exists() {
                    eprintln!("Supervisor: Failed to remove cgroup at {:?}", cg);
                } else {
                    println!("Supervisor: Cleaned up cgroup at {:?}", cg);
                }
            }
        }
    }
}

pub fn start_sandbox(
    config: &crate::config::Config,
    exe: PathBuf,
    args: Vec<String>,
    dropzone: Option<PathBuf>,
) -> Result<std::thread::JoinHandle<()>> {
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

    let handler = SigHandler::Handler(handle_signal);
    let action = SigAction::new(handler, SaFlags::SA_RESTART, SigSet::empty());
    unsafe {
        let _ = signal::sigaction(Signal::SIGINT, &action);
        let _ = signal::sigaction(Signal::SIGTERM, &action);
    }

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

    let mut cleanup = ChildCleanup {
        pid: child_pid,
        cgroup_path: None,
        reaped: false,
    };

    let host_uid = nix::unistd::getuid();
    let host_gid = nix::unistd::getgid();

    let uid_map_path = format!("/proc/{}/uid_map", child_pid);
    let gid_map_path = format!("/proc/{}/gid_map", child_pid);

    let uid_map = format!("0 {} 1\n", host_uid);
    let gid_map = format!("0 {} 1\n", host_gid);

    std::fs::write(&uid_map_path, uid_map).context("Failed to write uid_map")?;

    let setgroups_path = format!("/proc/{}/setgroups", child_pid);
    std::fs::write(&setgroups_path, "deny\n").context("Failed to write setgroups")?;
    std::fs::write(&gid_map_path, gid_map).context("Failed to write gid_map")?;

    let cgroup_path = crate::cgroup::enforce_limits(&config.resources, child_pid)?;
    cleanup.cgroup_path = Some(cgroup_path.clone());

    println!("Supervisor: Wrote UID/GID maps and Cgroups. Unblocking child...");

    setup_write_file
        .write_all(&[0x00])
        .context("Failed to signal child")?;
    drop(setup_write_file);

    let handle = std::thread::spawn(move || {
        let mut fuse_pid_tracked = None;
        let mut child_exec_failed = false;

        loop {
            let mut type_buf = [0u8; 1];
            match error_read_file.read_exact(&mut type_buf) {
                Ok(_) => {
                    let mut data_buf = [0u8; 4];
                    if let Ok(_) = error_read_file.read_exact(&mut data_buf) {
                        if type_buf[0] == 1 {
                            let f_pid = u32::from_le_bytes(data_buf);
                            println!(
                                "Supervisor: Tracking fuse-overlayfs daemon (Namespace PID: {})",
                                f_pid
                            );
                            fuse_pid_tracked = Some(f_pid);
                        } else if type_buf[0] == 2 {
                            let exit_code = i32::from_le_bytes(data_buf);
                            eprintln!("Supervisor: Child handoff failed with errno: {}", exit_code);
                            child_exec_failed = true;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    break;
                }
                Err(_) => break,
            }
        }

        let status = match waitpid(child_pid, None) {
            Ok(s) => s,
            Err(e) if e == nix::errno::Errno::EINTR => {
                waitpid(child_pid, None).expect("Failed to wait for child after EINTR")
            }
            Err(e) => {
                eprintln!("Failed to wait for child: {}", e);
                return;
            }
        };

        if let Some(pid) = fuse_pid_tracked {
            println!(
                "Supervisor: FUSE daemon {} safely reaped by namespace destruction.",
                pid
            );
        }

        CHILD_PID.store(0, Ordering::SeqCst);
        cleanup.reaped = true;

        if !child_exec_failed {
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

        let sig = SIGNAL_RECEIVED.load(Ordering::SeqCst);
        if sig != 0 {
            drop(cleanup);
            std::process::exit(128 + sig);
        }
    });

    Ok(handle)
}
