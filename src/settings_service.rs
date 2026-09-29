use std::{sync::Arc, str::FromStr};
use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Room, ruma::{
        RoomId, OwnedRoomId, UserId, OwnedUserId
    }
};
use jiff::{tz::TimeZone, civil::Time};

use crate::{db::DbContext, config::BotConfig};
use crate::settings::{
    SettingError,
    ActiveSettings, RoomSettings, UserSettings, 
    SettingUpdate, SettingKey, RawSetting, 
    RoomTimezoneContent
};

#[derive(Clone, Debug)]
pub struct SettingsService {
    db: Arc<DbContext>,
    config: Arc<BotConfig>,
    // bot_id: OwnedUserId,
}

impl SettingsService {
    pub fn new(db: Arc<DbContext>, config: Arc<BotConfig>, _bot_id: OwnedUserId) -> Self {
        Self { db, config }
    }

    /// Loading settings for room
    pub async fn load_room(&self, room_id: &RoomId, room: Option<&Room>) -> RoomSettings {
        let raw_settings = self.db.settings.get_room_settings(room_id)
            .await
            .unwrap_or_default();

        /*
        if let Some(r) = room {
            if let Some(tz) = Self::fetch_room_tz(r).await {
                raw_settings.push(RawSetting { key: SettingKey::Timezone.to_string(), value: tz.into() });
            }
        };
        */
        
        self.build_room_settings(room_id.to_owned(), room, raw_settings).await
    }

    /// Loading settings for user
    pub async fn load_user(&self, user_id: &UserId) -> UserSettings {
        let raw_settings = self.db.settings.get_user_settings(user_id)
            .await
            .unwrap_or_default();

        self.build_user_settings(raw_settings).await
    }

    /// Loading all settings
    pub async fn load_active(&self, room_id: &RoomId, room: Option<&Room>, user_id: &UserId) -> ActiveSettings {
        let (room_s, user_s) = tokio::join!(
            self.load_room(room_id, room),
            self.load_user(user_id),
        );

        ActiveSettings { room: room_s, user: user_s }
    }

    // Builder for room settings
    async fn build_room_settings(&self, room_id: OwnedRoomId, room: Option<&Room>, raw: Vec<RawSetting>) -> RoomSettings {
        let mut tz = None;
        let mut lang = None;
        for s in raw {
            match SettingKey::from_str(&s.key) {
                Ok(SettingKey::Timezone) => tz = Some(s.value),
                Ok(SettingKey::Lang) => lang = Some(s.value),
                Ok(_) => tracing::warn!("User-scoped key in room_settings: {}", s.key),
                Err(_) => tracing::warn!("Unknown setting key: {}", s.key),
            }
        }
        // Attempting to retrieve time zone via Matrix State Event
        if !tz.is_some() && let Some(r) = room {
            tz = Self::fetch_room_tz(r).await;
        }
        let room_tz = self.parse_input_tz_or_default(tz).await;
        RoomSettings { 
            room_id, 
            room_tz_name: room_tz.iana_name().unwrap_or("UTC").into(), 
            room_tz, 
            room_lang: lang.unwrap_or_else(|| self.config.lang.clone()) 
        }
    }
    // Builder for user settings
    async fn build_user_settings(&self, raw: Vec<RawSetting>) -> UserSettings {
        let mut default_time = None;
        let mut morning = None;
        let mut afternoon = None;
        let mut evening = None;
        for s in raw {
            match SettingKey::from_str(&s.key) {
                Ok(SettingKey::DefaultTime) => default_time = Some(s.value),
                Ok(SettingKey::Morning) => morning = Some(s.value),
                Ok(SettingKey::Afternoon) => afternoon = Some(s.value),
                Ok(SettingKey::Evening) => evening = Some(s.value),
                Ok(_) => tracing::warn!("Room-scoped key in user_settings: {}", s.key),
                Err(_) => tracing::warn!("Unknown setting key: {}", s.key),
            }
        }
        UserSettings {
            default_time: self.parse_time_or_default(default_time, &self.config.default_time),
            morning: self.parse_time_or_default(morning, &self.config.morning),
            afternoon: self.parse_time_or_default(afternoon, &self.config.afternoon),
            evening: self.parse_time_or_default(evening, &self.config.evening),
        }
    }

    // ===== Update Settings =====
    /// Set settings via Vec with key and value as a String.
    pub async fn set_room_settings(
        &self,
        room_id: &RoomId,
        updates: Vec<SettingUpdate>,
        updated_by: &UserId,
    ) -> Result<(), SettingError> {
        let mut raw = Vec::with_capacity(updates.len());
        for u in updates {
            if !u.key.is_room_scoped() {
                return Err(SettingError::WrongScope);
            }
            raw.push(RawSetting { key: u.key.to_string(), value: u.value });
        }
        self.db.settings.update_room_settings(room_id, raw, updated_by).await
    }

    // SettingUpate to RawSetting
    fn _to_raw_settings(&self, updates: Vec<SettingUpdate>) -> Vec<RawSetting> {
        updates.into_iter().map(|upd| RawSetting {
            key: upd.key.to_string(),
            value: upd.value,
        }).collect()
    }

    pub async fn set_user_settings(
        &self,
        user_id: &UserId,
        updates: Vec<SettingUpdate>,
        updated_by: &UserId,
    ) -> Result<(), SettingError> {
        let mut raw = Vec::with_capacity(updates.len());
        for u in updates {
            if !u.key.is_user_scoped() {
                tracing::error!("Wrong scope for the setting key: {}", u.key);
                return Err(SettingError::WrongScope);
            }
            raw.push(RawSetting { key: u.key.to_string(), value: u.value });
        }
        let result = self.db.settings.update_user_settings(user_id, raw, updated_by).await?;
        Ok(result)
    }

    /// Special wrapper for time zone settings which updates it using Matrix Custom Events
    /// and sends then to convenience set_settings().
    pub async fn set_matrix_tz(
        &self,
        room: &Room,
        tz_name: &str,
    ) -> Result<(), SettingError> {
        // let tz_name = tz.iana_name().ok_or(SettingError::InvalidTzFormat)?.to_string();

        // Prepare Matrix State Event.
        let content = RoomTimezoneContent {
            timezone: tz_name.to_string(),
        };
        // Save as a custom state.
        // let state_key = client.user_id().unwrap().to_string(); 
        room.send_state_event(content).await?;

        Ok(())

        /*
        // Send to the convenience set_settings method.
        let new_setting = SettingUpdate {
            key: SettingKey::Timezone,
            value: tz_name.clone(),
        };
        let _ = self.set_room_settings(
            room_id,
            vec![new_setting],
            updated_id,
        ).await?;

        Ok(tz_name)
        */
    }

    //===== Parsers and Validators =====
    /// Parse user input to Tz
    pub fn parse_tz(tz_str: &str) -> Result<TimeZone, SettingError> {
        TimeZone::get(tz_str).map_err(|_| SettingError::InvalidTzFormat)
    }
    /// Return parsed Timezone from &tz_str, or BotConfig &tz, or DEFAULT_TZ.
    pub fn parse_tz_or_default(&self, tz_str: &str) -> TimeZone {
        match TimeZone::get(tz_str) {
            Ok(t) => t,
            Err(_) => {
                TimeZone::get(&self.config.tz)
                    .unwrap_or_else(|_| {
                        tracing::warn!("Failed to parse config time zone, falling back to default {}", super::config::DEFAULT_TZ);
                        TimeZone::get(super::config::DEFAULT_TZ).unwrap()
                    })
            }
        }
    }

    /// Parse Option<String> to TimeZone.
    pub async fn parse_input_tz_or_default(&self, tz_str: Option<String>) -> TimeZone {
        // From str
        if let Some(t) = tz_str.as_ref().and_then(|s| TimeZone::get(s).ok()) {
            return t;
        }
        if tz_str.is_some() {
            tracing::warn!("Invalid timezone in DB, trying Matrix...", );
        }

        // From config
        if let Ok(t) = TimeZone::get(&self.config.tz) {
            return t;
        }

        // From constant value
        tracing::error!("Invalid default timezone in config: {}, falling back to default {}: ", &self.config.tz, super::config::DEFAULT_TZ);
        TimeZone::get(super::config::DEFAULT_TZ).unwrap()
    }

    /// Parse Option<String> to Time -> or return default -> or DEFAULT_MORNING_TIME.
    pub fn parse_time_or_default(&self, time_str: Option<String>, default: &str) -> Time {
        if let Some(ref s) = time_str {
            if let Ok(t) = s.parse::<Time>() {
                return t;
            }
            tracing::warn!("Invalid time format in DB: {}, falling back to config", s);
        }

        if let Ok(t) = default.parse::<Time>() {
            return t;
        }

        tracing::error!("Invalid default time in config: {}, falling back to default {}: ", default, super::config::DEFAULT_MORNING_TIME);
        super::config::DEFAULT_MORNING_TIME.parse::<Time>().unwrap()
    }

    // ===== Matrix State Event =====
    /// Fetch time zone for the room by Matrix State Event.
    pub async fn fetch_room_tz(room: &Room) -> Option<String> {
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
                    // TimeZone::get(&original.content.timezone).ok()
                    Some(original.content.timezone.to_string())
                } else {
                    tracing::warn!("Redacted timezone can not be viewed.");
                    None
                }
            } else { None }
        } else {
            None
        }
    }
}
