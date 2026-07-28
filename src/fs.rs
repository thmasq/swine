use anyhow::{Context, Result};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::unistd::{chdir, pivot_root};
use std::fs;
use std::path::{Path, PathBuf};

pub struct FsSetupResult {
    pub fuse_pid: Option<u32>,
}

pub fn isolate_filesystem(profile_name: &str, dropzone: Option<PathBuf>) -> Result<FsSetupResult> {
    println!("Child: Assembling isolated filesystem...");
    let none: Option<&str> = None;

    mount(none, "/", none, MsFlags::MS_PRIVATE | MsFlags::MS_REC, none)
        .context("Failed to remount / as private")?;

    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let staging = PathBuf::from(xdg_runtime).join("swine_root");

    setup_staging_area(&staging)?;
    mount_host_system_dirs(&staging)?;
    mount_pseudo_filesystems(&staging)?;
    mount_dev_nodes(&staging)?;

    mount_overlay_binds(profile_name, &staging)?;

    proxy_audio_socket(&staging)?;

    if let Some(dz) = dropzone {
        mount_dropzone(&dz, &staging)?;
    }

    mount_network_and_fonts(&staging)?;

    finalize_pivot_root(&staging)?;

    let fuse_pid = mount_overlay_fs_post_pivot()?;

    println!("Child: Filesystem isolated successfully.");

    Ok(FsSetupResult { fuse_pid })
}

fn setup_staging_area(staging: &Path) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(staging) {
        if !meta.is_dir() {
            anyhow::bail!("Staging path exists but is not a directory (possible symlink attack)");
        }
    } else {
        fs::create_dir_all(staging).context("Failed to create staging dir")?;
    }

    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(staging, fs::Permissions::from_mode(0o700))
        .context("Failed to secure staging dir permissions")?;

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
    Ok(())
}

fn mount_host_system_dirs(staging: &Path) -> Result<()> {
    let none: Option<&str> = None;
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
                MsFlags::MS_REMOUNT
                    | MsFlags::MS_BIND
                    | MsFlags::MS_RDONLY
                    | MsFlags::MS_REC
                    | MsFlags::MS_NOSUID
                    | MsFlags::MS_NODEV,
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

    Ok(())
}

fn mount_pseudo_filesystems(staging: &Path) -> Result<()> {
    let none: Option<&str> = None;

    let sys_target = staging.join("sys");

    if mount(
        Some("sysfs"),
        &sys_target,
        Some("sysfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        none,
    )
    .is_err()
    {
        mount(Some("/sys"), &sys_target, none, MsFlags::MS_BIND, none)
            .context("Failed to bind-mount host /sys as fallback")?;

        mount(
            none,
            &sys_target,
            none,
            MsFlags::MS_REMOUNT
                | MsFlags::MS_BIND
                | MsFlags::MS_RDONLY
                | MsFlags::MS_REC
                | MsFlags::MS_NOSUID
                | MsFlags::MS_NODEV
                | MsFlags::MS_NOEXEC,
            none,
        )
        .context("Failed to remount host /sys read-only")?;
    }

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

    let proc_target = staging.join("proc");
    mount(
        Some("proc"),
        &proc_target,
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        none,
    )
    .context("Failed to mount /proc onto staging")?;

    Ok(())
}

fn mount_dev_nodes(staging: &Path) -> Result<()> {
    let none: Option<&str> = None;
    let dev_nodes = [
        "/dev/null",
        "/dev/zero",
        "/dev/urandom",
        "/dev/random",
        "/dev/fuse",
    ];

    for node in &dev_nodes {
        let target = staging.join(node.trim_start_matches('/'));
        if Path::new(node).exists() {
            fs::File::create(&target).ok();

            mount(Some(*node), &target, none, MsFlags::MS_BIND, none)
                .with_context(|| format!("Failed to bind-mount {}", node))?;
        }
    }
    Ok(())
}

fn mount_overlay_binds(profile_name: &str, staging: &Path) -> Result<()> {
    let none: Option<&str> = None;
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("~"));
    let base_prefix = PathBuf::from(&home).join(".local/share/swine/base_prefix");

    let profile_dir = PathBuf::from(&home)
        .join(".local/share/swine/profiles")
        .join(profile_name);

    fs::create_dir_all(&base_prefix).ok();
    fs::create_dir_all(&profile_dir.join("upper")).ok();
    fs::create_dir_all(&profile_dir.join("work")).ok();

    let overlay_base = staging.join(".swine_overlay");
    fs::create_dir_all(overlay_base.join("lower")).ok();
    fs::create_dir_all(overlay_base.join("profile")).ok();

    mount(
        Some(&base_prefix),
        &overlay_base.join("lower"),
        none,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        none,
    )?;
    mount(
        none,
        &overlay_base.join("lower"),
        none,
        MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY | MsFlags::MS_REC,
        none,
    )?;

    mount(
        Some(&profile_dir),
        &overlay_base.join("profile"),
        none,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        none,
    )?;

    Ok(())
}

fn mount_overlay_fs_post_pivot() -> Result<Option<u32>> {
    let overlay_options = "lowerdir=/.swine_overlay/lower,upperdir=/.swine_overlay/profile/upper,workdir=/.swine_overlay/profile/work";
    let wine_prefix_target = "/home/user/.wine";
    let mut fuse_pid = None;

    if let Err(e) = mount(
        Some("overlay"),
        wine_prefix_target,
        Some("overlay"),
        MsFlags::empty(),
        Some(overlay_options),
    ) {
        println!(
            "Child: Native OverlayFS failed ({:?}). Attempting fuse-overlayfs fallback...",
            e
        );

        let mut child = std::process::Command::new("fuse-overlayfs")
            .arg("-o")
            .arg(overlay_options)
            .arg(wine_prefix_target)
            .spawn()
            .context("Failed to spawn fuse-overlayfs fallback")?;

        fuse_pid = Some(child.id());
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

    Ok(fuse_pid)
}

fn proxy_audio_socket(staging: &Path) -> Result<()> {
    let none: Option<&str> = None;

    if let Ok(xdg_runtime) = std::env::var("XDG_RUNTIME_DIR") {
        let xdg_dir = Path::new(&xdg_runtime);

        let source = xdg_dir.join("pulse/native");
        if source.exists() {
            let target = staging.join("run/user/1000/pulse/native");

            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).ok();
            }
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
                println!("Child: Proxied PulseAudio socket to sandbox.");
            }
        }
    }
    Ok(())
}

fn mount_dropzone(dz: &Path, staging: &Path) -> Result<()> {
    let none: Option<&str> = None;
    let workspace_target = staging.join("workspace");
    fs::create_dir_all(&workspace_target).context("Failed to create /workspace")?;

    mount(
        Some(dz),
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
    Ok(())
}

fn mount_network_and_fonts(staging: &Path) -> Result<()> {
    let none: Option<&str> = None;

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

        mount(
            none,
            &fonts_target,
            none,
            MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY | MsFlags::MS_REC,
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
            mount(
                none,
                &target,
                none,
                MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY | MsFlags::MS_REC,
                none,
            )
            .ok();
        }
    }

    Ok(())
}

fn finalize_pivot_root(staging: &Path) -> Result<()> {
    chdir(staging).context("Failed to chdir to staging")?;

    fs::create_dir_all("put_old").context("Failed to create put_old")?;
    pivot_root(".", "put_old").context("Failed to pivot_root")?;

    chdir("/").context("Failed to chdir to / after pivot")?;

    umount2("/put_old", MntFlags::MNT_DETACH).context("Failed to unmount old root")?;
    fs::remove_dir("/put_old").ok();

    Ok(())
}
