use anyhow::{Context, Result};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::unistd::{chdir, pivot_root};
use std::fs;
use std::path::{Path, PathBuf};

pub fn isolate_filesystem(profile_name: &str, dropzone: Option<PathBuf>) -> Result<()> {
    println!("Child: Assembling isolated filesystem...");
    let none: Option<&str> = None;

    mount(none, "/", none, MsFlags::MS_PRIVATE | MsFlags::MS_REC, none)
        .context("Failed to remount / as private")?;

    let staging = Path::new("/tmp/swine_root");
    if !staging.exists() {
        fs::create_dir_all(staging).context("Failed to create staging dir")?;
    }

    mount(
        Some("tmpfs"),
        staging,
        Some("tmpfs"),
        MsFlags::empty(),
        Some("mode=755"),
    )
    .context("Failed to mount tmpfs on staging")?;

    let dirs = [
        "usr",
        "bin",
        "sbin",
        "lib",
        "lib64",
        "usr/lib32",
        "dev",
        "dev/dri",
        "tmp",
        "proc",
        "sys",
        "home/user/.wine",
    ];
    for dir in &dirs {
        let p = staging.join(dir);
        fs::create_dir_all(&p).with_context(|| format!("Failed to create {:?}", p))?;
    }

    let bind_dirs = ["/usr", "/lib", "/lib64", "/usr/lib32", "/bin", "/sbin"];
    for b in &bind_dirs {
        let target = staging.join(b.trim_start_matches('/'));
        if Path::new(b).exists() {
            mount(
                Some(*b),
                &target,
                none,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                none,
            )?;
            mount(
                none,
                &target,
                none,
                MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY | MsFlags::MS_REC,
                none,
            )?;
        }
    }

    if Path::new("/dev/dri").exists() {
        mount(
            Some("/dev/dri"),
            &staging.join("dev/dri"),
            none,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            none,
        )?;
    }

    let sys_target = staging.join("sys");

    mount(
        Some("sysfs"),
        &sys_target,
        Some("sysfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        none,
    )
    .context("Failed to mount fresh sysfs")?;

    let shm_target = staging.join("dev/shm");
    fs::create_dir_all(&shm_target).ok();
    mount(
        Some("tmpfs"),
        &shm_target,
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        Some("mode=1777"),
    )
    .context("Failed to mount /dev/shm")?;

    let tmp_target = staging.join("tmp");
    mount(
        Some("tmpfs"),
        &tmp_target,
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
        Some("mode=1777"),
    )
    .context("Failed to mount /tmp")?;

    let dev_nodes = ["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"];
    for node in &dev_nodes {
        let target = staging.join(node.trim_start_matches('/'));
        fs::File::create(&target).ok();

        mount(Some(*node), &target, none, MsFlags::MS_BIND, none)
            .with_context(|| format!("Failed to bind-mount {}", node))?;
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("~"));
    let base_prefix = PathBuf::from(&home).join(".local/share/swine/base_prefix");
    let profile_dir = PathBuf::from(&home)
        .join(".local/share/swine/profiles")
        .join(profile_name);
    let upperdir = profile_dir.join("upper");
    let workdir = profile_dir.join("work");

    fs::create_dir_all(&base_prefix).ok();
    fs::create_dir_all(&upperdir).ok();
    fs::create_dir_all(&workdir).ok();

    let overlay_options = format!(
        "lowerdir={},upperdir={},workdir={}",
        base_prefix.display(),
        upperdir.display(),
        workdir.display()
    );

    let wine_prefix_target = staging.join("home/user/.wine");

    if let Err(e) = mount(
        Some("overlay"),
        &wine_prefix_target,
        Some("overlay"),
        MsFlags::empty(),
        Some(overlay_options.as_str()),
    ) {
        println!(
            "Child: Native OverlayFS failed ({:?}). Attempting fuse-overlayfs fallback...",
            e
        );

        let mut child = std::process::Command::new("fuse-overlayfs")
            .arg("-o")
            .arg(&overlay_options)
            .arg(&wine_prefix_target)
            .spawn()
            .context("Failed to spawn fuse-overlayfs fallback")?;

        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                anyhow::bail!("fuse-overlayfs exited early with error: {}", status);
            }
        }
        println!("Child: fuse-overlayfs mounted successfully.");
    } else {
        println!("Child: Native OverlayFS mounted successfully.");
    }

    let proc_target = staging.join("proc");
    mount(
        Some("proc"),
        &proc_target,
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        none,
    )
    .context("Failed to mount /proc onto staging")?;

    if let Ok(xdg_runtime) = std::env::var("XDG_RUNTIME_DIR") {
        let xdg_dir = Path::new(&xdg_runtime);
        if let Ok(entries) = std::fs::read_dir(xdg_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("wayland-") && !name.ends_with(".lock") {
                    let source = entry.path();
                    let target_dir = staging.join("run/user/1000");
                    fs::create_dir_all(&target_dir).ok();

                    let target = target_dir.join(&name);
                    fs::File::create(&target).ok();

                    if mount(
                        Some(&source),
                        &target,
                        none,
                        MsFlags::MS_BIND | MsFlags::MS_REC,
                        none,
                    )
                    .is_ok()
                    {
                        println!("Child: Proxied Wayland socket ({}) to sandbox.", name);
                    }
                }
            }
        }
    }

    if let Some(dz) = dropzone {
        let workspace_target = staging.join("workspace");
        fs::create_dir_all(&workspace_target).context("Failed to create /workspace")?;

        mount(
            Some(&dz),
            &workspace_target,
            none,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            none,
        )?;

        mount(
            none,
            &workspace_target,
            none,
            MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY | MsFlags::MS_REC,
            none,
        )?;
        println!(
            "Child: Mounted dropzone {:?} to /workspace (read-only).",
            dz
        );
    }

    chdir(staging).context("Failed to chdir to staging")?;

    fs::create_dir_all("put_old").context("Failed to create put_old")?;
    pivot_root(".", "put_old").context("Failed to pivot_root")?;

    chdir("/").context("Failed to chdir to / after pivot")?;

    umount2("/put_old", MntFlags::MNT_DETACH).context("Failed to unmount old root")?;
    fs::remove_dir("/put_old").ok();

    println!("Child: Filesystem isolated successfully.");

    if Path::new("/etc/fonts").exists() {
        let fonts_target = staging.join("etc/fonts");
        fs::create_dir_all(&fonts_target).ok();
        mount(
            Some("/etc/fonts"),
            &fonts_target,
            none,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            none,
        )
        .ok();
    }

    let net_files = [
        "/etc/resolv.conf",
        "/etc/nsswitch.conf",
        "/etc/ssl/certs",
        "/etc/ca-certificates",
    ];

    for file in &net_files {
        if Path::new(file).exists() {
            let target = staging.join(file.trim_start_matches('/'));

            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).ok();
            }

            if Path::new(file).is_dir() {
                fs::create_dir_all(&target).ok();
            } else {
                fs::File::create(&target).ok();
            }

            mount(
                Some(*file),
                &target,
                none,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                none,
            )
            .ok();
        }
    }

    Ok(())
}
