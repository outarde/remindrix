use matrix_sdk::{
    Client,
    ruma::{
        OwnedUserId, OwnedRoomId, RoomId,
        events::room::message::{RoomMessageEventContent},
    },
};
use tokio_rusqlite::Connection;
use chrono::{Local, TimeZone, NaiveDateTime, NaiveTime, DateTime, Utc};
use chrono_tz::Tz;
use std::{
    path::PathBuf,
    sync::Arc, 
    collections::HashMap
};
use rust_i18n::t;
use anyhow::{Context, Result};
use strum_macros::{Display, EnumString};

use crate::handlers::CommandContext;
use crate::parsers::ReminderData;

/// Reminder in UTC.
#[derive(Debug, Clone)]
pub struct ReminderUtc {
    pub id: i64,
    pub room_id: OwnedRoomId,
    pub text: String,
    pub target_time: NaiveDateTime,
    pub utc_time: DateTime<Utc>,
    pub tz: Tz,
    pub status: ReminderStatus,
}

/// New Reminder Structure.
#[derive(Debug, Clone)]
pub struct Reminder {
    pub id: i64,
    pub data: ReminderData,
    pub status: ReminderStatus,
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

/// Keys of i18n for reply in case of error.
#[derive(Debug, Display)]
pub enum ReminderError {
    #[strum(serialize = "reminder.error.month")]
    InvalidMonth,
    #[strum(serialize = "reminder.error.past-time")]
    TimeInPast,
    #[strum(serialize = "reminder.error.time")]
    InvalidTime,
    #[strum(serialize = "reminder.error.summer-time")]
    SummerTime,
    #[strum(serialize = "reminder.error.unsafe-datetime")]
    UnsafeDateTime,
    #[strum(serialize = "error.date-format")]
    InvalidDateFormat,
    #[strum(serialize = "error.time-format")]
    InvalidTimeFormat,
    #[strum(serialize = "error.datetime-format")]
    InvalidDateTimeFormat,
    #[strum(serialize = "error.delegation-room-format")]
    InvalidDelegationRoomFormat,
    #[strum(serialize = "error.delegation-no-room")]
    NoDelegatedRoom,
    #[strum(serialize = "error.db")]
    Db,
}

/*
impl TryFrom<i64> for ReminderStatus {
    type Error = String;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ReminderStatus::Pending),
            1 => Ok(ReminderStatus::Sent),
            2 => Ok(ReminderStatus::Missed),
            3 => Ok(ReminderStatus::Recurring),
            4 => Ok(ReminderStatus::Cancelled),
            _ => Err(format!("Unknown status: {}", value)),
        }
    }
}
*/
impl From<i64> for ReminderStatus {
    fn from(value: i64) -> Self {
        // TODO: change expect()
        ReminderStatus::try_from(value).expect("Invalid status in database")
    }
}

//ReminderStatus::try_from(db_value).unwrap_or(ReminderStatus::Pending);
//let status = ReminderStatus::from(db_value);

/// Database initialization.
pub async fn init_db(data_dir: &PathBuf) -> anyhow::Result<Connection> {
    // Path for DB file.
    let path = data_dir.join("remindrix.db");

    // Open or create DB file.
    let conn = Connection::open(&path).await?;
    
    // Create table.
    conn.call(|c| -> Result<(), tokio_rusqlite::Error> {
        let _ = c.execute(
            "CREATE TABLE IF NOT EXISTS reminders (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                room_id TEXT NOT NULL,
                text TEXT NOT NULL,
                target_time TEXT NOT NULL,
                utc_time TEXT NOT NULL,
                tz TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                created_by TEXT NOT NULL,
                status INTEGER DEFAULT 0
            )",
            [],
        );
        let _ = c.execute(
            "CREATE TABLE IF NOT EXISTS settings (
            room_id TEXT NOT NULL,
            user_id TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            updated_by TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (room_id, user_id, key)
            )",
            [],
        );
        let _ = c.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminders_status ON reminders(status)",
            [],
        );
        // Ok::<_, tokio_rusqlite::Error>(())
        // We can use OK(()) without turbo-fish if we specify
        // -> Result<(), tokio_rusqlite::Error> in function result.
        Ok(())
    }).await?;
    
    Ok(conn)
}

/// NEW Scheduling reminder.
pub async fn schedule_reminder(
    ctx: Arc<super::BotContext>,
    reminder: Reminder,
) {
    let now = Utc::now();

    let duration_to_wait = &reminder.data.utc_dt.signed_duration_since(now);

    if duration_to_wait.num_seconds() <= 0 {
        tracing::error!("The reminder #{} time has already passed!", reminder.id);
        return;
    }

    let std_duration = std::time::Duration::from_secs(duration_to_wait.num_seconds() as u64);

    // Tokio
    // TODO: move to the function?
    tokio::spawn(async move {
        tracing::info!("New reminder #{} in {} sec", reminder.id, std_duration.as_secs());
        
        // Asynchronic sleep
        tokio::time::sleep(std_duration).await;

        // After sleep
        if let Some(room) = ctx.client.get_room(&reminder.data.settings.room_id) {
            let reminder_text = t!("reminder.new", locale = &reminder.data.settings.room_lang, text = reminder.data.text);
            // If the message was sent successfully, update the status!
            match room.send(RoomMessageEventContent::text_markdown(reminder_text)).await {
                Ok(_) => {
                    let _ = ctx.db.call(move |c| {
                        c.execute("UPDATE reminders SET status = ?1 WHERE id = ?2", [ReminderStatus::Sent as i64, reminder.id])
                    }).await;
                    tracing::info!("Reminder #{} was sent", reminder.id);
                }
                Err(e) => tracing::info!("Reminder #{} was not sent due to an error: {}", reminder.id, e)
            }
        } else {
            tracing::warn!("Room {} was not found for reminder #{}", reminder.data.settings.room_id, reminder.id);
            let _ = ctx.db.call(move |c| {
                c.execute("UPDATE reminders SET status = ?1 WHERE id = ?2", [ReminderStatus::Missed as i64, reminder.id])
            }).await;
        }
    });
}

/// Scheduling reminder.
pub async fn schedule_reminder_utc(
    ctx: Arc<super::BotContext>,
    reminder: ReminderUtc,
) {
    // let utc_time = &reminder.utc_time; 
    let now = Utc::now();

    let duration_to_wait = &reminder.utc_time.signed_duration_since(now);

    if duration_to_wait.num_seconds() <= 0 {
        tracing::error!("The reminder time has already passed!");
        return;
    }

    let std_duration = std::time::Duration::from_secs(duration_to_wait.num_seconds() as u64);

    // Tokio
    tokio::spawn(async move {
        tracing::info!("New reminder in {} sec", std_duration.as_secs());
        
        // Asynchronic sleep
        tokio::time::sleep(std_duration).await;

        // After sleep
        if let Some(room) = ctx.client.get_room(&reminder.room_id) {
            let reminder_text = t!("reminder.new", text = reminder.text);
            let _ = room.send(RoomMessageEventContent::text_markdown(reminder_text)).await;
            // If the message was sent successfully, update the status!
            let _ = ctx.db.call(move |c| {
                c.execute("UPDATE reminders SET status = ?1 WHERE id = ?2", [ReminderStatus::Sent as i64, reminder.id])
            }).await;
            tracing::info!("Reminder #{} was sent", reminder.id);
        } else {
            tracing::warn!("Room {} was not found for reminder #{}.", reminder.room_id, reminder.id);
            let _ = ctx.db.call(move |c| {
                c.execute("UPDATE reminders SET status = ?1 WHERE id = ?2", [ReminderStatus::Missed as i64, reminder.id])
            }).await;
        }
    });
}

/// Restores all future (or missed) reminders from the database.
pub async fn restore_reminders(ctx: Arc<super::BotContext>) -> anyhow::Result<()> {
    // Get reminders

    // let db = ctx.db_conn;
    // let client = ctx.client;

    let reminders: Vec<ReminderUtc> = ctx.db.call(|c| {
        let mut stmt = c.prepare("SELECT id, room_id, text, target_time, utc_time, tz FROM reminders WHERE status = 0")?;
        
        let mapped_rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let room_id_str: String = row.get(1)?;
            let text: String = row.get(2)?;
            let target_time_str: String = row.get(3)?;
            let utc_time_str: String = row.get(4)?;
            let room_tz_str: String = row.get(5)?;
            let status = ReminderStatus::Pending;

            // Parse strings to matrix RoomId and NaiveDateTime
            let room_id = RoomId::parse(&room_id_str)
                .map_err(|err| tokio_rusqlite::rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
                
            let target_time = NaiveDateTime::parse_from_str(&target_time_str, "%Y-%m-%d %H:%M:%S")
                .map_err(|err| tokio_rusqlite::rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;

            // Timezone.
            // It is always checked before sending to the server because of
            // fetch_room_tz() in settings.rs, so there can be no errors.
            let tz = match super::settings::parse_tz(&room_tz_str) {
                Ok(tz) => tz,
                Err(err) => {
                    super::config::DEFAULT_TZ.parse::<Tz>().unwrap()
                }
            };

            // UTC Time
            let utc_time: DateTime<Utc> = utc_time_str.parse().unwrap();

            Ok(ReminderUtc {
                id,
                room_id,
                text,
                target_time,
                utc_time,
                tz,
                status,
            })
        })?;

        let mut res = Vec::new();
        for row in mapped_rows {
            res.push(row?);
        }
        
        Ok::<_, tokio_rusqlite::Error>(res)
    }).await?;

    // HashMap for missed reminders
    let mut missed_by_room: HashMap<OwnedRoomId, Vec<ReminderUtc>> = HashMap::new();

    // Distribute reminders into scheduled and missed ones
    for reminder in reminders {
        if reminder.utc_time > Utc::now() {
            schedule_reminder_utc(
                ctx.clone(), 
                reminder.clone()
            ).await;
        } else {
            missed_by_room
                .entry(reminder.room_id.clone())
                .or_default()
                .push(reminder);
        }
    }

    // If we have missed reminders in HashMap
    if !missed_by_room.is_empty() {
        tracing::info!("Sending missed reminders by room numbers: {}", missed_by_room.len());
        summary_missed(ctx.clone(), missed_by_room).await?;
    } else {
        tracing::info!("No missed reminders");
    }

    Ok(())
}

/// Sending a summary of missed reminders for each room. 
/// Currently, this is only needed if the bot was offline and unable to send reminders.
async fn summary_missed(
    ctx: Arc<super::BotContext>,
    missed_by_room: HashMap<OwnedRoomId, Vec<ReminderUtc>>
) -> anyhow::Result<()> {
    for (room_id, reminders) in missed_by_room {
        //let room_id_cloned = RoomId::parse(&room_id).clone()?;
        let client_clone = ctx.client.clone();
        let db_clone = ctx.db.clone();

        tokio::spawn(async move {
            if let Some(room) = client_clone.get_room(&room_id) {
                // Sorting by time and combining into a summary, can also be numbered.
                let mut sorted = reminders.clone();
                sorted.sort_by_key(|r| r.target_time);

                let summary = sorted
                    .iter()
                    .map(|r| {
                        let target_time_date = r.target_time.format("%d.%m.%Y").to_string();
                        let target_time_time = r.target_time.format("%H:%M").to_string();
                        let sum = t!("reminder.list", text = r.text, date = target_time_date, time = target_time_time);
                        sum
                        //format!("{} ({} {} {})", r.text, target_time_date, "at", target_time_time)
                    })
                    .collect::<String>();
                    //.collect::<Vec<_>>();
                    //.join("\n");

                let message = t!("reminder.missed", sum = summary);

                // todo urgent: change to UPDATE! one method!
                if room.send(RoomMessageEventContent::text_plain(message)).await.is_ok() {
                    let ids: Vec<i64> = reminders.iter().map(|r| r.id).collect();
                    let _ = db_clone.call(move |c| -> Result<(), tokio_rusqlite::Error> {
                        for id in ids {
                            c.execute("DELETE FROM reminders WHERE id = ?1", [id])?;
                            tracing::info!("Reminder #{} deleted from DB", id);
                        }
                        Ok(())
                    }).await;
                }
            }
        });
    }

    Ok(())
}

// ===== DB =====
/// Save reminder to DB
pub async fn save_reminder_to_db_utc(
    cmd_ctx: &CommandContext,
    text: String,
    naive_time: NaiveDateTime,
    utc_time: DateTime<Utc>,
) -> Result<ReminderUtc, tokio_rusqlite::Error> {
    let room_id_clone = cmd_ctx.settings.room_id.clone();
    let room_tz_clone = cmd_ctx.settings.room_tz.clone();

    let room_id_str = cmd_ctx.settings.room_id.to_string();
    let datetime_str = naive_time.format("%Y-%m-%d %H:%M:%S").to_string();
    let utc_str = utc_time.to_string();
    let tz_str = cmd_ctx.settings.room_tz.to_string();
    let created_by_str = cmd_ctx.user_id.to_string();
    
    cmd_ctx.ctx.db.call(move |c| {
        c.execute(
            "INSERT INTO reminders (room_id, text, target_time, utc_time, tz, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            [&room_id_str, &text, &datetime_str, &utc_str, &tz_str, &created_by_str],
        )?;
        
        let reminder_id = c.last_insert_rowid();
        // let parsed_room_id = RoomId::parse(&room_id_str)
        //    .map_err(|err| tokio_rusqlite::rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;

        Ok(ReminderUtc {
            id: reminder_id,
            room_id: room_id_clone,
            text,
            target_time: naive_time,
            utc_time,
            tz: room_tz_clone,
            status: ReminderStatus::Pending,
        })
    }).await
}

/// Save reminder to DB with extended arguments. 
// Was planned for delegation in the CLI processing.
pub async fn save_reminder_to_db_extended(
    db: Arc<Connection>,
    reminder: ReminderData
) -> Result<Reminder, tokio_rusqlite::Error> {
    let room_id_clone = reminder.settings.room_id.clone();
    let room_tz_clone = reminder.settings.room_tz.clone();

    let room_id_str = reminder.settings.room_id.to_string();
    let datetime_str = reminder.naive_dt.format("%Y-%m-%d %H:%M:%S").to_string();
    let utc_str = reminder.utc_dt.to_string();
    let tz_str = reminder.settings.room_tz.to_string();
    let created_by_str = reminder.created_by.to_string();
    // let text = reminder.text;
    
    db.call(move |c| {
        c.execute(
            "INSERT INTO reminders (room_id, text, target_time, utc_time, tz, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            [&room_id_str, &reminder.text, &datetime_str, &utc_str, &tz_str, &created_by_str],
        )?;
        
        let id = c.last_insert_rowid();
        // let parsed_room_id = RoomId::parse(&room_id_str)
        //    .map_err(|err| tokio_rusqlite::rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;

        Ok(Reminder {
            id,
            data: reminder,
            status: ReminderStatus::Pending,
        })
    }).await
}

/// Save reminder with ParsedReminder.
pub async fn process_saving(
    cmd_ctx: &CommandContext, 
    reminder: ReminderData,
) -> Result<(), tokio_rusqlite::Error> {
    match save_reminder_to_db_extended(cmd_ctx.ctx.db.clone(), reminder).await {
        Ok(new_reminder) => schedule_reminder(cmd_ctx.ctx.clone(), new_reminder).await,
        Err(err) => return Err(err)
    }

    Ok(())
}

// ===== Service =====
/// Check if reminder time as string can be parsed to NaiveTime
pub fn is_time_valid(time_str: &str, time_format: &str) -> bool {
    NaiveTime::parse_from_str(time_str, time_format).is_ok()
}
