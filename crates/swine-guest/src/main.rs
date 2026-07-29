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
            c"/run".as_ptr(),
            c"tmpfs".as_ptr(),
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
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
    }

    let socat_cmd = "socat UNIX-LISTEN:/tmp/wayland-0,fork VSOCK-CONNECT:2:50000";
    if let Ok(_) = std::process::Command::new("sh")
        .arg("-c")
        .arg(socat_cmd)
        .spawn()
    {
        println!("swine-guest: Started socat vsock bridge for Wayland.");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("swine-guest: Executing {:?}...", exec_args);
    let err = execvp(&exec_bin, &exec_args).unwrap_err();

    eprintln!("swine-guest: execvp failed: {:?}", err);
    std::process::exit(1);
}
