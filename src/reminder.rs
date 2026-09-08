use matrix_sdk::{
    Client,
    ruma::{
        OwnedUserId, OwnedRoomId, RoomId,
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

use crate::handlers::CommandContext;
use crate::settings::SettingsManager;

/// Pragma user_version.
pub const DB_TARGET_VERSION: i32 = 1;

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
    pub settings: SettingsManager
}

impl ReminderData {
    /// Save ReminderData to DB and return Reminder.
    pub async fn save(self, db: Arc<Connection>) -> anyhow::Result<Reminder> {
        // Clone data.
        let room_id_str = self.settings.room_id.to_string();
        // let datetime_str = reminder.civil_dt.strftime("%Y-%m-%d %H:%M:%S").to_string();
        let datetime_str = self.civil_dt.to_string();
        let utc_str = self.utc_dt.to_string();
        let tz_str = self.settings.room_tz.iana_name().unwrap().to_string();
        let created_by_str = self.created_by.to_string();
        let text = self.text.clone();
        
        // Insert to DB.
        let result = db.call(move |c| -> Result<Reminder, tokio_rusqlite::Error>{
            c.execute(
                "INSERT INTO reminders (room_id, text, target_time, utc_time, tz, created_at, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                [&room_id_str, &text, &datetime_str, &utc_str, &tz_str, &(Timestamp::now().to_string()), &created_by_str],
            )?;
            
            let id = c.last_insert_rowid();

            Ok(Reminder {
                id,
                data: self,
                status: ReminderStatus::Pending,
            })
        }).await;

        // Map Result to anyhow::Result
        result.map_err(|e| anyhow::anyhow!(e))
    }
}

/// Structure for restoring reminders from DB.
struct RawReminder {
    id: i64,
    room_id: String,
    text: String,
    target_time: String,
    utc_time: String,
    tz: String,
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

/// Keys of i18n for erros.
#[derive(Debug, Display)]
pub enum ReminderError {
    #[strum(serialize = "error.month")] InvalidMonth,
    #[strum(serialize = "error.past-time")] TimeInPast,
    #[strum(serialize = "error.time")] InvalidTime,
    #[strum(serialize = "error.empty-text")] EmptyText,
    #[strum(serialize = "error.unsafe-datetime")] UnsafeDateTime,
    #[strum(serialize = "error.date-format")] InvalidDateFormat,
    #[strum(serialize = "error.time-format")] InvalidTimeFormat,
    #[strum(serialize = "error.datetime-format")] InvalidDateTimeFormat,
    #[strum(serialize = "error.delegation-room-format")] InvalidDelegationRoomFormat,
    #[strum(serialize = "error.delegation-no-room")] NoDelegatedRoom,
    #[strum(serialize = "error.db")] Db,
    #[strum(serialize = "tz.invalid-format")] InvalidTzFormat,
}
impl From<jiff::Error> for ReminderError {
    fn from(e: jiff::Error) -> Self {
        tracing::error!("{}", e);
        ReminderError::UnsafeDateTime
    }
}

impl From<i64> for ReminderStatus {
    fn from(value: i64) -> Self {
        ReminderStatus::try_from(value).unwrap_or(ReminderStatus::Pending)
    }
}

// ===== DB =====
/// Database initialization.
pub async fn init_db(data_dir: &PathBuf) -> anyhow::Result<Connection> {
    // Path for DB file.
    let path = data_dir.join("remindrix.db");

    // Open or create DB file.
    let conn = Connection::open(&path).await?;
    
    // Create table.
    conn.call(|c| -> Result<(), tokio_rusqlite::Error> {
        c.execute(
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
        )?;
        c.execute(
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
        )?;
        c.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminders_status ON reminders(status)",
            [],
        )?;
        // Ok::<_, tokio_rusqlite::Error>(())
        // We can use OK(()) without turbo-fish if we specify
        // -> Result<(), tokio_rusqlite::Error> in function result.
        Ok(())
    }).await?;

    // Migrations.
    run_migrations(&conn).await.map_err(|e| {
        tracing::error!("An error occurred during database migration");
        e
    })?;
    
    Ok(conn)
}

/// DB Migration.
async fn run_migrations(conn: &Connection) -> Result<()> {
    let current_version: i32 = conn
        .call(|conn| {
            conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        }).await?;

    // Check.
    if current_version < DB_TARGET_VERSION {
        tracing::info!("Running DB migrations from version {} to version {}", current_version, DB_TARGET_VERSION);
        
        conn.call(|conn| -> Result<(), tokio_rusqlite::Error> {
            let tx = conn.transaction()?;
            
            // v1.1.0
            /*
            tx.execute("ALTER TABLE reminders ADD COLUMN created_by TEXT", [])?;
            */
            
            // Set Pragma.
            let pragma_query = format!("PRAGMA user_version = {}", DB_TARGET_VERSION);
            tx.execute(&pragma_query, [])?;
            
            tx.commit()?;

            Ok(())
        }).await?;

        tracing::info!("Migrations are completed");
    }

    Ok(())
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
    // TODO: move to the function
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

/// Scheduling ReminderUtc.
pub async fn schedule_reminder_utc(
    ctx: Arc<super::BotContext>,
    reminder: ReminderUtc,
) {
    let now = Timestamp::now();

    // Get Span.
    let span = now.until(reminder.utc_time).unwrap_or(0.seconds());
    let seconds = span.total(Unit::Second).unwrap() as i64;

    if seconds <= 0 {
        tracing::error!("The reminder #{} time has already passed!", reminder.id);
        return;
    }

    let std_duration = std::time::Duration::from_secs(seconds as u64);

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
    let raw_reminders: Vec<RawReminder> = ctx.db.call(move |c| {
        let mut stmt = c.prepare("SELECT id, room_id, text, target_time, utc_time, tz FROM reminders WHERE status = 0")?;
        
        // Get rows with a RawReminder
        let mapped_rows = stmt.query_map([], |row| {
            Ok(RawReminder {
                id: row.get(0)?,
                room_id: row.get(1)?,
                text: row.get(2)?,
                target_time: row.get(3)?,
                utc_time: row.get(4)?,
                tz: row.get(5)?,
            })
        })?;

        let mut res = Vec::new();
        for row in mapped_rows {
            res.push(row?);
        }
        
        Ok::<_, tokio_rusqlite::Error>(res)
    }).await?;

    // Make a Vec for reminders.
    let reminders: Vec<ReminderUtc> = raw_reminders.into_iter().map(|raw| {
        let id: i64 = raw.id;
        let room_id_str = raw.room_id.to_string();
        let text = raw.text;
        let target_time_str = raw.target_time;
        let utc_time_str = raw.utc_time;
        let room_tz_str = raw.tz;

        // Parse strings to OwnedRoomId and CivilDateTime
        let room_id = RoomId::parse(&room_id_str)?;

        // Get TZ
        let tz = super::settings::parse_tz_or_default(&room_tz_str, &ctx.bot_config.tz);

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

        // Parse Civil from Timestamp -> Zoned
        // let target_zoned = timestamp.in_tz(&room_tz_str);
        let target_zoned = timestamp.to_zoned(tz.clone());
        let target_time = target_zoned.datetime();

        Ok(ReminderUtc {
            id,
            room_id,
            text,
            target_time,
            utc_time: timestamp,
            tz,
            status: ReminderStatus::Pending,
        })
    }).collect::<anyhow::Result<Vec<_>>>()?;

    // HashMap for missed reminders
    let mut missed_by_room: HashMap<OwnedRoomId, Vec<ReminderUtc>> = HashMap::new();

    // Distribute reminders into scheduled and missed ones
    for reminder in reminders {
        if reminder.utc_time > Timestamp::now() {
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
        let ctx_clone = ctx.clone();

        tokio::spawn(async move {
            if let Some(room) = ctx_clone.client.get_room(&room_id) {
                // Sorting by time and combining into a summary, can also be numbered.
                let mut sorted = reminders.clone();
                sorted.sort_by_key(|r| r.target_time);

                let summary = sorted
                    .iter()
                    .map(|r| {
                        let target_time_date = r.target_time.strftime("%d.%m.%Y").to_string();
                        let target_time_time = r.target_time.strftime("%H:%M").to_string();
                        let sum = t!("reminder.list", text = r.text, date = target_time_date, time = target_time_time);
                        sum
                    })
                    .collect::<String>();

                let message = t!("reminder.missed", sum = summary);

                if room.send(RoomMessageEventContent::text_plain(message)).await.is_ok() {
                    let ids: Vec<i64> = reminders.iter().map(|r| r.id).collect();
                    let _ = ctx_clone.db.call(move |c| -> Result<(), tokio_rusqlite::Error> {
                        for id in ids {
                            c.execute("UPDATE reminders SET status = ?1 WHERE id = ?2", [ReminderStatus::Sent as i64, id])?;
                            tracing::info!("Missed reminder #{} has been sent", id);
                        }
                        Ok(())
                    }).await;
                }
            } else {
                tracing::warn!("Room {} was not found for the summary. All reminders in this room have been marked as missed.", &room_id);
                let _ = ctx_clone.db.call(move |c| {
                    c.execute("UPDATE reminders SET status = ?1 WHERE room_id = ?2", params![ReminderStatus::Missed as i64, &room_id.as_str()])
                }).await;
            }
        });
    }

    Ok(())
}

// ===== DB =====
/*
/// Save reminder to DB and return ReminderUtc.
pub async fn save_reminder_to_db_utc(
    cmd_ctx: &CommandContext,
    text: String,
    civil_time: CivilDateTime,
    utc_time: Timestamp,
) -> Result<ReminderUtc, tokio_rusqlite::Error> {
    let room_id_clone = cmd_ctx.settings.room_id.clone();
    let room_tz_clone = cmd_ctx.settings.room_tz.clone();

    let room_id_str = cmd_ctx.settings.room_id.to_string();
    let datetime_str = civil_time.to_string();
    let utc_str = utc_time.to_string();
    let tz_str = cmd_ctx.settings.room_tz.iana_name().unwrap().to_string();
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
            target_time: civil_time,
            utc_time,
            tz: room_tz_clone,
            status: ReminderStatus::Pending,
        })
    }).await
}

/// Save ReminderData to DB and return Reminder.
pub async fn save_reminder_data(
    db: Arc<Connection>,
    reminder: ReminderData
) -> anyhow::Result<Reminder> {
    // let room_id_clone = reminder.settings.room_id.clone();
    // let room_tz_clone = reminder.settings.room_tz.clone();

    let room_id_str = reminder.settings.room_id.to_string();
    // let datetime_str = reminder.civil_dt.strftime("%Y-%m-%d %H:%M:%S").to_string();
    let datetime_str = reminder.civil_dt.to_string();
    let utc_str = reminder.utc_dt.to_string();
    let tz_str = reminder.settings.room_tz.iana_name().unwrap().to_string();
    let created_by_str = reminder.created_by.to_string();
    // let text = reminder.text;
    
    let result = db.call(move |c| -> Result<Reminder, tokio_rusqlite::Error>{
        c.execute(
            "INSERT INTO reminders (room_id, text, target_time, utc_time, tz, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            [&room_id_str, &reminder.text, &datetime_str, &utc_str, &tz_str, &created_by_str],
        )?;
        
        let id = c.last_insert_rowid();

        Ok(Reminder {
            id,
            data: reminder,
            status: ReminderStatus::Pending,
        })
    }).await;

    result.map_err(|e| anyhow::anyhow!(e))
}
*/

// ===== Service =====
/// Check if reminder time as string can be parsed. Used in the cli.rs.
pub fn is_time_valid(time_str: &str, _time_format: &str) -> bool {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() != 2 { return false; }
    let h = parts[0].parse::<u8>().unwrap_or(99);
    let m = parts[1].parse::<u8>().unwrap_or(99);
    h < 24 && m < 60
}
