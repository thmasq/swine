use anyhow::{Context, Result};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

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
            let path = entry.path();
            if path.is_dir() {
                let (c, s) = calculate_dir_stats(&path)?;
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
