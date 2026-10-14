use std::{sync::Arc, str::FromStr, collections::HashMap};
use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Room, ruma::{
        RoomId, OwnedRoomId, UserId, OwnedUserId
    }
};
use jiff::{
    Zoned, tz::TimeZone, Timestamp, Unit,
    civil::{DateTime as CivilDateTime, Date}
};
use anyhow::Result;
use strum_macros::EnumString;
use rust_i18n::t;

use crate::db::{DbContext, ReminderRepository};
use crate::config::BotConfig;
use crate::reminder::{RawReminder, Reminder, ReminderData, ReminderStatus, ReminderError, DelegationType};

#[derive(Clone, Debug, PartialEq, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum ListScopes {
    #[strum(ascii_case_insensitive)]
    Pending = 0,
    #[strum(ascii_case_insensitive)]
    Sent = 1,
    #[strum(ascii_case_insensitive)]
    Missed = 2,
}

/*
pub async fn get_list_for_page(
    reminder_repo: &ReminderRepository,
    room_id: &RoomId,
    scope: ListScopes,
    page: i32,
) -> Result<Vec<Reminder>> {
    let raw_reminders = reminder_repo.get_by_page(room_id, scope, page).await;
    let reminders = from_raw_reminders(raw_reminders)?;

    Ok(reminders)
}
*/

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

pub fn sort_by_date(reminders: Vec<Reminder>) -> HashMap<Date, Vec<Reminder>> {
    let mut sorted_reminders: HashMap<Date, Vec<Reminder>> = HashMap::new();

    for reminder in reminders {
        sorted_reminders
            .entry(reminder.data.utc_dt.to_zoned(reminder.data.tz.clone()).date().clone())
            .or_default()
            .push(reminder);
    }

    sorted_reminders
}

pub fn get_list_by_date(date_sorted_reminders: HashMap<Date, Vec<Reminder>>, locale: &str) -> Vec<String> {
    let mut list_messages = Vec::new();
    // let mut list_messages = Vec::with_capacity(date_sorted_reminders.values().map(|v| v.len()).sum());

    for (date, reminders) in date_sorted_reminders {
        // let mut sorted = reminders.clone();
        // sorted.sort_by_key(|r| r.data.civil_dt);

        let items = reminders
            .iter()
            .map(|r| {
                let date = r.data.civil_dt.strftime("%d.%m.%Y").to_string();
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
