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
        let room_tz = self.get_tz_or_fallback(tz).await;
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
            default_time: self.get_time_or_fallback(default_time, SettingKey::DefaultTime),
            morning: self.get_time_or_fallback(morning, SettingKey::Morning),
            afternoon: self.get_time_or_fallback(afternoon, SettingKey::Afternoon),
            evening: self.get_time_or_fallback(evening, SettingKey::Evening),
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

    // ===== Parsers and Validators =====
    // ===== TimeZone =====
    pub async fn get_tz_or_fallback(&self, tz_str: Option<String>) -> TimeZone {
        if let Some(tz) = Self::parse_optional_tz(tz_str.clone()) {
            return tz;
        }

        tracing::warn!(
            "Failed to parse tz '{:?}'. Using fallback.", 
            tz_str
        );

        self.get_fallback_tz()
    }

    pub fn parse_optional_tz(tz_str: Option<String>) -> Option<TimeZone> {
        TimeZone::get(tz_str.as_deref()?).ok()
    }

    pub fn parse_tz(tz_str: &str) -> Result<TimeZone, SettingError> {
        TimeZone::get(tz_str).map_err(|_| SettingError::InvalidTzFormat)
    }

    pub fn get_fallback_tz(&self) -> TimeZone {
        TimeZone::get(&self.config.tz).unwrap()
    }

    // ===== Time =====
    pub fn get_time_or_fallback(&self, time_str: Option<String>, key: SettingKey) -> Time {
        if let Some(time) = self.parse_optional_time(time_str.clone()) {
            return time;
        }

        tracing::warn!(
            "Failed to parse time for key {:?}: '{:?}'. Using fallback.", 
            key, time_str
        );

        self.get_fallback_time(key)
    }

    fn parse_optional_time(&self, time_str: Option<String>) -> Option<Time> {
        time_str?.parse::<Time>().ok()
    }

    pub fn get_fallback_time(&self, key: SettingKey) -> Time {
        let time = match key {
            SettingKey::Morning => &self.config.morning,
            SettingKey::Afternoon => &self.config.afternoon,
            SettingKey::Evening => &self.config.evening,
            _ => &self.config.default_time,
        };

        time.parse::<Time>().unwrap()
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
