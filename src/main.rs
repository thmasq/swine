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
                .env("WINEPREFIX", &base_prefix)
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
            let mut parsed_config = config::Config::load(&profile)?;

            if net {
                parsed_config.network.allow_network = true;
            }
            if resolution.is_some() {
                parsed_config.graphics.resolution = resolution;
            }

            if dry_run {
                println!("--- DRY RUN ---");
                println!(
                    "Loaded config for profile '{}': {:#?}",
                    parsed_config.profile.name, parsed_config
                );
                println!("Executable: {:?}", exe);
                println!("Args: {:?}", args);
                if let Some(dz) = &dropzone {
                    println!("Dropzone: {:?}", dz);
                }
                return Ok(());
            }

            if let Some(dz_path) = &dropzone {
                let proceed = dropzone::check_and_prompt(dz_path, allow_dropzone)?;
                if !proceed {
                    println!("Aborting due to user cancellation.");
                    return Ok(());
                }
            }

            println!("Running executable: {:?}", exe);
            supervisor::start_sandbox(&parsed_config, exe.clone(), args.clone(), dropzone)?;
        }

        Commands::Profile { command } => match command {
            ProfileCommands::List => {
                println!("Listing profiles...");
                // TODO: List profiles
            }
            ProfileCommands::Create { name } => {
                println!("Creating profile: {}", name);
                // TODO: Create profile
            }
            ProfileCommands::Reset { name } => {
                println!("Resetting profile: {}", name);
                // TODO: Reset profile
            }
        },
    }

    Ok(())
}
