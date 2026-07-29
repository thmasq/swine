use anyhow::Result;
use nix::unistd::execvp;
use std::ffi::CString;

fn main() -> Result<()> {
    println!("swine-guest: Successfully booted inside the microVM!");

    // 1. (Optional) Inner Sandbox Lockdown
    // You could import your `caps` and `libseccomp` logic here to lock down
    // the guest kernel, preventing the Windows application from using `ptrace`
    // or namespace syscalls *inside* the virtual machine.

    // 2. Fetch the target executable passed from the host
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        anyhow::bail!("swine-guest: No executable provided to launch");
    }

    let target_bin = &args[1];
    println!("swine-guest: Preparing to execute {}", target_bin);

    // 3. Prepare arguments for execvp
    let exec_bin = CString::new(target_bin.as_str()).unwrap();
    let mut exec_args = Vec::new();
    for arg in &args[1..] {
        exec_args.push(CString::new(arg.as_str()).unwrap());
    }

    // 4. (Future Work) Forwarding Sockets
    // Eventually, you will want to listen on UNIX sockets here inside the guest
    // (e.g., /run/user/1000/wayland-0) and proxy the bytes over VSOCK to the host.

    // 5. Hand over execution to Wine / Gamescope!
    println!("swine-guest: Executing {:?}...", exec_args);
    let err = execvp(&exec_bin, &exec_args).unwrap_err();

    eprintln!("swine-guest: execvp failed: {:?}", err);
    std::process::exit(1);
}
