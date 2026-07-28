# swine

`swine` is a specialized, low-level container runtime written in Rust designed to execute untrusted Windows binaries through Wine in an isolated environment.

Instead of relying on generic sandbox wrappers like Bubblewrap or Flatpak, `swine` interacts directly with Linux kernel primitives (`unshare`, `pivot_root`, OverlayFS, Seccomp-BPF, and Cgroups v2) to enforce strict security boundaries specifically tuned for Windows executables, games, and installers.

---

## Architectural Principles

* **Zero Host File Visibility:** The host filesystem is replaced using `pivot_root` into an in-memory `tmpfs`. The container only sees system libraries required for runtime execution, mounted read-only.
* **Transient & Persistent State Split:** Wine prefixes are constructed using kernel OverlayFS (with automatic fallback to `fuse-overlayfs`). A shared, immutable base prefix acts as the `lowerdir`, while game modifications, registry changes, and save data are isolated in a profile-specific `upperdir`. Resetting a profile requires only deleting its `upperdir`.
* **Display & Input Server Isolation:** GUI and input handling are proxied through Gamescope (a Wayland micro-compositor). The guest process receives a sandboxed X11 server, preventing clipboard access, screen scraping, and host keylogging. Physical device nodes under `/dev/input` are never exposed to the sandbox.
* **System Call Filtering:** A Seccomp-BPF filter is compiled and loaded before process execution to drop high-risk kernel interfaces (`ptrace`, `bpf`, `keyctl`, `userfaultfd`, `kcmp`, `unshare`).
* **Identity Scrubbing:** User namespaces map internal UIDs to isolated IDs, stripping host usernames, hostname identifiers, and environment variables.

---

## Scope (V1)

The host system must satisfy the following constraints:

* **Distribution Target:** Arch Linux (V1 hardcodes system path mappings for Arch multiarch `/usr/lib` and `/usr/lib32`).
* **Kernel:** Linux 5.11 or newer (with unprivileged user namespaces and delegated Cgroups v2 enabled).
* **Graphics:** Open-source Vulkan-capable graphics drivers (Mesa / AMDGPU / Intel) with DRM access (`/dev/dri`). Proprietary Nvidia drivers are unsupported in V1.
* **Runtime Dependencies:**
  * `wine` / `wine64`
  * `gamescope`
  * `pipewire` or `pulseaudio` (for audio socket passthrough over Wayland)
  * `fuse-overlayfs` in case your current filesystem can't use overlayfs via the kernel

---

## Directory Layout

`swine` manages configurations and state in standard XDG directories:

```text
~/.config/swine/
├── config.toml               # Global settings and default parameters
└── profiles/
    ├── default.toml          # Default fallback profile
    └── game_name.toml        # Game-specific overrides

~/.local/share/swine/
├── base_prefix/              # Clean, read-only base Wine prefix
└── profiles/
    └── game_name/
        ├── upper/            # Read-write overlay layer
        └── work/             # OverlayFS worker directory

```

---

## CLI

The binary exposes commands for initializing the runtime, executing programs, managing profile states, and inspecting configurations.

```bash
# Initialize or update the shared base Wine prefix
swine init

# Run an executable using the default sandbox profile
swine run path/to/game.exe

# Run an executable under a specific profile
swine run --profile game_name path/to/game.exe

# Run an installer with a read-only dropzone directory
swine run --profile game_name --dropzone ~/Downloads/Repack setup.exe

# Manage profiles
swine profile list
swine profile create game_name
swine profile reset game_name    # Clears the profile upperdir

```

### CLI Commands & Flags

#### `swine init`

Initializes the base Wine prefix located at `~/.local/share/swine/base_prefix` by executing `wineboot -u` outside the restrictive sandbox. This directory acts as the immutable `lowerdir` for all profiles.

#### `swine run`

* `--profile <NAME>`: Target profile configuration (defaults to `default`).
* `--net`: Enable network namespace access (disabled by default).
* `--dropzone <DIR>`: Expose a host directory read-only at `/workspace` inside the sandbox. Prompts for interactive confirmation displaying file count and total size before launching.
* `--allow-dropzone`: Automatically approve the dropzone confirmation prompt (for non-interactive scripting).
* `--resolution <WIDTHxHEIGHT>`: Override the Gamescope display resolution (e.g., `1920x1080`).
* `--dry-run`: Output the planned namespace, mount, and execve parameters without executing.

---

## Dropzone Safety & Interactive Prompt

When launching an installer or executable using `--dropzone <DIR>`, `swine` inspects the target directory on the host and prompts the user before mounting:

```text
Mounting Dropzone: /home/user/Downloads/Repack
Contains: 14 files (4.2 GB)
Warning: These files will be visible (read-only) to the untrusted process.
Do you want to proceed? [y/N]

```

To bypass this prompt in automated scripts, pass `--allow-dropzone`.

---

## Configuration Format

Profiles are defined using TOML files located in `~/.config/swine/profiles/`.

```toml
[profile]
name = "game_name"
description = "Untrusted installer and game profile"

[network]
allow_network = false       # If false, unshares CLONE_NEWNET (loopback only)

[graphics]
gamescope = true
resolution = "1920x1080"
framerate_limit = 60
fsr_enabled = false

[resources]
memory_limit_mb = 8192      # Enforced via delegated Cgroups v2 memory.max
cpu_quota_percent = 400     # Maximum CPU core consumption (400% = 4 cores)

[sandbox]
drop_all_caps = true          # Drops bounding capabilities before execve

[environment]
# Additional environment variables passed to Wine inside the sandbox
WINEARCH = "win64"
DXVK_HUD = "compiler"

```

---

## Security Non-Goals & Boundaries (V1)

`swine` has not received a security audit,and does not attempt to defend against:

1. **Kernel Zero-Day Exploits:** If an untrusted binary contains exploit code targeting a Linux syscall allowed by the Seccomp filter, `swine` cannot prevent execution at the kernel level.
2. **Open-Source GPU Driver Vulnerabilities:** Because `/dev/dri` must be exposed for 3D acceleration, malicious code targeting vulnerabilities inside Mesa drivers falls outside this boundary. Proprietary Nvidia driver nodes are strictly excluded.
3. **Physical Hardware Controllers:** Direct passthrough of `/dev/input` hardware nodes is omitted in V1 to avoid keylogging vectors; all mouse and keyboard inputs are proxied via Wayland/Gamescope.
