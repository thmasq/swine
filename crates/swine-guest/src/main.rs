use anyhow::Result;
use caps::CapSet;
use libseccomp::{ScmpAction, ScmpArgCompare, ScmpCompareOp, ScmpFilterContext, ScmpSyscall};
use nix::unistd::execvp;
use std::ffi::CString;

fn main() -> Result<()> {
    println!("swine-guest: Successfully booted inside the microVM!");

    unsafe {
        nix::libc::mount(
            c"devtmpfs".as_ptr(),
            c"/dev".as_ptr(),
            c"devtmpfs".as_ptr(),
            0,
            std::ptr::null(),
        );
        nix::libc::mount(
            c"proc".as_ptr(),
            c"/proc".as_ptr(),
            c"proc".as_ptr(),
            0,
            std::ptr::null(),
        );
        nix::libc::mount(
            c"sysfs".as_ptr(),
            c"/sys".as_ptr(),
            c"sysfs".as_ptr(),
            0,
            std::ptr::null(),
        );
        nix::libc::mount(
            c"tmpfs".as_ptr(),
            c"/tmp".as_ptr(),
            c"tmpfs".as_ptr(),
            0,
            std::ptr::null(),
        );
    }

    let _ = std::fs::create_dir_all("/tmp/swine-sockets");
    let _ = std::env::set_current_dir("/workspace");

    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Child) => {
            std::thread::spawn(|| {
                let _ = std::fs::create_dir_all("/tmp/.X11-unix");
                let _ = std::fs::remove_file("/tmp/.X11-unix/X0");
                if let Ok(listener) = std::os::unix::net::UnixListener::bind("/tmp/.X11-unix/X0") {
                    for stream in listener.incoming() {
                        if let Ok(mut client) = stream {
                            let fd = unsafe {
                                nix::libc::socket(nix::libc::AF_VSOCK, nix::libc::SOCK_STREAM, 0)
                            };
                            if fd < 0 {
                                eprintln!(
                                    "swine-guest proxy: Failed to create vsock socket for X11"
                                );
                                continue;
                            }

                            let mut addr: nix::libc::sockaddr_vm = unsafe { std::mem::zeroed() };
                            addr.svm_family = nix::libc::AF_VSOCK as nix::libc::sa_family_t;
                            addr.svm_cid = nix::libc::VMADDR_CID_HOST;
                            addr.svm_port = 10001;

                            let res = unsafe {
                                nix::libc::connect(
                                    fd,
                                    &addr as *const _ as *const nix::libc::sockaddr,
                                    std::mem::size_of_val(&addr) as u32,
                                )
                            };

                            if res < 0 {
                                eprintln!(
                                    "swine-guest proxy: Failed to connect to host X11 via VSOCK"
                                );
                                unsafe { nix::libc::close(fd) };
                                continue;
                            }

                            println!("swine-guest proxy: Successfully connected to X11 via VSOCK");

                            use std::os::unix::io::FromRawFd;
                            let mut server = unsafe { std::net::TcpStream::from_raw_fd(fd) };

                            let mut client_clone = client.try_clone().unwrap();
                            let mut server_clone = server.try_clone().unwrap();

                            std::thread::spawn(move || {
                                let _ = std::io::copy(&mut client, &mut server);
                            });
                            std::thread::spawn(move || {
                                let _ = std::io::copy(&mut server_clone, &mut client_clone);
                            });
                        }
                    }
                }
            });

            let _ = std::fs::remove_file("/tmp/waypipe-local.sock");
            if let Ok(listener) = std::os::unix::net::UnixListener::bind("/tmp/waypipe-local.sock")
            {
                for stream in listener.incoming() {
                    if let Ok(mut client) = stream {
                        let fd = unsafe {
                            nix::libc::socket(nix::libc::AF_VSOCK, nix::libc::SOCK_STREAM, 0)
                        };
                        if fd < 0 {
                            eprintln!(
                                "swine-guest proxy: Failed to create vsock socket for Wayland"
                            );
                            continue;
                        }

                        let mut addr: nix::libc::sockaddr_vm = unsafe { std::mem::zeroed() };
                        addr.svm_family = nix::libc::AF_VSOCK as nix::libc::sa_family_t;
                        addr.svm_cid = nix::libc::VMADDR_CID_HOST;
                        addr.svm_port = 10000;

                        let res = unsafe {
                            nix::libc::connect(
                                fd,
                                &addr as *const _ as *const nix::libc::sockaddr,
                                std::mem::size_of_val(&addr) as u32,
                            )
                        };

                        if res < 0 {
                            eprintln!(
                                "swine-guest proxy: Failed to connect to host Wayland via VSOCK"
                            );
                            unsafe { nix::libc::close(fd) };
                            continue;
                        }

                        println!(
                            "swine-guest proxy: Successfully connected to wayland-0 via VSOCK"
                        );

                        use std::os::unix::io::FromRawFd;
                        let mut server = unsafe { std::net::TcpStream::from_raw_fd(fd) };

                        let mut client_clone = client.try_clone().unwrap();
                        let mut server_clone = server.try_clone().unwrap();

                        std::thread::spawn(move || {
                            let _ = std::io::copy(&mut client, &mut server);
                        });
                        std::thread::spawn(move || {
                            let _ = std::io::copy(&mut server_clone, &mut client_clone);
                        });
                    }
                }
            }
            std::process::exit(0);
        }
        Ok(nix::unistd::ForkResult::Parent { child: _ }) => {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(e) => {
            eprintln!("Failed to fork proxy process: {}", e);
        }
    }

    if let Ok(_) = std::process::Command::new("/usr/bin/seatd")
        .arg("-g")
        .arg("root")
        .arg("-n")
        .arg("/run/seatd.sock")
        .spawn()
    {
        println!("swine-guest: Started seatd for DRM session management.");
        unsafe {
            std::env::set_var("SEATD_SOCK", "/run/seatd.sock");
            std::env::set_var("XDG_RUNTIME_DIR", "/tmp");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    } else {
        eprintln!("swine-guest: Warning: failed to start seatd.");
    }

    if let Ok(_) = std::process::Command::new("/usr/lib/systemd/systemd-udevd").spawn() {
        println!("swine-guest: Started systemd-udevd to populate hardware nodes.");
        std::thread::sleep(std::time::Duration::from_millis(200));
    } else {
        eprintln!(
            "swine-guest: Warning: failed to start systemd-udevd. Vulkan might fail if /dev/dri is not ready."
        );
    }

    if let Err(e) = inner_lockdown() {
        eprintln!("swine-guest: Inner security lockdown failed: {:?}", e);
    }

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        anyhow::bail!("swine-guest: No executable provided to launch");
    }

    let target_bin = &args[1];
    println!("swine-guest: Preparing to execute {}", target_bin);

    let exec_bin = CString::new(target_bin.as_str()).unwrap();
    let mut exec_args = Vec::new();
    for arg in &args[1..] {
        exec_args.push(CString::new(arg.as_str()).unwrap());
    }

    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp");
    }

    println!("swine-guest: Executing {:?}...", exec_args);
    let err = execvp(&exec_bin, &exec_args).unwrap_err();

    eprintln!("swine-guest: execvp failed: {:?}", err);
    std::process::exit(1);
}

fn inner_lockdown() -> Result<()> {
    println!("swine-guest: Dropping capabilities inside the microVM...");
    caps::clear(None, CapSet::Bounding)?;
    caps::clear(None, CapSet::Effective)?;
    caps::clear(None, CapSet::Inheritable)?;
    caps::clear(None, CapSet::Permitted)?;

    unsafe {
        nix::libc::prctl(nix::libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    }

    println!("swine-guest: Applying inner Seccomp filter...");
    let mut ctx = ScmpFilterContext::new(ScmpAction::Allow)?;

    ctx.add_arch(libseccomp::ScmpArch::X86)?;
    ctx.add_arch(libseccomp::ScmpArch::X8664)?;

    let blocked_syscalls = [
        "ptrace",
        "process_vm_readv",
        "process_vm_writev",
        "vmsplice",
        "userfaultfd",
        "init_module",
        "finit_module",
        "delete_module",
        "kexec_load",
        "kexec_file_load",
        "bpf",
        "perf_event_open",
        "unshare",
        "setns",
        "clone3",
        "mount",
        "umount2",
        "pivot_root",
        "keyctl",
        "add_key",
        "request_key",
        "io_uring_setup",
        "io_uring_enter",
        "io_uring_register",
        "kcmp",
        "fsopen",
        "fsconfig",
        "fsmount",
        "fspick",
        "move_mount",
        "open_tree",
        "mount_setattr",
        "statmount",
        "listmount",
        "open_by_handle_at",
        "name_to_handle_at",
        "pidfd_getfd",
        "process_madvise",
        "process_mrelease",
        "ioperm",
        "iopl",
        "personality",
        "syslog",
        "chroot",
        "create_module",
        "query_module",
        "get_kernel_syms",
        "uselib",
        "_sysctl",
        "sysfs",
        "ustat",
        "nfsservctl",
        "quotactl",
        "reboot",
        "clock_adjtime",
        "clock_settime",
        "settimeofday",
        "stime",
        "get_mempolicy",
        "set_mempolicy",
        "mbind",
        "move_pages",
        "vm86",
        "vm86old",
        "lookup_dcookie",
        "acct",
        "socketcall",
        "swapon",
        "swapoff",
    ];

    for syscall_name in &blocked_syscalls {
        if let Ok(syscall) = ScmpSyscall::from_name(syscall_name) {
            let errno: i32 = if *syscall_name == "clone3" {
                nix::libc::ENOSYS
            } else {
                nix::libc::EPERM
            };

            let _ = ctx.add_rule(ScmpAction::Errno(errno), syscall);
        }
    }

    // SOCKET: Block AF_ALG (38) and AF_VSOCK (40)
    if let Ok(socket_syscall) = ScmpSyscall::from_name("socket") {
        let _ = ctx.add_rule_conditional(
            ScmpAction::Errno(nix::libc::EPERM),
            socket_syscall,
            &[ScmpArgCompare::new(0, ScmpCompareOp::Equal, 38)],
        );
        let _ = ctx.add_rule_conditional(
            ScmpAction::Errno(nix::libc::EPERM),
            socket_syscall,
            &[ScmpArgCompare::new(0, ScmpCompareOp::Equal, 40)],
        );
    }

    // CLONE: Block creation of namespaces, but allow normal multithreading
    if let Ok(clone_syscall) = ScmpSyscall::from_name("clone") {
        let forbidden_clone_flags: [u64; 8] = [
            0x00000080, 0x02000000, 0x00020000, 0x04000000, 0x08000000, 0x10000000, 0x20000000,
            0x40000000,
        ];
        for flag in forbidden_clone_flags.iter() {
            let _ = ctx.add_rule_conditional(
                ScmpAction::Errno(nix::libc::EPERM),
                clone_syscall,
                &[ScmpArgCompare::new(
                    0,
                    ScmpCompareOp::MaskedEqual(*flag),
                    *flag,
                )],
            );
        }
    }

    // IOCTL: Block TIOCSTI (Terminal Injection)
    if let Ok(ioctl_syscall) = ScmpSyscall::from_name("ioctl") {
        let _ = ctx.add_rule_conditional(
            ScmpAction::Errno(nix::libc::EPERM),
            ioctl_syscall,
            &[ScmpArgCompare::new(1, ScmpCompareOp::Equal, 0x5412)],
        );
    }

    ctx.load()?;
    Ok(())
}
