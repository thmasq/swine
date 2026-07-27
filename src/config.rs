use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub profile: ProfileConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub graphics: GraphicsConfig,
    #[serde(default)]
    pub resources: ResourcesConfig,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            profile: ProfileConfig::default(),
            network: NetworkConfig::default(),
            graphics: GraphicsConfig::default(),
            resources: ResourcesConfig::default(),
            sandbox: SandboxConfig::default(),
            environment: HashMap::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ProfileConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NetworkConfig {
    #[serde(default = "default_false")]
    pub allow_network: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            allow_network: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GraphicsConfig {
    #[serde(default = "default_true")]
    pub gamescope: bool,
    pub resolution: Option<String>,
    pub framerate_limit: Option<u32>,
    #[serde(default = "default_false")]
    pub fsr_enabled: bool,
}

impl Default for GraphicsConfig {
    fn default() -> Self {
        Self {
            gamescope: true,
            resolution: None,
            framerate_limit: None,
            fsr_enabled: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ResourcesConfig {
    pub memory_limit_mb: Option<u64>,
    pub cpu_quota_percent: Option<u32>,
}

impl Default for ResourcesConfig {
    fn default() -> Self {
        Self {
            memory_limit_mb: None,
            cpu_quota_percent: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SandboxConfig {
    #[serde(default = "default_true")]
    pub seccomp_strict: bool,
    #[serde(default = "default_true")]
    pub drop_all_caps: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            seccomp_strict: true,
            drop_all_caps: true,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}

impl Config {
    /// Loads a profile from ~/.config/swine/profiles/<name>.toml
    /// If the file does not exist, returns a default configuration.
    pub fn load(name: &str) -> anyhow::Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| String::from("~"));
        let config_dir = PathBuf::from(home)
            .join(".config")
            .join("swine")
            .join("profiles");

        let path = config_dir.join(format!("{}.toml", name));

        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let mut config: Config = toml::from_str(&content)?;
            if config.profile.name.is_empty() {
                config.profile.name = name.to_string();
            }
            Ok(config)
        } else {
            let mut config = Config::default();
            config.profile.name = name.to_string();
            Ok(config)
        }
    }
}
