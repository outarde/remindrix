/*
#[macro_use]
extern crate rust_i18n;
*/

use std::{
    sync::Arc,
    path::PathBuf,
    collections::HashMap
};
use matrix_sdk::{
    Client, 
    config::SyncSettings,
    ruma::OwnedUserId
};
use anyhow::Result;
use tracing_subscriber;
use tracing::info;
use tokio::signal;
use tokio::sync::RwLock;
use tokio_rusqlite::Connection;

mod cli;
mod config;
mod auth;
mod context;
mod handlers;
mod parsers;
mod natural;
mod messaging;
mod db;
mod reminder;
mod settings;
mod settings_service;
mod remote_i18n;

use crate::remote_i18n::RemoteI18n;
use crate::context::I18nManager;
use crate::settings_service::SettingsService;
use crate::db::{ ReminderRepository, SettingRepository, DbContext };

rust_i18n::i18n!("locales", fallback = "en", backend = RemoteI18n::new());

/// Folder for storing session files: session.json, database for persist session. recovery.json
/// and app sqlite database: reminders.db
/// Located in dirs::data_dir() directory.
pub const APP_FOLDER: &str = "remindrix";
/// Docker Healthcheck.
const HEALTHCHECK_PATH: &str = "/tmp/healthy";

pub struct AppConfig {
    pub bot: config::BotConfig,
    pub auth: config::MatrixConfig,
    pub recovery: Option<config::RecoveryConfig>,
    pub data_dir: PathBuf,
    pub session_file: PathBuf,
}

impl AppConfig {
    pub async fn load() -> Result<Self> {
        let data_dir = dirs::data_dir().expect("No data_dir directory found").join(APP_FOLDER);
        let session_file = data_dir.join("session.json");

        let config = config::AppConfig::load(&data_dir).await?;
        rust_i18n::set_locale(&config.bot.lang);

        Ok(Self {
            bot: config.bot, 
            auth: config.auth, 
            recovery: config.recovery, 
            data_dir, 
            session_file,
        })
    }
}

pub struct BotRuntime {
    client: Client,
    bot_id: OwnedUserId,
    // sync_token: Option<String>,
    sync_settings: SyncSettings,
    session_file: PathBuf,
}

impl BotRuntime {
    pub async fn init(config: &AppConfig) -> Result<Self> {
        let (client, sync_token) = if config.session_file.exists() {
            auth::restore_session(&config.session_file).await?
        } else {
            // Try to get recovery key for recover backup and cross-signing keys in login()
            let key = auth::pick_recovery_key(None, &config.auth, &config.recovery).await;
            
            (auth::login(
                &config.data_dir, 
                &config.session_file, 
                &config.auth,
                key.as_deref()
            ).await?, None)
        };

        //let client = client.unwrap();
        let bot_id = client.user_id().unwrap().to_owned();
        let sync_settings = Some(auth::get_sync(
            &client, 
            &sync_token, 
            &config.session_file
        ).await?);

        Ok(Self {
            client,
            bot_id,
            sync_settings: sync_settings.unwrap(),
            session_file: config.session_file.clone(),
        })
    }

    // ===== Docker Healthcheck =====
    /// Heartbeat
    fn start_healthcheck_heartbeat(&self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if let Err(e) = tokio::fs::write(HEALTHCHECK_PATH, "ok").await {
                    tracing::error!("❌ Failed to write healthcheck heartbeat: {}", e);
                }
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            }
        })
    }
    /// Delete heartbeat file
    async fn cleanup_healthcheck() {
        if let Err(e) = tokio::fs::remove_file(HEALTHCHECK_PATH).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("⚠️ Could not remove healthcheck file during cleanup: {}", e);
            }
        } else {
            tracing::info!("🧹 Healthcheck file removed successfully.");
        }
    }

    // ===== Sync =====
    /// Sync
    pub async fn sync(&self) -> Result<()> {
        let heartbeat_handle = self.start_healthcheck_heartbeat();

        tokio::select! {
            result = auth::sync(self.client.clone(), self.sync_settings.clone(), &self.session_file) => {
                result?;
            }
            _ = signal::ctrl_c() => {
                info!("🛑 The application is terminating...");
                // https://docs.rs/matrix-sdk/latest/matrix_sdk/struct.Client.html#method.sync
                // https://docs.rs/matrix-sdk/latest/matrix_sdk/sliding_sync/struct.SlidingSync.html#method.stop_sync
                // drop(client); or let _ = client;

                heartbeat_handle.abort();
                Self::cleanup_healthcheck().await;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
struct BotContext {
    pub client: Client,
    pub bot_id: OwnedUserId,
    pub db: Arc<Connection>,
    pub bot_config: Arc<config::BotConfig>,
    pub i18n_cache: Arc<RwLock<HashMap<String, Arc<I18nManager>>>>,
    pub db_ctx: Arc<DbContext>,
    pub settings_service: SettingsService,
}

impl BotContext {
    /// Get I18nManager for locale with lazyload.
    pub async fn get_i18n_manager(&self, locale: &str) -> Arc<I18nManager> {
        // Access to read. ReadGuard.
        {
            let cache = self.i18n_cache.read().await;
            if let Some(manager) = cache.get(locale) {
                return Arc::clone(manager);
            }
        }

        // Create it if we have not it. WriteGuard.
        let mut cache = self.i18n_cache.write().await;
        
        // Double-checked locking. 
        // While we were waiting for WriteGuard, another thread could have created the manager.
        cache.entry(locale.to_string())
            .or_insert_with(|| {
                Arc::new(I18nManager::new_for_locale(locale))
            })
            .clone()
    }
}

type SharedState = Arc<BotContext>;

pub struct BotManager {
    context: Arc<BotContext>,
}

impl BotManager {
    pub async fn new(runtime: &BotRuntime, config: &AppConfig) -> Result<Self> {
        let bot_config = Arc::new(config.bot.clone());
        let db = Arc::new(db::init_db(&config.data_dir).await?);
        let reminders = ReminderRepository::new(db.clone());
        let settings = SettingRepository::new(db.clone());
        let db_ctx = Arc::new(DbContext { reminders, settings });
        let settings_service = SettingsService::new(
            db_ctx.clone(), 
            bot_config.clone(), 
            runtime.bot_id.clone()
        );

        let context = Arc::new(BotContext {
            client: runtime.client.clone(),
            bot_id: runtime.bot_id.clone(),
            db,
            bot_config,
            i18n_cache: Arc::new(RwLock::new(HashMap::new())),
            db_ctx,
            settings_service,
        });

        Ok(Self { context })
    }

    pub async fn start(&self, runtime: &BotRuntime) -> Result<()> {
        // Restore reminders
        match reminder::restore_reminders(self.context.clone()).await {
            Ok(_) => (),
            Err(e) => tracing::error!("Restoring reminders failed with an error: {:?}", e)
        };

        // Register handlers
        self.register_handlers();

        // Start synchronization
        runtime.sync().await?;
        Ok(())
    }

    fn register_handlers(&self) {
        let ctx: SharedState = self.context.clone();
        let ctx_2: SharedState = self.context.clone();
        let ctx_3: SharedState = self.context.clone();

        self.context.client.add_event_handler(move |event, room| {
            handlers::on_room_message(event, room, ctx.clone())
        });
        self.context.client.add_event_handler(move |room_member, room| {
            handlers::on_stripped_state_member(room_member, room, ctx_2.clone())
        });
        self.context.client.add_event_handler(move |event, room| {
            handlers::on_reaction(event, room, ctx_3.clone())
        });    
    }
}

// ===== main =====
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    // variable is mutable because command Commands::SetupConfig 
    // can change it in config.bot.setup_config
    let mut config = AppConfig::load().await?;

    cli::run(&mut config).await?;

    Ok(())
}
