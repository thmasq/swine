use anyhow::{Context, Result};
use caps::CapSet;
use libseccomp::{ScmpAction, ScmpFilterContext, ScmpSyscall};
use std::fs;

pub fn lockdown(config: &crate::config::SandboxConfig) -> Result<()> {
    println!("Child: Initiating security lockdown...");

    fs::create_dir_all("/etc").context("Failed to create /etc")?;
    fs::write(
        "/etc/passwd",
        "root:x:0:0:root:/home/user:/bin/sh\nplayer:x:1000:1000:player:/home/user:/bin/sh\n",
    )
    .context("Failed to write /etc/passwd")?;

    fs::write("/etc/hostname", "swine\n").ok();
    fs::write(
        "/etc/hosts",
        "127.0.0.1 localhost swine\n::1 localhost swine\n",
    )
    .ok();
    fs::write("/etc/machine-id", "00000000000000000000000000000000\n").ok();

    if config.drop_all_caps {
        caps::clear(None, CapSet::Bounding)?;
        caps::clear(None, CapSet::Effective)?;
        caps::clear(None, CapSet::Inheritable)?;
        caps::clear(None, CapSet::Permitted)?;
        println!("Child: Dropped all Linux capabilities.");
    }

    if config.seccomp_strict {
        let mut ctx = ScmpFilterContext::new(ScmpAction::Allow)?;

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

        ctx.load()?;
        println!("Child: Loaded Seccomp-BPF filter.");
    }

    Ok(())
}
