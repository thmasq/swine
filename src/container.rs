use nix::unistd::{execvp, read};
use std::ffi::CString;
use std::io::Write;
use std::os::fd::{BorrowedFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

pub fn entrypoint(
    setup_read_fd: RawFd,
    error_write_fd: RawFd,
    config: crate::config::Config,
    exe: PathBuf,
    args: Vec<String>,
    dropzone: Option<PathBuf>,
) -> isize {
    println!("Child: Starting namespace initialization...");

    unsafe {
        nix::libc::prctl(
            nix::libc::PR_SET_PDEATHSIG,
            nix::libc::SIGKILL as libc::c_ulong,
        );
    }

    let setup_fd = unsafe { BorrowedFd::borrow_raw(setup_read_fd) };

    let mut buf = [0u8; 1];
    match read(setup_fd, &mut buf) {
        Ok(1) => {
            if buf[0] != 0x00 {
                return 1;
            }
        }
        Ok(_) => return 1,
        Err(_) => return 1,
    }

    let uid = nix::unistd::getuid();
    if uid.as_raw() != 0 {
        eprintln!("Child: UID mapping failed! Expected 0, got {}", uid);
        return 1;
    }

    let mut wayland_fd_env = None;
    if config.graphics.gamescope {
        if let Ok(xdg_runtime) = std::env::var("XDG_RUNTIME_DIR") {
            let host_socket =
                std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
            let socket_path = std::path::Path::new(&xdg_runtime).join(host_socket);

            if let Ok(stream) = std::os::unix::net::UnixStream::connect(&socket_path) {
                use std::os::unix::io::IntoRawFd;
                let fd = stream.into_raw_fd();

                unsafe {
                    let flags = nix::libc::fcntl(fd, nix::libc::F_GETFD);
                    if flags >= 0 {
                        nix::libc::fcntl(fd, nix::libc::F_SETFD, flags & !nix::libc::FD_CLOEXEC);
                    }
                }
                wayland_fd_env = Some(fd.to_string());
                println!("Child: Extracted host Wayland FD {} via UnixStream.", fd);
            } else {
                eprintln!("Child: WARNING: Failed to connect to host Wayland socket.");
            }
        }
    }

    let fs_result = match crate::fs::isolate_filesystem(&config.profile.name, dropzone) {
        Ok(res) => res,
        Err(e) => {
            eprintln!("Child: Filesystem setup failed: {:?}", e);
            return 1;
        }
    };

    if let Some(pid) = fs_result.fuse_pid {
        let mut err_pipe = unsafe { std::fs::File::from_raw_fd(error_write_fd) };
        let mut msg = vec![1u8];
        msg.extend_from_slice(&pid.to_le_bytes());
        let _ = err_pipe.write_all(&msg);

        std::mem::forget(err_pipe);
    }

    if !config.network.allow_network {
        println!("Child: Bringing up loopback interface (lo)...");
        match std::process::Command::new("/usr/bin/ip")
            .args(["link", "set", "lo", "up"])
            .output()
        {
            Ok(out) => {
                if !out.status.success() {
                    eprintln!(
                        "Child: WARNING: ip link failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            }
            Err(e) => eprintln!("Child: WARNING: Failed to execute ip command: {}", e),
        }
    }

    if let Err(e) = crate::security::lockdown(&config.sandbox) {
        eprintln!("Child: Security lockdown failed: {:?}", e);
        return 1;
    }

    println!("Child: Testing Seccomp filter (calling ptrace)...");
    match nix::sys::ptrace::traceme() {
        Ok(_) => {
            eprintln!("Child: WARNING: ptrace succeeded! Seccomp filter failed.");
            return 1;
        }
        Err(e) => {
            println!("Child: Seccomp successfully blocked ptrace with: {}", e);
        }
    }

    println!("Child: Preparing for execve...");

    if let Some(parent) = exe.parent() {
        if parent.as_os_str() != "" {
            if let Err(e) = nix::unistd::chdir(parent) {
                eprintln!("Child: WARNING: Failed to chdir to {:?}: {}", parent, e);
            } else {
                println!("Child: Changed working directory to {:?}", parent);
            }
        }
    }

    let _ = nix::unistd::sethostname("swine");

    unsafe {
        nix::libc::clearenv();
    }

    unsafe {
        std::env::set_var("PATH", "/usr/bin:/usr/local/bin:/bin:/sbin");
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");

        if let Some(fd_str) = wayland_fd_env {
            std::env::set_var("WAYLAND_SOCKET", fd_str);
        } else {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        }

        std::env::set_var("HOME", "/home/user");
        std::env::set_var("USER", "root");
        std::env::set_var("LOGNAME", "root");

        let reserved_keys = [
            "PATH",
            "XDG_RUNTIME_DIR",
            "WAYLAND_SOCKET",
            "WAYLAND_DISPLAY",
            "HOME",
            "USER",
            "LOGNAME",
        ];

        for (k, v) in &config.environment {
            if reserved_keys.contains(&k.as_str()) {
                eprintln!(
                    "Child: WARNING: Profile is overriding reserved environment variable '{}'",
                    k
                );
            }
            std::env::set_var(k, v);
        }
    }

    let mut exec_args = Vec::new();
    let exec_bin_name: String;

    if config.graphics.gamescope {
        exec_bin_name = "gamescope".to_string();
        exec_args.push(CString::new("gamescope").unwrap());
        exec_args.push(CString::new("--backend").unwrap());
        exec_args.push(CString::new("wayland").unwrap());

        if let Some(res) = &config.graphics.resolution {
            let parts: Vec<&str> = res.split('x').collect();
            if parts.len() == 2 {
                exec_args.push(CString::new("-W").unwrap());
                exec_args.push(CString::new(parts[0]).unwrap());
                exec_args.push(CString::new("-H").unwrap());
                exec_args.push(CString::new(parts[1]).unwrap());
            }
        }

        if let Some(scaler) = &config.graphics.scaler {
            let scaler_str = match scaler {
                crate::config::Scaler::Auto => "auto",
                crate::config::Scaler::Integer => "integer",
                crate::config::Scaler::Fit => "fit",
                crate::config::Scaler::Fill => "fill",
                crate::config::Scaler::Stretch => "stretch",
            };
            exec_args.push(CString::new("-S").unwrap());
            exec_args.push(CString::new(scaler_str).unwrap());
        }

        if let Some(filter) = &config.graphics.filter {
            let filter_str = match filter {
                crate::config::Filter::Linear => "linear",
                crate::config::Filter::Nearest => "nearest",
                crate::config::Filter::Fsr => "fsr",
                crate::config::Filter::Nis => "nis",
                crate::config::Filter::Pixel => "pixel",
            };
            exec_args.push(CString::new("-F").unwrap());
            exec_args.push(CString::new(filter_str).unwrap());
        }

        for arg in &config.graphics.gamescope_args {
            exec_args.push(CString::new(arg.as_str()).unwrap());
        }

        exec_args.push(CString::new("--").unwrap());
        exec_args.push(CString::new("wine").unwrap());
        exec_args.push(CString::new(exe.as_os_str().as_bytes()).unwrap());
    } else {
        exec_bin_name = "wine".to_string();
        exec_args.push(CString::new("wine").unwrap());
        exec_args.push(CString::new(exe.as_os_str().as_bytes()).unwrap());
    }

    for arg in args {
        exec_args.push(CString::new(arg).unwrap());
    }

    let exec_bin = CString::new(exec_bin_name).unwrap();

    println!("Child: Handing off execution to {:?}", exec_args);

    let _ = nix::unistd::setsid();

    let e = execvp(&exec_bin, &exec_args).unwrap_err();
    eprintln!("Child: execvp failed: {:?}", e);

    let mut err_pipe = unsafe { std::fs::File::from_raw_fd(error_write_fd) };
    let err_code = e as i32;
    let mut msg = vec![2u8];
    msg.extend_from_slice(&err_code.to_le_bytes());
    let _ = err_pipe.write_all(&msg);

    1
}
