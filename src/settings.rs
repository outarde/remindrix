use std::{
    sync::Arc
};
use anyhow::{Result, Context, anyhow};
use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Room,
    ruma::{
    OwnedUserId, OwnedRoomId,
        events::{
        EmptyStateKey, macros::EventContent, 
        room::message::RoomMessageEventContent
        }
    }
};
use serde::{Deserialize, Serialize};
use tokio_rusqlite::{params, Connection};

use jiff::{
    Zoned, Span, ToSpan, tz::TimeZone, Timestamp,
    civil::{DateTime as CivilDateTime, Date}
};
use crate::reminder::{ReminderError};
use crate::handlers::{I18nManager, CommandContext, CliError};

#[derive(Clone, Debug, Deserialize, Serialize, EventContent)]
#[ruma_event(type = "com.reminder-bot.room_timezone", kind = State, state_key_type = EmptyStateKey)]
pub struct RoomTimezoneContent {
    pub timezone: String,
}

/// Settings for room where bot was activated.
// When we create CommandContext, we create it for a specific user interaction in the room,
// when we create SettingsManager, we create it for the room with possible filtering by user.
// (The necessity of the latter is questionable.)
#[derive(Clone, Debug)]
pub struct SettingsManager {
    // db: Arc<Connection>,
    // room_id, key, value, updated_by, _at
    pub room_id: OwnedRoomId,
    pub user_id: Option<OwnedUserId>, // The necessity is questionable
    pub room_tz: TimeZone,
    pub room_tz_name: String,
    pub room_lang: String
}

/// Lightweight structure for settings in ReminderData
#[derive(Clone, Debug)]
pub struct ReminderSettings {
    pub room_id: OwnedRoomId,
    pub room_tz: TimeZone,
    pub room_lang: String,
}

impl ReminderSettings {
    pub async fn load(
        room_id: OwnedRoomId, 
        room_tz: TimeZone, 
        ctx: &Arc<super::BotContext>
    ) -> Self {
        let room_lang = get_setting("lang", None, &room_id, ctx).await
            .unwrap_or_else(|| ctx.bot_config.lang.clone());

        Self {
            room_id,
            room_tz,
            room_lang,
        }
    }
}

// From SettingsManager to ReminderSettings
impl From<SettingsManager> for ReminderSettings {
    fn from(manager: SettingsManager) -> Self {
        Self {
            room_id: manager.room_id,
            room_tz: manager.room_tz,
            room_lang: manager.room_lang,
        }
    }
}

impl SettingsManager {
    pub async fn new(
        room: &Room, 
        user_id: Option<OwnedUserId>, 
        ctx: &Arc<super::BotContext>
    ) -> Self {
        /*
        let mut settings = Self {
            room_tz: Tz::UTC,
            room_lang: ctx.bot_config.lang
        };

        settings.fetch_room_tz().await;
        settings
        */

        let room_id = room.room_id().to_owned();

        // TODO: fill the whole structure at once.
        let room_tz = Self::fetch_room_tz(room, ctx).await;
        let room_tz_name = room_tz.iana_name().unwrap().to_string();
        let room_lang = match get_setting("lang", user_id.clone(), &room_id, ctx).await {
            Some(v) => v,
            None => ctx.bot_config.lang.clone()
        };

        /*
        let user_id = match user_id {
            Some(u) => u,
            None => None
        }*/

        Self { room_id, user_id, room_tz, room_tz_name, room_lang }
    }

    /// Get timezone for the room.
    pub async fn fetch_room_tz(room: &Room, ctx: &Arc<super::BotContext>) -> TimeZone {
        let default_tz = parse_tz_or_default(&ctx.bot_config.tz, &ctx.bot_config.tz);

        // let tz = Self::get_setting("timezone", user_id, room, ctx).await

        // Raw JSON: Option<Raw<StateEvent<C>>>
        if let Ok(Some(raw)) = room.get_state_event_static::<RoomTimezoneContent>().await {
            // Pattern matching to Sync variant, not Stripped. SyncStateEvent
            // https://docs.rs/matrix-sdk/latest/matrix_sdk/deserialized_responses/enum.SyncOrStrippedState.html
            // Instead of pattern matching we can use Ok(state) = raw.deserialize(), 
            // where state: SyncOrStrippedState<RoomTimezoneContent> and then
            // as_sync() to get SyncStateEvent.
            if let Ok(SyncOrStrippedState::Sync(sync_event)) = raw.deserialize() {
                // Check if it is not Redacted
                // https://docs.rs/ruma-events/0.34.0/ruma_events/enum.SyncStateEvent.html
                if let Some(original) = sync_event.as_original() {
                    parse_tz(&original.content.timezone).unwrap_or(default_tz)
                } else {
                    tracing::warn!("Redacted timezone can not be viewed.");
                    return default_tz;
                }
            } else { return default_tz; }
        } else {
            return default_tz;
        }
    }

    /// Set timezone for the room using Matrix custom events.
    pub async fn set_room_tz(
        // &mut self,
        &self,
        cmd_ctx: &CommandContext,
        tz: TimeZone,
    ) -> anyhow::Result<String> {
        // We can update it if we'll create it mutable in CommandContext.
        // self.room_tz = tz;

        // TimeZone has been parsed and can't panic
        // let tz_name = tz.iana_name().ok_or(ReminderError::InvalidTzFormat)?.to_string();
        let tz_name = tz.iana_name().unwrap().to_string();

        let content = RoomTimezoneContent {
            timezone: tz_name.clone(),
        };

        // Save as a custom state.
        // let state_key = client.user_id().unwrap().to_string(); 
        cmd_ctx.room.send_state_event(content).await?;

        // Save to DB.
        let room_id = self.room_id.to_string();
        let user_id = match &self.user_id {
            Some(u) => u.to_string(),
            None => cmd_ctx.user_id.to_string()
        };
        let tz_name_clone = tz_name.clone();

        let _ = cmd_ctx.ctx.db.call(move |c| -> Result<(), tokio_rusqlite::Error> {
            c.execute(
                "INSERT INTO settings (room_id, user_id, key, value, updated_by, updated_at) 
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(room_id, user_id, key) 
                DO UPDATE SET value=excluded.value, updated_by=excluded.updated_by, updated_at=excluded.updated_at;",
                [&room_id, &user_id, "timezone", &tz_name_clone, &user_id, &Timestamp::now().to_string()]
            )?;
            
            // We can use OK(()) without turbo-fish if we specify
            // -> Result<(), tokio_rusqlite::Error> in function result.
            Ok::<_, tokio_rusqlite::Error>(())
            
        }).await;

        Ok(tz_name)
    }

    /*
    /// Universal get
    pub async fn get_room_setting<T>(&self, room: &Room, default: T) -> T 
    where T: DeserializeOwned + Clone {
        // State event -> default
        return;
    }

    /// Univeral set
    pub async fn set_room_setting<T>(&self, room: &Room, content: T) -> Result<()> 
    where T: EventContent {
        // room.send_state_event(content).await.map_err(|e| e.into())
        return;
    }
    */
}

/// Get the setting of the room and then the user.
pub async fn get_setting(
    key: &str,
    user_id: Option<OwnedUserId>, 
    room_id: &OwnedRoomId,
    ctx: &Arc<super::BotContext>
) -> Option<String> {
    let room_id = room_id.to_string();
    let key = key.to_string();
    // Modify the query based on the presence of the user ID.
    let (statement, params) = match user_id {
        Some(u) => {
            let user_id = u.to_string();
            let st = "SELECT value FROM settings WHERE room_id = ?1 AND user_id =?2 AND key = ?3";
            (st, vec![room_id, user_id, key])
        }
        None => {
            let st = "SELECT value FROM settings WHERE room_id = ?1 AND key = ?3 ORDER BY updated_at LIMIT 1";
            (st, vec![room_id, key])
        }
    };

    let value = ctx.db.call(move |c| /*-> Result<String, tokio_rusqlite::Error>*/ {
        let value = c.query_row(
            statement,
            tokio_rusqlite::params_from_iter(params.iter()),

            |row| {
                let value: String = row.get(0)?;
                Ok(value)
            },
        )?;

        Ok::<_, tokio_rusqlite::Error>(value)
        
    }).await;

    value.ok()
}

/// Parse user input to Tz
pub fn parse_tz(tz_str: &str) -> Result<TimeZone, ReminderError> {
    TimeZone::get(tz_str).map_err(|_| ReminderError::InvalidTzFormat)
}

/// Return parsed Timezone from &tz_str, or BotConfig &tz, or DEFAULT_TZ.
pub fn parse_tz_or_default(tz_str: &str, config_tz: &str) -> TimeZone {
    match TimeZone::get(tz_str) {
        Ok(t) => t,
        Err(_) => {
            TimeZone::get(&config_tz)
                .unwrap_or_else(|_| {
                    tracing::warn!("Default time zone from config.yaml is invalid");
                    TimeZone::get(super::config::DEFAULT_TZ).unwrap()
                })
        }
    }
}
