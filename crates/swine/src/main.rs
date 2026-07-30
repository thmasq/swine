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

            let is_inner = std::env::var("SWINE_GAMESCOPE_INNER").is_ok();
            if parsed_config.graphics.gamescope && !is_inner {
                println!("Spawning swine on a higher level via Gamescope...");
                let mut wrap_cmd = std::process::Command::new("gamescope");

                if let Some(res) = &parsed_config.graphics.resolution {
                    let parts: Vec<&str> = res.split('x').collect();
                    if parts.len() == 2 {
                        wrap_cmd.arg("-W").arg(parts[0]).arg("-H").arg(parts[1]);
                    }
                }

                if let Some(scaler) = &parsed_config.graphics.scaler {
                    let scaler_str = match scaler {
                        crate::config::Scaler::Auto => "auto",
                        crate::config::Scaler::Integer => "integer",
                        crate::config::Scaler::Fit => "fit",
                        crate::config::Scaler::Fill => "fill",
                        crate::config::Scaler::Stretch => "stretch",
                    };
                    wrap_cmd.arg("-S").arg(scaler_str);
                }

                if let Some(filter) = &parsed_config.graphics.filter {
                    let filter_str = match filter {
                        crate::config::Filter::Linear => "linear",
                        crate::config::Filter::Nearest => "nearest",
                        crate::config::Filter::Fsr => "fsr",
                        crate::config::Filter::Nis => "nis",
                        crate::config::Filter::Pixel => "pixel",
                    };
                    wrap_cmd.arg("-F").arg(filter_str);
                }

                for arg in &parsed_config.graphics.gamescope_args {
                    if arg != "--xwayland-count" && arg != "0" {
                        wrap_cmd.arg(arg);
                    }
                }

                wrap_cmd.arg("--");
                wrap_cmd
                    .arg(std::env::current_exe().context("Failed to get current executable path")?);
                for arg in std::env::args().skip(1) {
                    wrap_cmd.arg(arg);
                }

                wrap_cmd.env("SWINE_GAMESCOPE_INNER", "1");

                let mut child = wrap_cmd
                    .spawn()
                    .context("Failed to spawn Gamescope wrapper")?;
                let status = child
                    .wait()
                    .context("Failed to wait on Gamescope wrapper")?;
                std::process::exit(status.code().unwrap_or(1));
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

            let sockets_dir = "/tmp/swine-sockets";
            std::fs::create_dir_all(sockets_dir).ok();
            let host_waypipe_sock = format!("{}/wayland-0", sockets_dir);

            let _ = std::fs::remove_file(&host_waypipe_sock);
            let _ = std::fs::remove_file(format!("{}.lock", host_waypipe_sock));

            let host_waypipe_log = std::fs::File::create("/tmp/swine-waypipe-host.log")?;

            println!("Starting Waypipe client on host...");
            let mut host_cmd = std::process::Command::new("waypipe");

            let wayland_display = std::env::var("GAMESCOPE_WAYLAND_DISPLAY")
                .or_else(|_| std::env::var("WAYLAND_DISPLAY"))
                .unwrap_or_else(|_| "wayland-0".to_string());

            let mut waypipe_host = host_cmd
                .arg("--compress")
                .arg("none")
                .arg("-s")
                .arg(&host_waypipe_sock)
                .arg("client")
                .env("WAYLAND_DISPLAY", wayland_display)
                .env(
                    "XDG_RUNTIME_DIR",
                    std::env::var("XDG_RUNTIME_DIR").unwrap_or_default(),
                )
                .stdout(std::process::Stdio::from(host_waypipe_log.try_clone()?))
                .stderr(std::process::Stdio::from(host_waypipe_log))
                .spawn()
                .context("Failed to spawn waypipe on host")?;

            let handle =
                supervisor::start_sandbox(&parsed_config, container_exe, args, final_dropzone)?;

            let _ = handle.join();
            let _ = waypipe_host.kill();
            let _ = std::fs::remove_file(&host_waypipe_sock);
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
