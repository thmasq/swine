mod cgroup;
mod config;
mod container;
mod dropzone;
mod fs;
mod security;
mod supervisor;

use anyhow::Context;
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "swine")]
#[command(about = "A secure container runtime for executing untrusted Windows binaries via Wine.", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize or update the shared base Wine prefix
    Init,

    /// Run an executable using a sandbox profile
    Run {
        /// Target profile configuration
        #[arg(long, default_value = "default")]
        profile: String,

        /// Enable network namespace access (disabled by default)
        #[arg(long)]
        net: bool,

        /// Expose a host directory read-only at /workspace inside the sandbox
        #[arg(long)]
        dropzone: Option<PathBuf>,

        /// Automatically approve the dropzone confirmation prompt
        #[arg(long)]
        allow_dropzone: bool,

        /// Override the Gamescope display resolution (e.g., '1920x1080')
        #[arg(long)]
        resolution: Option<String>,

        /// Output the planned namespace, mount, and execve parameters without executing
        #[arg(long)]
        dry_run: bool,

        /// The executable path to run
        exe: PathBuf,

        /// Additional arguments to pass to the executable
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Manage profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommands,
    },
}

#[derive(Subcommand, Debug)]
enum ProfileCommands {
    /// List all profiles
    List,
    /// Create a new profile
    Create { name: String },
    /// Clear the profile upperdir
    Reset { name: String },
}

fn main() -> Result<()> {
    if nix::unistd::getuid().is_root() {
        anyhow::bail!(
            "swine must not be run as root. The sandbox relies on unprivileged user namespaces."
        );
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            println!("Initializing the base Wine prefix...");
            let home = std::env::var("HOME").unwrap_or_else(|_| String::from("~"));
            let base_prefix = std::path::PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("swine")
                .join("base_prefix");

            if !base_prefix.exists() {
                std::fs::create_dir_all(&base_prefix)
                    .context("Failed to create base_prefix directory")?;
            }

            let status = std::process::Command::new("wineboot")
                .arg("-u")
                .env_clear()
                .env("WINEPREFIX", &base_prefix)
                .env("HOME", "/home/user")
                .env("USER", "root")
                .env("LOGNAME", "root")
                .env("PATH", "/usr/bin:/usr/local/bin:/bin:/sbin")
                .status()
                .context("Failed to execute wineboot")?;

            if !status.success() {
                anyhow::bail!("wineboot exited with status: {}", status);
            }
            println!("Base prefix initialized successfully.");
        }
        Commands::Run {
            profile,
            net,
            dropzone,
            allow_dropzone,
            resolution,
            dry_run,
            exe,
            args,
        } => {
            config::validate_profile_name(&profile)?;
            let mut parsed_config = config::Config::load(&profile)?;

            if net {
                parsed_config.network.allow_network = true;
            }
            if resolution.is_some() {
                parsed_config.graphics.resolution = resolution;
            }

            let host_exe = std::fs::canonicalize(&exe)
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(&exe));

            let mut final_dropzone = dropzone.clone();

            if final_dropzone.is_none() {
                if let Some(parent) = host_exe.parent() {
                    if !host_exe.starts_with("/usr")
                        && !host_exe.starts_with("/bin")
                        && !host_exe.starts_with("/lib")
                    {
                        final_dropzone = Some(parent.to_path_buf());
                        println!("Auto-detected dropzone: {:?}", parent);
                    }
                }
            }

            let mut container_exe = exe.clone();
            if let Some(dz) = &final_dropzone {
                let dz_canonical = std::fs::canonicalize(dz).unwrap_or_else(|_| dz.clone());
                if host_exe.starts_with(&dz_canonical) {
                    if let Ok(stripped) = host_exe.strip_prefix(&dz_canonical) {
                        container_exe = std::path::PathBuf::from("/workspace").join(stripped);
                    }
                }
            } else {
                container_exe = host_exe;
            }
            // ----------------------------------------

            if dry_run {
                println!("--- DRY RUN ---");
                println!(
                    "Loaded config for profile '{}': {:#?}",
                    parsed_config.profile.name, parsed_config
                );
                println!("Executable: {:?}", container_exe);
                println!("Args: {:?}", args);
                if let Some(dz) = &final_dropzone {
                    println!("Dropzone: {:?}", dz);
                }
                return Ok(());
            }

            if let Some(dz_path) = &final_dropzone {
                dropzone::validate_dropzone(dz_path)?;

                let proceed = dropzone::check_and_prompt(dz_path, allow_dropzone)?;
                if !proceed {
                    println!("Aborting due to user cancellation.");
                    return Ok(());
                }
            }

            println!("Running executable: {:?}", container_exe);
            supervisor::start_sandbox(&parsed_config, container_exe, args, final_dropzone)?;
        }

        Commands::Profile { command } => match command {
            ProfileCommands::List => {
                let profiles_dir = config::Config::get_profiles_dir();
                if profiles_dir.exists() {
                    let mut found = false;
                    for entry in std::fs::read_dir(profiles_dir)
                        .context("Failed to read profiles directory")?
                    {
                        let entry = entry?;
                        let path = entry.path();

                        if path.is_file()
                            && path.extension().and_then(|s| s.to_str()) == Some("toml")
                        {
                            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                                println!("- {}", name);
                                found = true;
                            }
                        }
                    }
                    if !found {
                        println!("No profiles found.");
                    }
                } else {
                    println!("No profiles directory found.");
                }
            }

            ProfileCommands::Create { name } => {
                config::validate_profile_name(&name)?;
                let profiles_dir = config::Config::get_profiles_dir();
                let path = profiles_dir.join(format!("{}.toml", name));

                if path.exists() {
                    anyhow::bail!("Profile '{}' already exists at {:?}", name, path);
                }

                let mut new_config = config::Config::default();
                new_config.profile.name = name.clone();
                new_config
                    .save()
                    .context("Failed to save new profile configuration")?;

                println!("Created new profile '{}' at {:?}", name, path);
            }

            ProfileCommands::Reset { name } => {
                config::validate_profile_name(&name)?;
                let data_dir = config::Config::get_data_dir(&name);

                if data_dir.exists() {
                    std::fs::remove_dir_all(&data_dir).with_context(|| {
                        format!("Failed to remove profile data directory at {:?}", data_dir)
                    })?;
                    println!("Successfully reset overlay data for profile '{}'.", name);
                } else {
                    println!(
                        "Profile data directory for '{}' does not exist (nothing to reset).",
                        name
                    );
                }
            }
        },
    }

    Ok(())
}
