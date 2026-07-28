use anyhow::{Context, Result};
use caps::CapSet;
use libseccomp::{ScmpAction, ScmpArgCompare, ScmpCompareOp, ScmpFilterContext, ScmpSyscall};
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

        ctx.add_arch(libseccomp::ScmpArch::X86)?;
        ctx.add_arch(libseccomp::ScmpArch::X32)?;

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
            let syscall = ScmpSyscall::from_name(syscall_name)
                .with_context(|| format!("Failed to resolve syscall '{}'", syscall_name))?;

            let errno: i32 = if *syscall_name == "clone3" {
                nix::libc::ENOSYS
            } else {
                nix::libc::EPERM
            };

            ctx.add_rule(ScmpAction::Errno(errno), syscall)
                .with_context(|| format!("Failed to add seccomp rule for '{}'", syscall_name))?;
        }

        // SOCKET: Block AF_ALG (38) and AF_VSOCK (40)
        // socket(domain, type, protocol). 'domain' is argument 0.
        if let Ok(socket_syscall) = ScmpSyscall::from_name("socket") {
            ctx.add_rule_conditional(
                ScmpAction::Errno(nix::libc::EPERM),
                socket_syscall,
                &[ScmpArgCompare::new(0, ScmpCompareOp::Equal, 38)], // AF_ALG
            )?;
            ctx.add_rule_conditional(
                ScmpAction::Errno(nix::libc::EPERM),
                socket_syscall,
                &[ScmpArgCompare::new(0, ScmpCompareOp::Equal, 40)], // AF_VSOCK
            )?;
        }

        // CLONE: Block creation of namespaces, but allow normal multithreading
        // clone(flags, ...). 'flags' is argument 0.
        if let Ok(clone_syscall) = ScmpSyscall::from_name("clone") {
            let forbidden_clone_flags: [u64; 8] = [
                0x00000080, // CLONE_NEWTIME
                0x02000000, // CLONE_NEWCGROUP
                0x00020000, // CLONE_NEWNS
                0x04000000, // CLONE_NEWUTS
                0x08000000, // CLONE_NEWIPC
                0x10000000, // CLONE_NEWUSER
                0x20000000, // CLONE_NEWPID
                0x40000000, // CLONE_NEWNET
            ];
            for flag in forbidden_clone_flags.iter() {
                ctx.add_rule_conditional(
                    ScmpAction::Errno(nix::libc::EPERM),
                    clone_syscall,
                    &[ScmpArgCompare::new(
                        0,
                        ScmpCompareOp::MaskedEqual(*flag),
                        *flag,
                    )],
                )?;
            }
        }

        // IOCTL: Explicitly block TIOCSTI (0x5412) to prevent terminal injection
        // ioctl(fd, request, ...). 'request' is argument 1.
        if let Ok(ioctl_syscall) = ScmpSyscall::from_name("ioctl") {
            ctx.add_rule_conditional(
                ScmpAction::Errno(nix::libc::EPERM),
                ioctl_syscall,
                &[ScmpArgCompare::new(1, ScmpCompareOp::Equal, 0x5412)], // TIOCSTI
            )?;
        }

        ctx.load()?;
        println!("Child: Loaded Seccomp-BPF filter (with conditional argument blocking).");
    }

    Ok(())
}
