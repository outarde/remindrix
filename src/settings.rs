use matrix_sdk::ruma::{
    OwnedUserId, OwnedRoomId,
        events::{
        EmptyStateKey, macros::EventContent,
        }
    };
use serde::{Deserialize, Serialize};
use jiff::{tz::TimeZone, civil::Time};
use strum_macros::{Display, EnumString};
use thiserror::Error;

/// Error types for setting processing.
#[derive(Debug, Error)]
pub enum SettingError {
    #[error("error.db")] Db(#[from] tokio_rusqlite::Error),
    #[error("error.matrix")] MatrixError(#[from] matrix_sdk::Error),
    #[error("error.matrix")] WrongScope(String),
    #[error("settings.help")] NoCommand,
    #[error("error.lang.not-exist")] NoLanguage,
    #[error("error.lang.not-set")] LanguageNotSet,
    #[error("tz.invalid-format")] InvalidTzFormat,
    #[error("error.invalid-time-format")] InvalidTimeFormat,
}

// Matrix State Event for a time zone.
#[derive(Clone, Debug, Deserialize, Serialize, EventContent)]
#[ruma_event(type = "com.reminder-bot.room_timezone", kind = State, state_key_type = EmptyStateKey)]
pub struct RoomTimezoneContent {
    pub timezone: String,
}

#[derive(Clone, Debug)]
pub struct RoomSettings {
    pub room_id: OwnedRoomId,
    pub room_tz: TimeZone,
    pub room_tz_name: String,
    pub room_lang: String,
}

#[derive(Clone, Debug)]
pub struct UserSettings {
    pub default_time: Time,
    pub morning: Time,
    pub afternoon: Time,
    pub evening: Time,
}

#[derive(Clone, Debug)]
pub struct ActiveSettings {
    pub room: RoomSettings,
    pub user: UserSettings,
}

/*
/// Settings for room where bot was activated.
/// When we create CommandContext, we create it for a specific user interaction in the room,
/// when we create SettingsManager, we create it for the room with possible filtering by user.
#[derive(Clone, Debug)]
pub struct Settings {
    pub room_id: OwnedRoomId,
    // The necessity is questionable
    // pub user_id: Option<OwnedUserId>,
    pub room_tz: TimeZone,
    pub room_tz_name: String,
    pub room_lang: String,
    pub default_time: Time,
    pub morning: Time,
    pub afternoon: Time,
    pub evening: Time,
}
*/

#[derive(Debug, PartialEq, EnumString, Display)]
#[strum(serialize_all = "snake_case")]
pub enum SettingKey {
    Lang,
    Timezone,
    DefaultTime,
    Morning,
    Afternoon,
    Evening,
}

impl SettingKey {
    pub fn is_room_scoped(&self) -> bool {
        matches!(self, Self::Lang | Self::Timezone)
    }
    pub fn is_user_scoped(&self) -> bool {
        matches!(self, Self::DefaultTime | Self::Morning | Self::Afternoon | Self::Evening)
    }
}

pub enum SettingScope {
    Room,
    User(OwnedUserId),
}

pub struct SettingUpdate {
    pub key: SettingKey,
    pub value: String,
}

/// Structure for DB operations (DTO - Data Transfer Object).
#[derive(Clone, Debug)]
pub struct RawSetting {
    pub key: String,
    pub value: String,
}

/*
/// Lightweight structure for settings in ReminderData
#[derive(Clone, Debug)]
pub struct ReminderSettings {
    // pub room_id: OwnedRoomId,
    pub room_tz: TimeZone,
    pub room_lang: String,
}

// From SettingsManager to ReminderSettings
impl From<Settings> for ReminderSettings {
    fn from(manager: Settings) -> Self {
        Self {
            // room_id: manager.room_id,
            room_tz: manager.room_tz,
            room_lang: manager.room_lang,
        }
    }
}
*/
