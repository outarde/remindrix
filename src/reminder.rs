use matrix_sdk::{
    Client,
    ruma::{
        UserId, OwnedUserId, OwnedRoomId, RoomId,
        events::room::message::{RoomMessageEventContent},
    },
};
use tokio_rusqlite::{params, Connection};
use jiff::{
    Zoned, Span, ToSpan, tz::TimeZone, Timestamp, Unit,
    civil::{DateTime as CivilDateTime, Date}
};
use std::{
    path::PathBuf,
    sync::Arc, 
    collections::HashMap
};
use rust_i18n::t;
use anyhow::{Context, Result};
use strum_macros::{Display, EnumString};
use thiserror::Error;

use crate::handlers::CommandContext;
use crate::settings::{SettingsManager, ReminderSettings};

/// Structure for restoring reminders from DB.
struct RawReminder {
    id: i64,
    room_id: String,
    text: String,
    target_time: String,
    utc_time: String,
    tz: String,
    created_by: String,
}

/// Statuses of Reminder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ReminderStatus {
    Pending = 0,
    Sent = 1,
    // missed can be when bot was offline
    Missed = 2,
    // Recurring = 3,
    // Cancelled = 4,
}
impl From<i64> for ReminderStatus {
    fn from(value: i64) -> Self {
        ReminderStatus::try_from(value).unwrap_or(ReminderStatus::Pending)
    }
}

/// Keys of i18n for erros.
#[derive(Debug, Error)]
pub enum ReminderError {
    #[error("error.db: {0}")] Db(#[from] tokio_rusqlite::Error),
    #[error("error.month")] InvalidMonth,
    #[error("error.past-time")] TimeInPast,
    #[error("error.time")] InvalidTime,
    #[error("error.empty-text")] EmptyText,
    #[error("error.unsafe-datetime")] UnsafeDateTime,
    #[error("error.date-format")] InvalidDateFormat,
    #[error("error.time-format")] InvalidTimeFormat,
    #[error("error.datetime-format")] InvalidDateTimeFormat,
    #[error("error.delegation-room-format")] InvalidDelegationRoomFormat,
    #[error("error.delegation-no-room")] NoDelegatedRoom,
    #[error("tz.invalid-format")] InvalidTzFormat,
    #[error("Time error: {0}")] JiffError(#[from] jiff::Error), 
}

/// Reminder in UTC.
#[derive(Debug, Clone)]
pub struct ReminderUtc {
    pub id: i64,
    pub room_id: OwnedRoomId,
    pub text: String,
    pub target_time: CivilDateTime,
    pub utc_time: Timestamp,
    pub tz: TimeZone,
    pub status: ReminderStatus,
}

/// Reminder Structure with ReminderData.
#[derive(Debug, Clone)]
pub struct Reminder {
    pub id: i64,
    pub data: ReminderData,
    pub status: ReminderStatus,
}

/// ReminderData
#[derive(Debug, Clone)]
pub struct ReminderData {
    pub utc_dt: Timestamp,
    pub civil_dt: CivilDateTime,
    pub text: String,
    pub created_by: OwnedUserId,
    pub settings: ReminderSettings
}

// ===== Reminders =====
/// Schedule Reminder.
pub async fn schedule_reminder(
    ctx: Arc<super::BotContext>,
    reminder: Reminder,
) {
    let now = Timestamp::now();

    // Get Span.
    let span = now.until(reminder.data.utc_dt).unwrap_or(0.seconds());
    let seconds = span.total(Unit::Second).unwrap() as i64;

    if seconds <= 0 {
        tracing::error!("The reminder #{} time has already passed!", reminder.id);
        return;
    }

    let std_duration = std::time::Duration::from_secs(seconds as u64);

    // Tokio
    tokio::spawn(async move {
        tracing::info!("New reminder #{} in {} sec", reminder.id, std_duration.as_secs());
        
        // Asynchronic sleep
        tokio::time::sleep(std_duration).await;

        // After sleep
        let room = match ctx.client.get_room(&reminder.data.settings.room_id) {
            Some(r) => r,
            None => {
                tracing::warn!("Room {} was not found for reminder #{}", &reminder.data.settings.room_id, &reminder.id);
            
                if let Err(e) = update_reminder_status(ctx.db.clone(), reminder.id, ReminderStatus::Missed).await {
                    tracing::error!("Failed to update status for reminder: {:?}", e);
                }

                // End of the spawn.
                return;
            }
        };

        let reminder_text = t!(
            "reminder.new", 
            locale = &reminder.data.settings.room_lang, 
            text = reminder.data.text
        );

        // If the message was sent successfully, update the status!
        match room.send(RoomMessageEventContent::text_markdown(reminder_text)).await {
            Ok(_) => {
                if let Err(e) = update_reminder_status(ctx.db.clone(), reminder.id, ReminderStatus::Sent).await {
                    tracing::error!("Failed to update status for reminder: {:?}", e);
                }
            }
            Err(e) => tracing::info!("Reminder #{} was not sent due to an error: {}", reminder.id, e)
        }
    });
}

/// Restores all future (or missed) reminders from the database.
pub async fn restore_reminders(ctx: Arc<super::BotContext>) -> anyhow::Result<()> {
    // Get reminders
    let raw_reminders: Vec<RawReminder> = ctx.db.call(move |c| {
        let mut stmt = c.prepare("SELECT id, room_id, text, target_time, utc_time, tz, created_by FROM reminders WHERE status = 0")?;
        
        // Get rows with a RawReminder
        let mapped_rows = stmt.query_map([], |row| {
            Ok(RawReminder {
                id: row.get(0)?,
                room_id: row.get(1)?,
                text: row.get(2)?,
                target_time: row.get(3)?,
                utc_time: row.get(4)?,
                tz: row.get(5)?,
                created_by: row.get(6)?,
            })
        })?;

        let mut res = Vec::new();
        for row in mapped_rows {
            res.push(row?);
        }
        
        Ok::<_, tokio_rusqlite::Error>(res)
    }).await?;

    // Make a Vec for reminders.
    let mut reminders = Vec::with_capacity(raw_reminders.len());

    for raw in raw_reminders {
        let id: i64 = raw.id;
        let room_id_str = raw.room_id.to_string();
        let created_by_str = raw.created_by.to_string();
        let text = raw.text;
        let target_time_str = raw.target_time;
        let utc_time_str = raw.utc_time;
        let room_tz_str = raw.tz;

        // Parse strings to OwnedRoomId and OwnedUserId
        let room_id = RoomId::parse(&room_id_str)?;
        let created_by = UserId::parse(&created_by_str)?;

        // Get TZ
        let room_tz = super::settings::parse_tz_or_default(&room_tz_str, &ctx.bot_config.tz);

        // Parse Utc as Timestamp
        let timestamp: Timestamp = match utc_time_str.parse() {
            Ok(t) => t,
            // DateTime for the legacy (chrono) format of utc_time as 2026-09-06 04:00:00 UTC
            Err(_) => {
                let parts: Vec<&str> = utc_time_str.split(|c| c == '-' || c == ' ' || c == ':').collect();
                if parts.len() < 5 {
                    return Err(anyhow::anyhow!("Err"));
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
        let dt_zoned = timestamp.to_zoned(room_tz.clone());
        let civil_dt = dt_zoned.datetime();

        // Get lightweight ReminderSettings.
        let settings = ReminderSettings::load(room_id, room_tz, &ctx).await;

        // Prepare ReminderData.
        let reminder_data = ReminderData {
            utc_dt: timestamp,
            civil_dt,
            text,
            created_by,
            settings,
        };

        reminders.push(Reminder {
            id,
            data: reminder_data,
            status: ReminderStatus::Pending,
        });
    }

    // HashMap for missed reminders
    let mut missed_by_room: HashMap<OwnedRoomId, Vec<Reminder>> = HashMap::new();

    // Distribute reminders into scheduled and missed ones
    for reminder in reminders {
        if reminder.data.utc_dt > Timestamp::now() {
            schedule_reminder(
                ctx.clone(), 
                reminder.clone()
            ).await;
        } else {
            missed_by_room
                .entry(reminder.data.settings.room_id.clone())
                .or_default()
                .push(reminder);
        }
    }

    // If we have missed reminders in HashMap
    if missed_by_room.is_empty() {
        tracing::info!("No missed reminders");
        return Ok(());
    }

    let len = missed_by_room.len();
    
    summary_missed(ctx.clone(), missed_by_room).await?;
    tracing::info!("Sending missed reminders by room numbers: {}", len);

    Ok(())
}

/// Sending a summary of missed reminders for each room. 
/// Currently, this is only needed if the bot was offline and unable to send reminders.
async fn summary_missed(
    ctx: Arc<super::BotContext>,
    missed_by_room: HashMap<OwnedRoomId, Vec<Reminder>>
) -> anyhow::Result<()> {
    for (room_id, reminders) in missed_by_room {
        let ctx_clone = ctx.clone();

        tokio::spawn(async move {
            // If the room is unavailable, we mark the reminders as missed 
            // so they do not load every time the bot restarts.
            let room = match ctx_clone.client.get_room(&room_id) {
                Some(r) => r,
                None => {
                    tracing::warn!("Room {} was not found for the summary. All reminders in this room have been marked as missed", &room_id);
                    if let Err(e) = update_room_reminder_status(
                        ctx_clone.db.clone(), 
                        room_id.to_string(), 
                        ReminderStatus::Missed
                    ).await {
                        tracing::error!("Failed to update missed status for room {}: {:?}", room_id, e);
                    }

                    // End of the spawn.
                    return;
                }
            };

            // Sorting by time and combining into a summary, can also be numbered.
            let mut sorted = reminders.clone();
            sorted.sort_by_key(|r| r.data.civil_dt);

            let summary = sorted
                .iter()
                .map(|r| {
                    let date = r.data.civil_dt.strftime("%d.%m.%Y").to_string();
                    let time = r.data.civil_dt.strftime("%H:%M").to_string();
                    let sum = t!(
                        "reminder.list", 
                        locale = r.data.settings.room_lang.as_ref(), 
                        text = r.data.text, 
                        date = date, 
                        time = time
                    );
                    sum
                })
                .collect::<String>();

            // Prepare a final summary message.
            let message = t!(
                "reminder.missed", 
                locale = reminders[0].data.settings.room_lang.as_ref(), 
                sum = summary
            );

            // Update status for the sent reminders.
            if room.send(RoomMessageEventContent::text_markdown(message)).await.is_ok() {
                let ids: Vec<i64> = reminders.iter().map(|r| r.id).collect();
                tracing::info!("Missed reminders #{:#?} has been sent", ids);
                if let Err(e) = update_reminders_status(ctx_clone.db.clone(), ids, ReminderStatus::Sent).await {
                    tracing::error!("Failed to update sent status for room {}: {:?}", room_id, e);
                }
            }
        });
    }

    Ok(())
}

// ===== DB =====
/// Update all reminders in the room with the given status.
async fn update_room_reminder_status(
    db: Arc<Connection>, 
    room_id: String, 
    status: ReminderStatus
) -> Result<(), tokio_rusqlite::Error> {
    db.call(move |c| {
        c.execute("UPDATE reminders SET status = ?1 WHERE room_id = ?2", params![status as i64, &room_id])
    }).await?;
    Ok(())
}

/// Update reminders by ids with the given status.
async fn update_reminders_status(
    db: Arc<Connection>, 
    ids: Vec<i64>, 
    status: ReminderStatus
) -> Result<(), tokio_rusqlite::Error> {
    let placeholders: String = std::iter::repeat("?")
        .take(ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!("UPDATE reminders SET status = ?1 WHERE id IN ({})", placeholders);

    db.call(move |c| {        
        let params = tokio_rusqlite::params_from_iter(ids.iter());
        c.execute(&query, params)?;
        Ok(())
    }).await?;
    Ok(())
}

async fn update_reminder_status(
    db: Arc<Connection>, 
    id: i64, 
    status: ReminderStatus
) -> Result<(), tokio_rusqlite::Error> {
    db.call(move |c| {
        c.execute("UPDATE reminders SET status = ?1 WHERE id = ?2", [status as i64, id])?;
        Ok(())
    }).await?;
    Ok(())
}

// ===== Service =====
/// Check if reminder time as string can be parsed. Used in the cli.rs.
pub fn is_time_valid(time_str: &str, _time_format: &str) -> bool {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() != 2 { return false; }
    let h = parts[0].parse::<u8>().unwrap_or(99);
    let m = parts[1].parse::<u8>().unwrap_or(99);
    h < 24 && m < 60
}
