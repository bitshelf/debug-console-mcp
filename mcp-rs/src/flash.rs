//! Flash abstraction — per-SoC firmware flashing commands.
//!
//! Reads `[flash]` section from `.target.toml` to get the tool name and
//! command templates for full-image and kernel-only flashing.
//! Supports MASKROM loader binary path for recovery flashing.

use std::collections::HashMap;
use std::path::Path;

/// Flash configuration loaded from `[flash]` section of `.target.toml`.
#[derive(Debug, Clone, Default)]
pub struct FlashConfig {
    /// Flash tool name (e.g. "upgrade_tool", "rkdeveloptool", "fastboot").
    pub tool: String,
    /// Command template for full image flash. `{image}` is replaced with the
    /// image path on the dev host.
    pub full_image_cmd: String,
    /// Command template for kernel-only flash.
    pub kernel_image_cmd: String,
    /// MASKROM loader binary path (flashed first in MASKROM mode).
    pub loader_bin: String,
    /// Command template for loading MASKROM helper. `{loader}` is replaced
    /// with `loader_bin`.
    pub loader_cmd: String,
    /// Command used to list connected flash devices.
    pub list_devices_cmd: String,
    /// Upload directory on dev host (default: /tmp).
    pub upload_dir: String,
}

impl FlashConfig {
    /// Build from the flat config HashMap (keys prefixed with `FLASH_`).
    pub fn from_config(cfg: &HashMap<String, String>) -> Self {
        Self {
            tool: cfg.get("FLASH_TOOL").cloned().unwrap_or_default(),
            full_image_cmd: cfg
                .get("FLASH_FULL_IMAGE_CMD")
                .cloned()
                .unwrap_or_else(|| "uf {image}".to_string()),
            kernel_image_cmd: cfg
                .get("FLASH_KERNEL_IMAGE_CMD")
                .cloned()
                .unwrap_or_else(|| "wp kernel {image}".to_string()),
            loader_bin: cfg.get("FLASH_LOADER_BIN").cloned().unwrap_or_default(),
            loader_cmd: cfg
                .get("FLASH_LOADER_CMD")
                .cloned()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "DB {loader}".to_string()),
            list_devices_cmd: cfg
                .get("FLASH_LIST_DEVICES_CMD")
                .cloned()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "LD".to_string()),
            upload_dir: cfg
                .get("FLASH_UPLOAD_DIR")
                .cloned()
                .unwrap_or_else(|| "/tmp".to_string()),
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.tool.is_empty()
    }

    /// Resolve the flash command for a full image, substituting `{image}`
    /// with the remote path on the dev host.
    pub fn full_image_command(&self, remote_image_path: &str) -> String {
        self.full_image_cmd.replace("{image}", remote_image_path)
    }

    /// Resolve the flash command for a kernel image.
    pub fn kernel_image_command(&self, remote_image_path: &str) -> String {
        self.kernel_image_cmd.replace("{image}", remote_image_path)
    }

    pub fn loader_command(&self) -> String {
        self.loader_cmd.replace("{loader}", &self.loader_bin)
    }

    pub fn list_devices_command(&self) -> String {
        self.list_devices_cmd.clone()
    }

    /// Resolve a symlink to its real path (for update.img that's a symlink).
    pub fn resolve_symlink(path: &Path) -> std::path::PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Image type to flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ImageType {
    /// Full firmware image (e.g. update.img).
    Full,
    /// Kernel image only.
    Kernel,
}

impl ImageType {
    #[allow(dead_code)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "full" | "update" | "all" => Some(ImageType::Full),
            "kernel" | "boot" => Some(ImageType::Kernel),
            _ => None,
        }
    }
}

/// Flash execution plan — steps to perform on the dev host.
#[derive(Debug, Clone)]
pub struct FlashPlan {
    pub tool: String,
    pub local_image: std::path::PathBuf,
    pub remote_path: String,
    pub dev_host: String,
    pub dev_user: String,
    pub upload_dir: String,
    pub full_image_cmd: String,
    pub kernel_image_cmd: String,
    pub loader_bin: String,
    pub loader_cmd: String,
    pub list_devices_cmd: String,
    pub image_type: ImageType,
}

impl FlashPlan {
    /// Build a flash plan from config + local image path.
    pub fn from_config(
        config: &FlashConfig,
        image_path: &std::path::Path,
        image_type: ImageType,
        dev_host: &str,
        dev_user: &str,
    ) -> Self {
        let real_path = FlashConfig::resolve_symlink(image_path);
        let fname = real_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("firmware.img");
        let upload_dir = if config.upload_dir.is_empty() {
            "/tmp".to_string()
        } else {
            config.upload_dir.clone()
        };
        let remote_path = format!("{upload_dir}/{fname}");

        let full_cmd = config.full_image_command(&remote_path);
        let kernel_cmd = config.kernel_image_command(&remote_path);
        let loader_cmd = if config.loader_bin.is_empty() {
            String::new()
        } else {
            config.loader_command()
        };

        Self {
            tool: config.tool.clone(),
            local_image: real_path,
            remote_path,
            dev_host: dev_host.to_string(),
            dev_user: dev_user.to_string(),
            upload_dir,
            full_image_cmd: full_cmd,
            kernel_image_cmd: kernel_cmd,
            loader_bin: config.loader_bin.clone(),
            loader_cmd,
            list_devices_cmd: config.list_devices_command(),
            image_type,
        }
    }

    /// Generate the list of shell commands to execute on the dev host.
    pub fn commands(&self) -> Vec<String> {
        let mut cmds = Vec::new();
        // Step 1: Upload
        cmds.push(format!(
            "scp {} {}@{}:{}",
            self.local_image.display(),
            self.dev_user,
            self.dev_host,
            self.remote_path
        ));
        // Step 2: Verify sha256
        cmds.push(format!(
            "ssh {}@{} 'sha256sum {}'",
            self.dev_user, self.dev_host, self.remote_path
        ));
        // Step 3: List devices
        if !self.list_devices_cmd.is_empty() {
            cmds.push(format!(
                "ssh {}@{} '{} {}'",
                self.dev_user, self.dev_host, self.tool, self.list_devices_cmd
            ));
        }
        // Step 4: Flash loader in MASKROM
        if !self.loader_bin.is_empty() {
            cmds.push(format!(
                "ssh {}@{} '{} {}'",
                self.dev_user, self.dev_host, self.tool, self.loader_cmd
            ));
        }
        // Step 5: Flash image
        let flash_cmd = match self.image_type {
            ImageType::Full => &self.full_image_cmd,
            ImageType::Kernel => &self.kernel_image_cmd,
        };
        cmds.push(format!(
            "ssh {}@{} '{} {}'",
            self.dev_user, self.dev_host, self.tool, flash_cmd
        ));
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flash_config_from_defaults() {
        let cfg = FlashConfig::from_config(&HashMap::new());
        assert!(!cfg.is_configured());
        assert_eq!(cfg.upload_dir, "/tmp");
        assert_eq!(cfg.full_image_cmd, "uf {image}");
    }

    #[test]
    fn test_flash_config_from_config() {
        let mut map = HashMap::new();
        map.insert("FLASH_TOOL".to_string(), "upgrade_tool".to_string());
        map.insert("FLASH_FULL_IMAGE_CMD".to_string(), "uf {image}".to_string());
        map.insert(
            "FLASH_KERNEL_IMAGE_CMD".to_string(),
            "wp kernel {image}".to_string(),
        );
        map.insert("FLASH_LOADER_BIN".to_string(), "loader.bin".to_string());

        let cfg = FlashConfig::from_config(&map);
        assert!(cfg.is_configured());
        assert_eq!(cfg.tool, "upgrade_tool");
        assert_eq!(cfg.loader_bin, "loader.bin");
    }

    #[test]
    fn test_full_image_command() {
        let cfg = FlashConfig {
            tool: "upgrade_tool".to_string(),
            full_image_cmd: "uf {image}".to_string(),
            ..Default::default()
        };
        let cmd = cfg.full_image_command("/tmp/update.img");
        assert_eq!(cmd, "uf /tmp/update.img");
    }

    #[test]
    fn test_kernel_image_command() {
        let cfg = FlashConfig {
            tool: "rkdeveloptool".to_string(),
            kernel_image_cmd: "wp kernel {image}".to_string(),
            ..Default::default()
        };
        let cmd = cfg.kernel_image_command("/tmp/boot.img");
        assert_eq!(cmd, "wp kernel /tmp/boot.img");
    }

    #[test]
    fn test_image_type_from_str() {
        assert_eq!(ImageType::from_str("full"), Some(ImageType::Full));
        assert_eq!(ImageType::from_str("kernel"), Some(ImageType::Kernel));
        assert_eq!(ImageType::from_str("unknown"), None);
    }

    #[test]
    fn test_flash_plan_missing_image() {
        let values = std::collections::HashMap::new();
        let cfg = crate::config::Config {
            values,
            config_path: None,
            project_dir: None,
            format: crate::config::ConfigFormat::None,
        };
        let flash_cfg = FlashConfig::from_config(&cfg.values);
        let plan = FlashPlan::from_config(
            &flash_cfg,
            std::path::Path::new("/nonexistent/image.img"),
            ImageType::Full,
            "devhost",
            "devuser",
        );
        // FlashPlan::from_config is infallible — nonexistent path stored as-is
        assert_eq!(
            plan.local_image.file_name().unwrap().to_str().unwrap(),
            "image.img"
        );
        // Plan still generates commands despite missing image on disk
        assert!(!plan.commands().is_empty());
    }
}
