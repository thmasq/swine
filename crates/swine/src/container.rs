use nix::unistd::read;
use serde::Serialize;
use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::os::fd::{BorrowedFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use krun_sys::{
    VIRGLRENDERER_NO_VIRGL, VIRGLRENDERER_RENDER_SERVER, VIRGLRENDERER_THREAD_SYNC,
    VIRGLRENDERER_USE_ASYNC_FENCE_CB, VIRGLRENDERER_USE_EGL, VIRGLRENDERER_VENUS,
};

#[derive(Serialize)]
struct KrunConfig {
    #[serde(rename = "Cmd")]
    args: Vec<String>,
    #[serde(rename = "Env")]
    envs: Vec<String>,
}

#[derive(Serialize)]
struct KrunBaseConfig {
    #[serde(rename = "Config")]
    config: KrunConfig,
}

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
        let mut rlim: nix::libc::rlimit = std::mem::zeroed();
        if nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut rlim) == 0 {
            rlim.rlim_cur = rlim.rlim_max;
            if nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &rlim) != 0 {
                eprintln!("Child: WARNING: Failed to maximize RLIMIT_NOFILE!");
            } else {
                println!("Child: Maximized RLIMIT_NOFILE to {}", rlim.rlim_max);
            }
        }
    }

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

    let fs_result = match crate::fs::isolate_filesystem(&config.profile.name, dropzone.clone()) {
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

    println!("Child: Preparing for execution...");

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

    exec_args.push(CString::new("waypipe").unwrap());
    exec_args.push(CString::new("--no-gpu").unwrap());
    exec_args.push(CString::new("--compress").unwrap());
    exec_args.push(CString::new("none").unwrap());
    exec_args.push(CString::new("-s").unwrap());
    exec_args.push(CString::new("/tmp/waypipe-local.sock").unwrap());
    exec_args.push(CString::new("server").unwrap());
    exec_args.push(CString::new("--").unwrap());

    exec_args.push(CString::new("wine").unwrap());
    exec_args.push(CString::new(exe.as_os_str().as_bytes()).unwrap());
    for arg in &args {
        exec_args.push(CString::new(arg.as_str()).unwrap());
    }

    println!("Child: Prepared to execute {:?}", exec_args);

    let _ = nix::unistd::setsid();

    println!("Child: Verifying socket exists in the sandbox:");
    let _ = std::process::Command::new("/usr/bin/ls")
        .arg("-la")
        .arg("/run/wayland-sockets")
        .status();

    println!("Child: Initializing libkrun microVM...");

    let mut guest_args = vec!["/swine-guest".to_string()];
    for arg in &exec_args {
        guest_args.push(arg.to_string_lossy().into_owned());
    }

    let krun_config = KrunBaseConfig {
        config: KrunConfig {
            args: guest_args,
            envs: vec![
                "PATH=/usr/bin:/bin".to_string(),
                "MESA_LOADER_DRIVER_OVERRIDE=zink".to_string(),
                "GALLIUM_DRIVER=zink".to_string(),
                "WINEPREFIX=/home/user/.wine".to_string(),
                "WINEDEBUG=-all".to_string(),
                "XDG_RUNTIME_DIR=/tmp".to_string(),
                "WLR_LIBINPUT_NO_DEVICES=1".to_string(),
                "DISPLAY=".to_string(),
                "HOME=/home/user".to_string(),
                "USER=root".to_string(),
                "LOGNAME=root".to_string(),
            ],
        },
    };

    let config_path = "/krun-config.json";
    let config_file = File::create(config_path).expect("Failed to create krun config file");
    serde_json::to_writer(&config_file, &krun_config).expect("Failed to write krun config");

    let ctx_id = unsafe { krun_sys::krun_create_ctx() };
    if ctx_id < 0 {
        eprintln!("Child: Failed to create krun context: {}", ctx_id);
        return 1;
    }

    let num_vcpus = config.resources.cpu_quota_percent.unwrap_or(100) / 100;
    let num_vcpus = if num_vcpus == 0 { 1 } else { num_vcpus as u8 };
    let ram_mib = config.resources.memory_limit_mb.unwrap_or(2048) as u32;

    let virgl_flags = VIRGLRENDERER_USE_EGL
        | VIRGLRENDERER_NO_VIRGL
        | VIRGLRENDERER_VENUS
        | VIRGLRENDERER_RENDER_SERVER
        | VIRGLRENDERER_THREAD_SYNC
        | VIRGLRENDERER_USE_ASYNC_FENCE_CB;

    let vram_shm_mib = 2048;

    unsafe {
        krun_sys::krun_set_vm_config(ctx_id as u32, num_vcpus, ram_mib);
        krun_sys::krun_set_gpu_options2(
            ctx_id as u32,
            virgl_flags,
            (vram_shm_mib as u64) * 1024 * 1024,
        );
        krun_sys::krun_set_root(ctx_id as u32, c"/".as_ptr());
        krun_sys::krun_add_vsock(ctx_id as u32, 3); // 3 = INET | UNIX

        let host_waypipe_sock = std::ffi::CString::new("/run/wayland-sockets/wayland-0").unwrap();
        krun_sys::krun_add_vsock_port(ctx_id as u32, 10000, host_waypipe_sock.as_ptr());

        let env_str = std::ffi::CString::new(format!("KRUN_CONFIG={}", config_path)).unwrap();
        let env_ptrs: Vec<*const libc::c_char> = vec![env_str.as_ptr(), std::ptr::null()];
        krun_sys::krun_set_env(ctx_id as u32, env_ptrs.as_ptr());
    }

    println!("Child: Launching KVM microVM...");

    let err = unsafe { krun_sys::krun_start_enter(ctx_id as u32) };

    if err < 0 {
        eprintln!("Child: krun_start_enter failed: {}", err);
        return 1;
    }

    0
}
