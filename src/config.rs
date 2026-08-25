use tokio::fs;
use std::{
    fs as std_fs, 
    path::PathBuf
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error};

/// Default language for bot messages.
pub const DEFAULT_LANG: &str = "en";
/// Default bot command.
pub const DEFAULT_COMMAND: &str = "remind";
/// Default bot command.
pub const DEFAULT_LIST_COMMAND: &str = "list";
/// Default bot command.
pub const DEFAULT_TIMEZONE_COMMAND: &str = "tz";
/// Default bot command.
pub const DEFAULT_TZ: &str = "Europe/Paris";
/// Default times
pub const DEFAULT_MORNING_TIME: &str = "09:00";
pub const DEFAULT_AFTERNOON_TIME: &str = "14:00";
pub const DEFAULT_EVENING_TIME: &str = "19:00";


/// Config for AppContext
#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub auth: MatrixConfig,
    pub recovery: Option<RecoveryConfig>,
    pub bot: BotConfig,
}

/// Config for matrix server authentication
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct MatrixConfig {
    pub homeserver: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
    pub device: Option<String>,
    pub recovery: Option<String>,
}

/// Recovery for account
#[derive(Debug, Serialize, Deserialize)]
pub struct RecoveryConfig {
    pub recovery_key: String,
    pub created_at: String,
}

/// Bot config
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct BotConfig {
    #[serde(default = "BotConfig::default_lang")]
    pub lang: String,
    #[serde(default = "BotConfig::default_remind_command")]
    pub remind_commands: Vec<String>,
    #[serde(default = "BotConfig::default_list_command")]
    pub list_commands: Vec<String>,
    #[serde(default = "BotConfig::default_tz_command")]
    pub tz_commands: Vec<String>,
    #[serde(default = "BotConfig::default_on_command")]
    pub on_command: bool,
    #[serde(default = "BotConfig::default_on_mention")]
    pub on_mention: bool,
    #[serde(default = "BotConfig::default_send_reactions")]
    pub send_reactions: bool,
    #[serde(default = "BotConfig::default_send_digits_reactions")]
    pub send_digits_reactions: bool,
    #[serde(default = "BotConfig::default_tz")]
    pub tz: String,
    #[serde(default = "BotConfig::default_morning_time")]
    pub morning: String,
    #[serde(default = "BotConfig::default_afternoon_time")]
    pub afternoon: String,
    #[serde(default = "BotConfig::default_evening_time")]
    pub evening: String,
}

impl BotConfig {
    fn default_lang() -> String {
        DEFAULT_LANG.into()
    }
    fn default_on_command() -> bool {
        true
    }
    fn default_on_mention() -> bool {
        false
    }
    fn default_send_reactions() -> bool {
        true
    }
    fn default_send_digits_reactions() -> bool {
        true
    }
    fn default_remind_command() -> Vec<String> {
        vec![DEFAULT_COMMAND.to_string()]
        // let mut map = HashMap::new();

        // for serde_yaml_ng::Value type: HashMap<String, serde_yaml_ng::Value>
        // map.insert("remind".to_string(), vec![serde_yaml_ng::Value::String(DEFAULT_COMMAND.to_string())].into());

        // map.insert("remind".to_string(), vec![DEFAULT_COMMAND.to_string()]);
        // map.insert("list".to_string(), vec![DEFAULT_LIST_COMMAND.to_string()]);
        // map.insert("tz".to_string(), vec![DEFAULT_TIMEZONE_COMMAND.to_string()]);
        // map
    }
    fn default_list_command() -> Vec<String> {
        vec![DEFAULT_LIST_COMMAND.to_string()]
    }
    fn default_tz_command() -> Vec<String> {
        vec![DEFAULT_TIMEZONE_COMMAND.to_string()]
    }
    fn default_tz() -> String {
        DEFAULT_TZ.into()
    }
    fn default_morning_time() -> String {
        DEFAULT_MORNING_TIME.into()
    }
    fn default_afternoon_time() -> String {
        DEFAULT_AFTERNOON_TIME.into()
    }
    fn default_evening_time() -> String {
        DEFAULT_EVENING_TIME.into()
    }

    /// Create config.yaml from config variable
    pub fn save_setup(&self, overwrite: bool, data_dir: &PathBuf, config: Option<BotConfig>) -> anyhow::Result<()> {
        let yaml_path = data_dir.join("config.yaml");
        if yaml_path.exists() && !overwrite {
            warn!("Configuration file already exists at {}", yaml_path.display());
            return Ok(());
        }

        let yaml_str = if let Some(c) = config {
            serde_yaml_ng::to_string(&c).context("Failed to serialize bot config")?
        }
        else {
            serde_yaml_ng::to_string(&self).context("Failed to serialize bot config")?
        };

        std_fs::write(&yaml_path, yaml_str)?;

        info!(file = %yaml_path.display(), "The data has been written to config.yaml.");

        Ok(())
    }

    /*
    /// Interactive setup config
    pub fn setup_config(&mut self, data_dir: &PathBuf) -> anyhow::Result<()> {
        // let languages = vec!["en", "de", "fr", "it", "es", "sv", "pl", "cs", "fi", "ja", "zh", "ru", "uk"];
        let languages = rust_i18n::available_locales!();
        self.lang = Select::new("Select language:", languages).prompt()?.to_string();

        let remind_commands = Text::new("Aliases for the reminder creation command:")
            .with_help_message("The command in your chosen language will always be available.")
            .with_placeholder("separated by spaces")
            .with_default("remind").prompt()?.to_string();
        self.remind_commands = remind_commands
            .split_whitespace()
            .map(String::from)
            .collect();

        self.on_command = Confirm::new("Activate the bot only on command?")
            .with_help_message("If you select \"no\", the bot will attempt to create a reminder whenever it receives a message.")
            .with_default(true).prompt()?;
        self.on_mention = Confirm::new("Activate the bot only when mentioned in group rooms?")
            .with_help_message("In rooms with only two people, the bot will respond regardless of whether it is mentioned.")
            .with_default(false).prompt()?;

        self.morning = Text::new("Set morning time (HH:MM):")
            .with_default(DEFAULT_MORNING_TIME)
            .with_validator(validate_config_time)
            .prompt()?.to_string();
        self.afternoon = Text::new("Set afternoon time (HH:MM):")
            .with_default(DEFAULT_AFTERNOON_TIME)
            .with_validator(validate_config_time)
            .prompt()?.to_string();
        self.evening = Text::new("Set evening time (HH:MM):")
            .with_default(DEFAULT_EVENING_TIME)
            .with_validator(validate_config_time)
            .prompt()?.to_string();

        let overwrite = Confirm::new("Overwrite current configuration if any?").with_default(true).prompt()?;
        
        self.save_to_file(overwrite, &data_dir)?;

        Ok(())
    }
    */
}

impl AppConfig {
    pub async fn load(data_dir: &PathBuf) -> Result<Self> {
        //dotenvy::dotenv().ok();

        // .env
        let auth: MatrixConfig = envy::prefixed("MATRIX_")
            .from_env()
            .map_err(|e| anyhow::anyhow!("Environment error: {}", e))?;

        // recovery.json
        // let data_dir = dirs::data_dir().context("No data_dir directory found")?.join(super::APP_FOLDER);
        let recovery_file = data_dir.join("recovery.json");

        let recovery = if recovery_file.exists() {
            let serialized = fs::read_to_string(&recovery_file).await
                .context("Error reading recovery.json")?;
            let data: RecoveryConfig = serde_json::from_str(&serialized)
                .context("File recovery.json has invalid JSON")?;
            Some(data)
        } else {
            None
        };

        // config.yaml or .env
        // yaml and yml files are supported
        let yaml_path = data_dir.join("config.yaml");
        let yml_path = data_dir.join("config.yml");
        let config_path = if yaml_path.exists() {
            Some(yaml_path)
        } else if yml_path.exists() {
            Some(yml_path)
        } else {
            None
        };
        
        let bot: BotConfig = if let Some(path) = config_path {
            let yaml_str = fs::read_to_string(&path).await
                .with_context(|| format!("Error reading {}", path.display()))?;
            
            match serde_yaml_ng::from_str::<BotConfig>(&yaml_str) {
                Ok(config) => {
                    info!(file = %path.display(), "Successfully loaded bot configuration");
                    config
                },
                Err(e) => {
                    error!(error = %e, "Failed to parse YAML config. Falling back to .env");
                    envy::prefixed("BOT_")
                        .from_env()
                        .map_err(|e| anyhow::anyhow!("Environment error: {}", e))?
                }
            }
        } else {
            warn!("No config.yaml or config.yml found, using .env variables");
            envy::prefixed("BOT_")
                .from_env()
                .map_err(|e| anyhow::anyhow!("Environment error: {}", e))?
        };

        Ok(Self { auth, recovery, bot })
    }
}
