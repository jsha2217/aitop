use crate::models::{BarMode, Provider, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub initialized: bool,
    pub refresh_interval_secs: u64,
    pub bar_mode: BarMode,
    pub show_claude: bool,
    pub show_codex: bool,
    pub show_agy: bool,
    pub show_cursor: bool,
    pub show_copilot: bool,
    pub show_grok: bool,
    pub ascii: bool,
    pub no_color: bool,
    pub claude_keychain: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            initialized: true,
            refresh_interval_secs: 300,
            bar_mode: BarMode::Remaining,
            show_claude: false,
            show_codex: false,
            show_agy: false,
            show_cursor: false,
            show_copilot: false,
            show_grok: false,
            ascii: false,
            no_color: false,
            claude_keychain: false,
        }
    }
}
impl Config {
    pub fn enabled(&self, p: Provider) -> bool {
        match p {
            Provider::Claude => self.show_claude,
            Provider::Codex => self.show_codex,
            Provider::Agy => self.show_agy,
            Provider::Cursor => self.show_cursor,
            Provider::Copilot => self.show_copilot,
            Provider::Grok => self.show_grok,
        }
    }
    pub fn toggle(&mut self, p: Provider) {
        let value = match p {
            Provider::Claude => &mut self.show_claude,
            Provider::Codex => &mut self.show_codex,
            Provider::Agy => &mut self.show_agy,
            Provider::Cursor => &mut self.show_cursor,
            Provider::Copilot => &mut self.show_copilot,
            Provider::Grok => &mut self.show_grok,
        };
        *value = !*value;
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version > 1 {
            return Err(
                "Configuration belongs to a newer aitop version; preserved unchanged".into(),
            );
        }
        if !(5..=86400).contains(&self.refresh_interval_secs) {
            return Err(
                "refresh_interval_secs must be between 5 and 86400; configuration preserved".into(),
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
}
impl Paths {
    pub fn new(home: PathBuf, data: Option<PathBuf>) -> Self {
        let config = home.join(".config/aitop/config.json");
        let data = data.unwrap_or_else(|| home.join(".cache/aitop"));
        Self { home, config, data }
    }
    pub fn load(&self) -> Result<Config> {
        match fs::read(&self.config) {
            Ok(bytes) => {
                let cfg: Config = serde_json::from_slice(&bytes).map_err(|_| {
                    "Invalid config.json; file preserved. Repair it before saving settings"
                })?;
                cfg.validate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config {
                show_claude: self.home.join(".claude").exists(),
                show_codex: self.home.join(".codex").exists(),
                show_agy: self.home.join(".gemini/antigravity-cli").exists(),
                show_cursor: self
                    .home
                    .join("Library/Application Support/Cursor")
                    .exists()
                    || self.home.join(".config/Cursor").exists(),
                show_copilot: self.home.join(".config/github-copilot").exists(),
                show_grok: self.home.join(".grok").exists(),
                ..Config::default()
            }),
            Err(_) => Err("Cannot read configuration; file preserved".into()),
        }
    }
    pub fn save(&self, cfg: &Config) -> Result<()> {
        cfg.validate()?;
        if self.config.exists() {
            self.load()?;
        }
        atomic_write(&self.config, &serde_json::to_vec_pretty(cfg)?)
    }
}
pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("Invalid storage path")?;
    private_dir(parent)?;
    let temp = parent.join(format!(
        ".aitop-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
