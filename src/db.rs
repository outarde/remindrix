use matrix_sdk::{
    ruma::{
        RoomId, UserId,
    }
};
use std::{
    path::PathBuf,
    sync::Arc, 
};
use tokio_rusqlite::Connection;
use anyhow::{Result, Context};

use crate::reminder::{Reminder, ReminderData, ReminderStatus, ReminderError};
use crate::settings::{SettingError, RawSetting};
use jiff::{Timestamp, Unit};

struct Migration {
    version: i32,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/000_init.sql"),
    },
];

/// Database initialization.
pub async fn init_db(data_dir: &PathBuf) -> Result<Connection> {
    let path = data_dir.join("remindrix.db");
    let conn = Connection::open(&path).await
        .with_context(|| format!("Failed to open database at {:?}", path))?;

    run_migrations(&conn).await?;

    Ok(conn)
}

/// DB Migration.
async fn run_migrations(conn: &Connection) -> Result<()> {
    // Get current version.
    let current_version: i32 = conn.call(|c| {
        c.query_row("PRAGMA user_version", [], |row| row.get(0))
    }).await.unwrap_or(0);

    if current_version as usize >= MIGRATIONS.len() {
        tracing::info!("Database is up to date (version {})", current_version);
        return Ok(());
    }

    tracing::info!("Starting migrations: current version {}, target version {}", 
        current_version, MIGRATIONS.len());

    // Queries.
    conn.call(move |c| -> Result<(), tokio_rusqlite::Error> {
        let tx = c.transaction()?;
        
        for migration in MIGRATIONS.iter().filter(|m| m.version > current_version) {
            tracing::info!("Applying migration version {}", migration.version);
            
            tx.execute_batch(migration.sql)?;

            // Update PRAGMA user_version.
            let pragma_query = format!("PRAGMA user_version = {}", migration.version);
            tx.execute(&pragma_query, [])?;
        }

        tx.commit()?;
        Ok(())

        // Ok::<_, tokio_rusqlite::Error>(())
        // We can use OK(()) without turbo-fish if we specify
        // -> Result<(), tokio_rusqlite::Error> in function result.
    }).await.map_err(|e| anyhow::anyhow!("Migration failed: {}", e))?;

    tracing::info!("Database migration completed successfully");
    Ok(())
}

// ===== Repositories ======
#[derive(Clone, Debug)]
pub struct DbContext {
    pub reminders: ReminderRepository,
    pub settings: SettingRepository,
}

/// Reminder Repository.
#[derive(Clone, Debug)]
pub struct ReminderRepository {
    conn: Arc<Connection>,
}

impl ReminderRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }
    /// Save ReminderData to DB and return Reminder.
    pub async fn save_reminder(&self, data: ReminderData) -> Result<Reminder, ReminderError> {
        let conn = self.conn.clone();
        
        let room_id = data.room_id.to_string();
        let text = data.text.clone();
        let target_time = data.civil_dt.to_string();
        let utc_time = data.utc_dt.to_string();
        let tz = data.settings.room_tz.iana_name().unwrap_or("UTC").to_string();
        let created_by = data.created_by.to_string();
        let created_at = Timestamp::now().round(Unit::Second)?.to_string();

        let result = conn.call(move |c| {
            c.execute(
                "INSERT INTO reminders (room_id, text, target_time, utc_time, tz, created_at, created_by) 
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                [
                    &room_id, 
                    &text, 
                    &target_time, 
                    &utc_time, 
                    &tz, 
                    &created_at, 
                    &created_by
                ],
            )?;
            
            let id = c.last_insert_rowid();

            Ok(Reminder {
                id,
                data,
                status: ReminderStatus::Pending,
            })
        }).await?;

        Ok(result)
    }

    pub async fn get_by_id(&self, _id: i64) -> Result<Reminder> {
        todo!()
    }

    pub async fn update_status(&self, _id: i64, _status: i32) -> Result<()> {
        todo!()
    }
}

/// Settings Repository.
#[derive(Clone, Debug)]
pub struct SettingRepository {
    conn: Arc<Connection>,
}

impl SettingRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }
    /// Get all setings for the room, merged with room (bot) > user settings.
    pub async fn get_settings(&self, room_id: &RoomId, bot_id: &UserId, user_id: Option<&UserId>) -> Result<Vec<RawSetting>, SettingError> {
        let conn = self.conn.clone();
        let room_id = room_id.to_string();
        let bot_id = bot_id.to_string();

        // The query based on the presence of the user ID.
        let (statement, params) = match user_id {
            Some(u) => {
                let user_id = u.to_string();
                // Create a virtual column `is_bot`, which equals 1 
                // if the record belongs to a bot and 0 if it belongs to a user.
                // When grouping, selects the record with is_bot = 1 (the bot configuration).
                let st = "
                    SELECT key, value, user_id FROM (
                        SELECT key, value, user_id,
                                CASE WHEN user_id = ?1 THEN 1 ELSE 0 END as is_bot
                        FROM settings
                        WHERE room_id = ?2 AND (user_id = ?3 OR user_id = ?1)
                    )
                    GROUP BY key
                    HAVING is_bot = MAX(is_bot)
                    ORDER BY key
                ";
                (st, vec![bot_id, room_id, user_id])
            },
            None => {
                let st = "
                    SELECT key, value, user_id FROM (
                        SELECT key, value, user_id,
                                CASE WHEN user_id = ?1 THEN 1 ELSE 0 END as is_bot
                        FROM settings
                        WHERE room_id = ?2
                    )
                    GROUP BY key 
                    HAVING is_bot = MAX(is_bot)
                    ORDER BY key";
                (st, vec![bot_id, room_id])
            }
        };
        
        let settings = conn.call(move |conn| {
            let mut stmt = conn.prepare(statement)?;
            let mut rows = stmt.query(tokio_rusqlite::params_from_iter(params.iter()))?;

            let mut result: Vec<RawSetting> = Vec::new();
            
            while let Some(row) = rows.next()? {
                let key: String = row.get(0)?;
                let value: String = row.get(1)?;
                let user_id: String = row.get(2)?;
                
                result.push(RawSetting {
                    key,
                    value,
                    user_id
                });
            }
            
            Ok(result)
        }).await?;

        println!("{:?}", settings);

        Ok(settings)
    }
    /// Save one setting.
    pub async fn _set_setting(&self, room_id: &RoomId, user_id: &UserId, updated_by: &UserId, key: &str, value: &str,) -> Result<(), SettingError> {
        let conn = self.conn.clone();

        let room_id = room_id.to_string();
        let user_id = user_id.to_string();
        let updated_by = updated_by.to_string();
        let key = key.to_string();
        let value = value.to_string();

        let _ = conn.call(move |c| {
            c.execute(
                "INSERT INTO settings (room_id, user_id, key, value, updated_by, updated_at) 
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(room_id, user_id, key) 
                DO UPDATE SET value=excluded.value, updated_by=excluded.updated_by, updated_at=excluded.updated_at;",
                [&room_id, &user_id, &key, &value, &updated_by, &Timestamp::now().to_string()]
            )?;

            Ok(())
        }).await?;

        Ok(())
    }

    /// Save Vecs of RawSetting.
    pub async fn update_settings(
        &self,
        settings: Vec<RawSetting>,
        room_id: &RoomId,
        updated_by: &UserId,
    ) -> Result<(), SettingError> {
        let conn = self.conn.clone();

        let room_id_s = room_id.to_string();
        let updated_by_s = updated_by.to_string();

        conn.call(move |conn| {
            let tx = conn.transaction()?;

            for setting in settings {
                tx.execute(
                    "INSERT INTO settings (room_id, user_id, key, value, updated_by, updated_at) 
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(room_id, user_id, key) 
                     DO UPDATE SET value=excluded.value, updated_by=excluded.updated_by, updated_at=excluded.updated_at;",
                    [
                        &room_id_s,
                        &setting.user_id,
                        &setting.key,
                        &setting.value,
                        &updated_by_s,
                        &Timestamp::now().to_string(),
                    ],
                )?;
            }

            tx.commit()?;
            Ok(())
        })
        .await?;

        Ok(())
    }
}
