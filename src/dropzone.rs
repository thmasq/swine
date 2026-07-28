use anyhow::{Context, Result};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn validate_dropzone(dropzone_path: &Path) -> Result<()> {
    let canonical =
        fs::canonicalize(dropzone_path).context("Failed to canonicalize dropzone path")?;

    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/nonexistent"));
    let home_path = Path::new(&home);

    let mut blocked_paths = vec![
        PathBuf::from("/"),
        PathBuf::from("/home"),
        PathBuf::from("/etc"),
        PathBuf::from("/var"),
        PathBuf::from("/usr"),
        PathBuf::from("/sys"),
        PathBuf::from("/proc"),
        PathBuf::from("/dev"),
        home_path.to_path_buf(),
        home_path.join(".ssh"),
        home_path.join(".gnupg"),
        home_path.join(".config"),
        home_path.join(".local"),
        home_path.join(".pki"),
    ];

    let common_xdg = [
        "Desktop",
        "Documents",
        "Downloads",
        "Music",
        "Pictures",
        "Public",
        "Templates",
        "Videos",
    ];
    for dir in &common_xdg {
        blocked_paths.push(home_path.join(dir));
    }

    if let Ok(content) = fs::read_to_string(home_path.join(".config/user-dirs.dirs")) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("XDG_") && line.contains("=\"$HOME/") {
                if let Some(split) = line.split("=\"$HOME/").nth(1) {
                    let dir_name = split.trim_end_matches('"');
                    blocked_paths.push(home_path.join(dir_name));
                }
            }
        }
    }

    for blocked in &blocked_paths {
        if let Ok(canon_blocked) = fs::canonicalize(blocked) {
            if canon_blocked == canonical {
                anyhow::bail!(
                    "Security: Refusing to mount sensitive directory {:?} as a dropzone.\nPlease place the executable in a dedicated subdirectory to isolate it.",
                    canonical
                );
            }
        }
    }

    Ok(())
}

pub fn check_and_prompt(dropzone_path: &Path, allow_dropzone: bool) -> Result<bool> {
    if !dropzone_path.exists() || !dropzone_path.is_dir() {
        anyhow::bail!(
            "Dropzone path does not exist or is not a directory: {:?}",
            dropzone_path
        );
    }

    let (file_count, total_size) =
        calculate_dir_stats(dropzone_path).context("Failed to calculate dropzone size")?;

    println!("Mounting Dropzone: {}", dropzone_path.display());
    println!(
        "Contains: {} files ({})",
        file_count,
        format_size(total_size)
    );
    println!("Warning: These files will be visible (read-only) to the untrusted process.");

    if allow_dropzone {
        return Ok(true);
    }

    print!("Do you want to proceed? [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let input = input.trim().to_lowercase();
    Ok(input == "y" || input == "yes")
}

fn calculate_dir_stats(dir: &Path) -> Result<(u64, u64)> {
    let mut count = 0;
    let mut size = 0;

    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;

            if file_type.is_symlink() {
                continue;
            }

            if file_type.is_dir() {
                let (c, s) = calculate_dir_stats(&entry.path())?;
                count += c;
                size += s;
            } else {
                count += 1;
                let metadata = entry.metadata()?;
                size += metadata.len();
            }
        }
    }

    Ok((count, size))
}

fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1} KB", size as f64 / KB as f64)
    } else {
        format!("{} bytes", size)
    }
}
