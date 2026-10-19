use std::{sync::Arc, str::FromStr, collections::HashMap};
use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Room, ruma::{
        OwnedEventId,
        RoomId, OwnedRoomId, UserId, OwnedUserId
    }
};
use jiff::{
    Zoned, tz::TimeZone, Timestamp, Unit,
    civil::{DateTime as CivilDateTime, Date}
};
use anyhow::Result;
use strum_macros::{EnumString, Display};
use serde::{Deserialize, Serialize};
use rust_i18n::t;

use crate::context::CommandContext;
use crate::db::{DbContext, ReminderRepository};
use crate::config::BotConfig;
use crate::reminder::{RawReminder, Reminder, ReminderData, ReminderStatus, ReminderError, DelegationType};

/// Reminders filters/scopes. Can be expanded by options "all" or "assigned".
/// Grouping is not featured.
#[derive(Clone, Debug, PartialEq, EnumString, Serialize, Deserialize)]
#[strum(serialize_all = "snake_case")]
pub enum ListScopes {
    #[strum(ascii_case_insensitive)]
    Pending = 0,
    #[strum(ascii_case_insensitive)]
    Sent = 1,
    #[strum(ascii_case_insensitive)]
    Missed = 2,
}

/// Order for list.
#[derive(Clone, Debug, PartialEq, EnumString, Serialize, Deserialize, Display)]
#[strum(serialize_all = "UPPERCASE")]
pub enum ListOrder {
    #[strum(ascii_case_insensitive)]
    Asc,
    #[strum(ascii_case_insensitive)]
    Desc,
}

/// Key for pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, Display)]
#[strum(serialize_all = "snake_case")]
pub enum StateKey {
    #[strum(ascii_case_insensitive)]
    Page,
    #[strum(ascii_case_insensitive)]
    ListButtonNext,
    #[strum(ascii_case_insensitive)]
    ListButtonPrev,
    // Filter,
    // SelectedItem,
}

/*
impl StateKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            StateKey::Page => "page",
            StateKey::ListButtonNext => "list_button_next",
            StateKey::ListButtonPrev => "list_button_prev",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "page" => Some(StateKey::Page),
            "list_button_next" => Some(StateKey::ListButtonNext),
            "list_button_prev" => Some(StateKey::ListButtonPrev),
            _ => None,
        }
    }
}
*/

/// State of the current list of reminders. Bound to message with a list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageState { 
    pub scope: ListScopes, 
    pub page: i32, 
    pub size: i32,
    pub order: ListOrder,
}

/// Reaction-button event_id (not the message it realated to).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonState {
    pub event_id: OwnedEventId,
}

/// List statistics.
#[derive(Debug)]
pub struct ReminderStats {
    pub total: i64,
    pub by_status: Vec<StatusCount>,
}

#[derive(Debug)]
pub struct StatusCount {
    pub status: ReminderStatus,
    pub count: i64,
}

pub struct RawStatRow {
    pub status_val: i64,
    pub count: i64,
    pub total: i64,
}

/// Get raw reminders by the one scope filter.
pub async fn get_raw_list(
    reminder_repo: &ReminderRepository,
    room_id: &RoomId,
    page_state: PageState,
) -> Result<Vec<RawReminder>, ReminderError> {
    let raw_reminders = reminder_repo.get_by_page(room_id, page_state.clone()).await?;
    
    // If there is no reminders, send an error depending on whether there are no reminders or they have run out.
    if raw_reminders.is_empty() {
        if page_state.page > 1 {
            return Err(ReminderError::ListEnded.into())
        }
        match page_state.scope {
            ListScopes::Pending => return Err(ReminderError::ListEmptyAffectionately.into()),
            _ => return Err(ReminderError::ListEmpty.into())
        }
    }

    Ok(raw_reminders)
}

/// RawReminder to Reminder
pub fn from_raw_reminders(ctx: Arc<super::BotContext>, raw_reminders: Vec<RawReminder>) -> Result<Vec<Reminder>> {
    // Make a Vec for reminders.
    let mut reminders = Vec::with_capacity(raw_reminders.len());

    for raw in raw_reminders {
        let id: i64 = raw.id;
        let room_id_str = raw.room_id.to_string();
        let created_by_str = raw.created_by.to_string();
        let text = raw.text;
        let utc_time_str = raw.utc_time;
        let room_tz_str = raw.tz;

        // Parse strings to OwnedRoomId and OwnedUserId
        let room_id = RoomId::parse(&room_id_str)?;
        let created_by = UserId::parse(&created_by_str)?;

        // Delegation data
        let delegation = match DelegationType::from_raw(&raw.delegation_kind, raw.target_user_id.as_deref()) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    reminder_id = raw.id,
                    delegation_kind = %raw.delegation_kind,
                    target_user = ?raw.target_user_id,
                    "invalid delegation in DB, falling back to Personal: {e}"
                );
                DelegationType::Personal
            }
        };

        // Get TZ
        let tz = ctx.settings_service.get_tz_or_fallback(Some(room_tz_str));

        // Parse Utc as Timestamp
        let timestamp: Timestamp = match utc_time_str.parse() {
            Ok(t) => t,
            // DateTime for the legacy (chrono) format of utc_time as 2026-09-06 04:00:00 UTC
            Err(_) => {
                let parts: Vec<&str> = utc_time_str.split(|c| c == '-' || c == ' ' || c == ':').collect();
                if parts.len() < 5 {
                    return Err(anyhow::anyhow!("Error restoring reminders with the legacy date format; delete the database to fix this."));
                }

                let target_time = CivilDateTime::new(
                    parts[0].parse().unwrap(), 
                    parts[1].parse().unwrap(), 
                    parts[2].parse().unwrap(),
                    parts[3].parse().unwrap(), 
                    parts[4].parse().unwrap(),
                    0,
                    0
                )?;

                let utc_time: Zoned = target_time.to_zoned(TimeZone::UTC)?;
                utc_time.timestamp()
            }
        };

        // Parse Civil from Timestamp -> Zoned.
        // let target_zoned = timestamp.in_tz(&room_tz_str);
        let dt_zoned = timestamp.to_zoned(tz.clone());
        let civil_dt = dt_zoned.datetime();

        // Prepare ReminderData.
        let reminder_data = ReminderData {
            room_id,
            utc_dt: timestamp,
            civil_dt,
            tz,
            text,
            created_by,
            delegation,
        };

        reminders.push(Reminder {
            id,
            data: reminder_data,
            status: ReminderStatus::Pending,
        });
    }

    Ok(reminders)
}

/// Group Vec<Reminder> to HashMap with Date as a key.
pub fn group_by_date(reminders: Vec<Reminder>) -> HashMap<Date, Vec<Reminder>> {
    let mut sorted_reminders: HashMap<Date, Vec<Reminder>> = HashMap::new();

    for reminder in reminders {
        sorted_reminders
            .entry(reminder.data.utc_dt.to_zoned(reminder.data.tz.clone()).date().clone())
            .or_default()
            .push(reminder);
    }

    sorted_reminders
}

/// Get list as a Vec<String> summary of sorted by date reminders.
pub fn get_summary_for_dates(date_sorted_reminders: HashMap<Date, Vec<Reminder>>, locale: &str) -> Vec<String> {
    let mut list_messages = Vec::new();
    // let mut list_messages = Vec::with_capacity(date_sorted_reminders.values().map(|v| v.len()).sum());

    for (date, reminders) in date_sorted_reminders {
        // let mut sorted = reminders.clone();
        // sorted.sort_by_key(|r| r.data.civil_dt);

        let items = reminders
            .iter()
            .map(|r| {
                let time = r.data.civil_dt.strftime("%H:%M").to_string();
                let item = t!(
                    "list.by-date.item", 
                    locale = locale, 
                    text = r.data.text,
                    time = time
                );
                item
            })
            .collect::<String>();

        // Prepare a final summary message.
        let message = t!(
            "list.by-date.section", 
            locale = locale,
            date = date.strftime("%d.%m.%Y"),
            items = items
        );

        list_messages.push(message.to_string());
    }

    list_messages
}

/// Handles the entire process of retrieving the necessary reminders, grouping them, and returning the message.
pub async fn get_list_message_by_date(
    raw_reminders: Vec<RawReminder>,
    page_state: PageState,
    cmd_ctx: &CommandContext
) -> Result<String, ReminderError> {
    // Get Reminder's and sort them by date (to a HashMap), then get localized summary list items.
    // let raw_reminders = get_raw_list(&cmd_ctx.ctx.db_ctx.reminders, cmd_ctx.room.room_id(), page_state.clone()).await?;
    let reminders = from_raw_reminders(cmd_ctx.ctx.clone(), raw_reminders).map_err(|_| ReminderError::ListError)?;
    let reminders_count = reminders.len();

    let sorted_reminders = group_by_date(reminders);
    let list = get_summary_for_dates(sorted_reminders, &cmd_ctx.settings.room.room_lang);

    // Form a message.
    let scope_localized = t!(format!("list.scope-short.{}", page_state.scope.clone() as i32), locale = &cmd_ctx.settings.room.room_lang);
    let order_localized = t!(format!("list.order.{}", page_state.order.to_string()), locale = &cmd_ctx.settings.room.room_lang);

    let message = t!(
        "list.by-date", 
        locale = &cmd_ctx.settings.room.room_lang,
        scope_short = scope_localized,
        count = reminders_count,
        size = page_state.size,
        page = page_state.page,
        order = order_localized,
        list = list.join("\n")
    );
    
    Ok(message.to_string())
}
