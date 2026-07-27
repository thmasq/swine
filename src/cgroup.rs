use anyhow::{Context, Result};
use nix::unistd::Pid;
use std::fs;
use std::path::{Path, PathBuf};

pub fn enforce_limits(config: &crate::config::ResourcesConfig, child_pid: Pid) -> Result<()> {
    let cgroup_path = get_current_cgroup()?;
    let swine_cgroup = cgroup_path.join(format!("swine-{}", child_pid));

    if let Err(e) = fs::create_dir_all(&swine_cgroup) {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow::bail!(
                "Permission denied creating cgroup at {:?}. Your user session might not have delegated cgroup v2 permissions. Try launching swine via `systemd-run --user --scope swine run ...`",
                swine_cgroup
            );
        }
        return Err(e).context("Failed to create cgroup directory");
    }

    if let Some(mem_mb) = config.memory_limit_mb {
        let mem_bytes = mem_mb * 1024 * 1024;
        fs::write(swine_cgroup.join("memory.max"), mem_bytes.to_string())
            .unwrap_or_else(|e| eprintln!("Warning: Failed to set memory.max: {}", e));
    }

    if let Some(cpu_percent) = config.cpu_quota_percent {
        let max = cpu_percent * 1000;
        let period = 100000;
        let cpu_max_str = format!("{} {}", max, period);
        fs::write(swine_cgroup.join("cpu.max"), cpu_max_str).unwrap_or_else(|e| {
            eprintln!(
                "Warning: Failed to set cpu.max (cpu controller might not be delegated): {}",
                e
            )
        });
    }

    let procs_file = swine_cgroup.join("cgroup.procs");
    fs::write(&procs_file, child_pid.as_raw().to_string())
        .with_context(|| format!("Failed to write PID to {:?}", procs_file))?;

    println!(
        "Supervisor: Enforced Cgroup v2 limits at {:?}",
        swine_cgroup
    );

    Ok(())
}

fn get_current_cgroup() -> Result<PathBuf> {
    let content =
        fs::read_to_string("/proc/self/cgroup").context("Failed to read /proc/self/cgroup")?;

    for line in content.lines() {
        if line.starts_with("0::") {
            let path_suffix = line.trim_start_matches("0::");
            let full_path = Path::new("/sys/fs/cgroup").join(path_suffix.trim_start_matches('/'));
            return Ok(full_path);
        }
    }

    anyhow::bail!(
        "Could not find cgroup v2 path in /proc/self/cgroup. Ensure cgroups v2 is enabled."
    );
}
