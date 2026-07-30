use anyhow::Result;
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
            let _ = std::fs::remove_file("/tmp/waypipe-local.sock");
            if let Ok(listener) = std::os::unix::net::UnixListener::bind("/tmp/waypipe-local.sock")
            {
                for stream in listener.incoming() {
                    if let Ok(mut client) = stream {
                        let fd = unsafe {
                            nix::libc::socket(nix::libc::AF_VSOCK, nix::libc::SOCK_STREAM, 0)
                        };
                        if fd < 0 {
                            eprintln!("swine-guest proxy: Failed to create vsock socket");
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
                                "swine-guest proxy: Failed to connect to host waypipe via VSOCK"
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

    // TODO: Inner Sandbox Lockdown

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

    // TODO?: (Future Work) Forwarding Sockets

    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp");
    }

    println!("swine-guest: Executing {:?}...", exec_args);
    let err = execvp(&exec_bin, &exec_args).unwrap_err();

    eprintln!("swine-guest: execvp failed: {:?}", err);
    std::process::exit(1);
}
