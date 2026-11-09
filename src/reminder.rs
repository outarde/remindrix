use matrix_sdk::ruma::{
        UserId, OwnedUserId, OwnedRoomId, RoomId,
        events::room::message::{RoomMessageEventContent},
};
use tokio_rusqlite::{params, Connection};
use jiff::{
    Zoned, ToSpan, tz::TimeZone, Timestamp, Unit,
    civil::DateTime as CivilDateTime
};
use std::{
    str::FromStr,
    sync::Arc, 
    collections::HashMap
};
use rust_i18n::t;
use anyhow::Result;
use thiserror::Error;
use croner::Cron;
use tokio_cron_scheduler::{Job, JobScheduler};

use crate::messaging::{RoomMessenger, MessageReaction};

/// British classification of time ante and post meridiem/noon (am and pm).
#[derive(Debug, PartialEq, Eq)]
pub enum DayPeriod {
    Am,
    Pm,
    No,
}

/// Structure for restoring reminders from DB.
pub struct RawReminder {
    pub id: i64,
    pub room_id: String,
    pub text: String,
    pub target_time: String,
    pub utc_time: String,
    pub tz: String,
    pub created_by: String,
    pub status: i64,
    pub target_user_id: Option<String>,
    pub delegation_kind: String,
    pub schedule_type: String,
    pub schedule: Option<String>,
}

/// Statuses of Reminder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ReminderStatus {
    Pending = 0,
    Sent = 1,
    Missed = 2, // Reminders that were not sent and will not be sent
    Cancelled = 3,
    // Archived = 4,
}

impl From<i64> for ReminderStatus {
    fn from(value: i64) -> Self {
        match value {
            0 => ReminderStatus::Pending,
            1 => ReminderStatus::Sent,
            2 => ReminderStatus::Missed,
            3 => ReminderStatus::Cancelled,
            _ => ReminderStatus::Pending,
        }
    }
}

/// Keys of i18n for erros by thiserror.
#[derive(Debug, Error)]
pub enum ReminderError {
    #[error("error.db")] Db(#[from] tokio_rusqlite::Error),
    #[error("error.month")] InvalidMonth,
    #[error("error.past-time")] TimeInPast(Zoned),
    #[error("error.time")] _InvalidTime,
    #[error("error.empty-text")] EmptyText,
    #[error("error.unsafe-datetime")] UnsafeDateTime,
    #[error("error.date-format")] InvalidDateFormat,
    #[error("error.time-format")] InvalidTimeFormat,
    #[error("error.datetime")] InvalidDateTime,
    #[error("error.delegation-room-format")] InvalidDelegationRoomFormat,
    #[error("error.delegation-no-room")] NoDelegatedRoom,
    #[error("error.delegation-user-format")] InvalidUserId,
    #[error("error.delegation-options")] DelegationOptions,
    #[error("tz.not-set")] TzNotSet,
    #[error("error.sent-or-cancelled")] Outdated,
    #[error("error.invalid-schedule")] InvalidSchedule,
    #[error("error.invalid-cron-expression")] InvalidCronExpression,
    #[error("error.invalid-cron-time")] NoCronTime,

    #[error("error.unsafe-datetime")] JiffError(#[from] jiff::Error), 
    #[error("error.matrix")] MatrixError(#[from] matrix_sdk::Error),

    #[error("error.send-message")] SendError,

    #[error("list.unknown-scope")] ListUnknownScope,
    #[error("list.unknown-order")] ListUnknownOrder,
    #[error("list.error")] ListError,
    // #[error("list.error.display-buttons")] ListButtonsError,
    #[error("list.error.save-state")] ListStateError(tokio_rusqlite::Error),
    #[error("list.error.no-state")] ListNoState,
    #[error("list.empty")] ListEmpty,
    #[error("list.empty-page")] ListEmptyPage,
    #[error("list.empty-affectionately")] ListEmptyAffectionately,
    #[error("list.ended")] ListEnded,
}

impl ReminderError {
    /// In some cases, to obtain the localized error text, 
    /// we need to pass its value, not just its name as a key by thiserror.
    pub fn to_local(&self, locale: &str) -> String {
        match self {
            ReminderError::TimeInPast(val) => {
                let date = val.strftime("%d.%m.%Y").to_string();
                let time = val.strftime("%H:%M").to_string();
                let msg = t!(self.to_string(), locale = locale, date = date, time = time);
                msg.to_string()
            },
            ReminderError::Db(error) | ReminderError::ListStateError(error) => {
                tracing::error!("{:?}", error);

                let key = self.to_string(); 
                let msg = t!(key, locale = locale);
                msg.to_string()
            },
            _ => {
                let key = self.to_string(); 
                let msg = t!(key, locale = locale);
                msg.to_string()
            }
        }
    }
}

/// Reminder Structure with ReminderData.
#[derive(Debug, Clone)]
pub struct Reminder {
    pub id: i64,
    pub data: ReminderData,
    pub status: ReminderStatus,
}

/// Reminder data goes into schedule_reminder() method.
#[derive(Debug, Clone)]
pub struct ReminderData {
    pub room_id: OwnedRoomId,
    pub utc_dt: Timestamp,
    pub civil_dt: CivilDateTime,
    pub tz: TimeZone,
    pub text: String,
    pub created_by: OwnedUserId,
    pub delegation: DelegationType,
    pub schedule: ScheduleType,
}

/// Delegation data for ReminderData.
#[derive(Debug, Clone)]
pub enum DelegationType {
    Personal,
    User(OwnedUserId),
    Room
}

impl DelegationType {
    pub fn to_raw(&self) -> (&'static str, Option<String>) {
        match self {
            Self::Personal => ("personal", None),
            Self::User(user_id) => ("delegated_user", Some(user_id.to_string())),
            Self::Room => ("delegated_room", None),
        }
    }

    pub fn from_raw(delegation_kind: &str, target_user_id: Option<&str>) -> Result<Self, ReminderError> {
        match (delegation_kind, target_user_id) {
            ("personal", None) => Ok(Self::Personal),
            ("personal", Some(_)) => Err(ReminderError::DelegationOptions),
            ("delegated_user", Some(uid)) => Ok(Self::User(
                UserId::parse(uid).map_err(|_| ReminderError::InvalidUserId)?
            )),
            ("delegated_user", None) => Err(ReminderError::InvalidUserId),
            ("delegated_room", None) => Ok(Self::Room),
            ("delegated_room", Some(_)) => Err(ReminderError::DelegationOptions),
            (_other, _) => Err(ReminderError::DelegationOptions),
        }
    }
}

/// Schedule type for ReminderData.
#[derive(Debug, Clone)]
pub enum ScheduleType {
    Once,
    Recurring(Cron)
}

impl ScheduleType {
    pub fn to_raw(&self) -> (&'static str, Option<String>) {
        match self {
            Self::Once => ("once", None),
            Self::Recurring(cron) => ("recurring", Some(cron.pattern.to_string())),
        }
    }

    pub fn from_raw(kind: &str, cron: Option<&str>) -> Result<Self, ReminderError> {
        match (kind, cron) {
            ("once", None) => Ok(Self::Once),
            ("once", Some(_)) => Err(ReminderError::InvalidSchedule),
            ("recurring", Some(c)) => Ok(Self::Recurring(
                Cron::from_str(c).map_err(|_| ReminderError::InvalidCronExpression)?
            )),
            ("recurring", None) => Err(ReminderError::InvalidSchedule),
            (_other, _) => Err(ReminderError::InvalidSchedule),
        }
    }
}

// ===== Reminders =====
/// Schedule Reminder.
pub async fn schedule_reminder(
    ctx: Arc<super::BotContext>,
    reminder: Reminder,
) {
    // Check if it is recurring reminder.
    if let ScheduleType::Recurring(ref cron) = reminder.data.schedule {
        if let Err(e) = schedule_cron_reminder(ctx, reminder.clone(), cron.clone()).await {
            tracing::info!("Recurring reminder #{} was not queued due to an scheduler error: {}", reminder.id, e)
        }
        return;
    };

    let now = Timestamp::now();

    // Get Span.
    let span = now.until(reminder.data.utc_dt).unwrap_or(0.seconds());
    let seconds = span.total(Unit::Second).unwrap() as i64;

    if seconds <= 0 {
        tracing::error!("The reminder #{} time has already passed!", reminder.id);
        return;
    }

    let std_duration = std::time::Duration::from_secs(seconds as u64);

    // Tokio.
    tokio::spawn(async move {
        tracing::info!("New reminder #{} in {} sec at {}", reminder.id, std_duration.as_secs(), reminder.data.utc_dt);
        
        // Asynchronic sleep.
        tokio::time::sleep(std_duration).await;

        // Try to get the room after the sleep.
        let room = match ctx.client.get_room(&reminder.data.room_id) {
            Some(r) => r,
            None => {
                tracing::warn!("Room {} was not found for reminder #{}", &reminder.data.room_id, &reminder.id);
            
                if let Err(e) = ctx.db_ctx.reminders.update_reminder_status(reminder.id, ReminderStatus::Missed).await {
                    tracing::error!("Failed to update status for reminder: {:?}", e);
                }

                // End of the spawn.
                return;
            }
        };

        // Get Settings.
        let room_settings = ctx.settings_service.load_room(&reminder.data.room_id, Some(&room)).await;

        // Message depending on whether the reminder has been delegated.
        let msg = match reminder.data.delegation {
            DelegationType::Personal => {
                t!(
                    "reminder.new",
                    locale = &room_settings.room_lang,
                    text = reminder.data.text
                )
            },
            DelegationType::User(user_id) => {
                t!(
                    "reminder.new-to-from",
                    locale = &room_settings.room_lang,
                    to = &user_id.to_string(),
                    from = reminder.data.created_by,
                    text = reminder.data.text
                )
            },
            DelegationType::Room => {
                t!(
                    "reminder.new-from",
                    locale = &room_settings.room_lang,
                    from = reminder.data.created_by,
                    text = reminder.data.text
                )
            }
        };

        // If the message was sent successfully, update the status!
        match room.send(RoomMessageEventContent::text_markdown(msg)).await {
            Ok(_) => {
                if let Err(e) = ctx.db_ctx.reminders.update_reminder_status(reminder.id, ReminderStatus::Sent).await {
                    tracing::error!("Failed to update status for reminder: {:?}", e);
                }
            }
            Err(e) => tracing::info!("Reminder #{} was not sent due to an error: {}", reminder.id, e)
        }
    });
}

// ===== Recurring =====
/// Schedule Recurring Reminder.
pub async fn schedule_cron_reminder(
    ctx: Arc<super::BotContext>,
    reminder: Reminder,
    cron: Cron,
) -> anyhow::Result<()> {        
    let scheduler = JobScheduler::new().await?;
    let scheduler_job = scheduler.clone();
    
    scheduler.add(
        Job::new_async(&cron.pattern.to_string(), move |_uuid, mut _l| {
            let mut scheduler_clone = scheduler_job.clone();
            let cron_clone = cron.clone();
            let ctx_clone = ctx.clone();

            Box::pin(async move {
                // tracing::info!("Start sending cron reminder...");
                let fresh_reminder = match get_scheduled_reminder(ctx_clone.clone(), reminder.id.clone()).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("The scheduled reminder #{} will be removed from the sending queue due to an event or an error: {:?}", reminder.id, e);
                        match scheduler_clone.shutdown().await {
                            Ok(_) => tracing::info!("The scheduler has been stopped."),
                            Err(e) => tracing::error!("Scheduler stop error: {:?}", e),
                        }

                        return;
                    }
                };

                send_recurring_reminder(ctx_clone.clone(), fresh_reminder, cron_clone.clone()).await;
            })
        })?
    ).await?;
    
    let scheduler_clone = scheduler.clone();
    let _handle = tokio::spawn(async move { 
        if let Err(e) = scheduler_clone.start().await {
            tracing::error!("The scheduler failed to launch for the reminder #{} due to an error: {:?}", reminder.id, e);
        }
    });

    Ok(())
}

/// Get actual reminder or return an error if it was removed or cancelled.
async fn get_scheduled_reminder(ctx: Arc<super::BotContext>, reminder_id: i64) -> anyhow::Result<Reminder> {
    let raw_reminder = ctx.db_ctx.reminders.get_by_id(reminder_id).await?;
    let reminders = super::list::from_raw_reminders(ctx.clone(), vec![raw_reminder])?;
    let reminder = reminders[0].clone();

    // Check if the reminder was cancelled.
    match reminder.status {
        ReminderStatus::Cancelled | ReminderStatus::Sent => Err(ReminderError::Outdated.into()),
        _ => Ok(reminder)
    }
}

/// Manage sending of recurring reminder.
async fn send_recurring_reminder(ctx: Arc<super::BotContext>, reminder: Reminder, cron: Cron) {
    // Try to get the room.
    let room = match ctx.client.get_room(&reminder.data.room_id) {
        Some(r) => r,
        None => {
            tracing::warn!("Room {} was not found for reminder #{}", &reminder.data.room_id, &reminder.id);
        
            if let Err(e) = ctx.db_ctx.reminders.update_reminder_status(reminder.id, ReminderStatus::Missed).await {
                tracing::error!("Failed to update status for reminder: {:?}", e);
            }

            // End of the spawn.
            return;
        }
    };

    // Get Settings.
    let room_settings = ctx.settings_service.load_room(&reminder.data.room_id, Some(&room)).await;

    // Message depending on whether the reminder has been delegated.
    let msg = match reminder.data.delegation {
        DelegationType::Personal => {
            t!(
                "reminder.recurring.new",
                locale = &room_settings.room_lang,
                text = reminder.data.text
            )
        },
        DelegationType::User(user_id) => {
            t!(
                "reminder.recurring.new-to-from",
                locale = &room_settings.room_lang,
                to = &user_id.to_string(),
                from = reminder.data.created_by,
                text = reminder.data.text
            )
        },
        DelegationType::Room => {
            t!(
                "reminder.recurring.new-from",
                locale = &room_settings.room_lang,
                from = reminder.data.created_by,
                text = reminder.data.text
            )
        }
    };

    let msng = RoomMessenger::new(room.clone());

    // If the message was sent successfully, update the target time!
    let event_id = match msng.text_md_with_event_id(&msg).await {
        Ok(id) => {
            // Update reminder's target time
            if let Err(e) = update_recurring_time(ctx.clone(), reminder.id.clone(), &cron, room_settings.room_tz.clone()).await {
                tracing::error!(
                    "Failed to get or update target time for recurring reminder #{}: {:?}. The reminder will repeat until the bot is restarted",
                    reminder.id,
                    e
                );
            }
            /*
            let Ok((utc_dt, civil_dt)) = super::natural::build_cron_datetime(&cron, room_settings.room_tz.clone()).await else { return };
            
            if let Err(e) = ctx.db_ctx.reminders.update_reminder_time(reminder.id, utc_dt.clone(), civil_dt).await {
                tracing::error!("Failed to update target time for recurring reminder: {:?}", e);
            }
            */
            
            tracing::info!("Recurring reminder #{} was sent on the CRON schedule: {:?}", reminder.id, cron.pattern.as_str());

            id
        }
        Err(e) => {
            tracing::info!("Reminder #{} was not sent due to an error: {}", reminder.id, e);
            if let Err(e) = ctx.db_ctx.reminders.update_reminder_status(reminder.id, ReminderStatus::Missed).await {
                tracing::error!("Failed to update status for reminder: {:?}", e);
            }

            return;
        }
    };

    // Send reactions that will serve as buttons
    // msng.react(event_id, MessageReaction::Cancel);
    msng.react(event_id, MessageReaction::Done).await;
}

/// Try to set next target time based on the cron pattern.
async fn update_recurring_time(ctx: Arc<super::BotContext>, id: i64, cron: &Cron, tz: TimeZone) -> anyhow::Result<()> {
    /*let Ok((utc_dt, civil_dt)) = super::natural::build_cron_datetime(cron, tz.clone()).await else {
        return Err(anyhow::anyhow!("Failed to get the next target time for recurring reminder: {:?}", id));
    };*/
    
    let (utc_dt, civil_dt) = super::natural::build_cron_datetime(cron, tz.clone()).await.map_err(|_|
        anyhow::anyhow!("Failed to get the next target time for recurring reminder: {:?}", id)
    )?;
    ctx.db_ctx.reminders.update_reminder_time(id, utc_dt, civil_dt).await?;

    Ok(())

    /*
    let Ok((utc_dt, civil_dt)) = super::natural::build_cron_datetime(cron, tz.clone()).await else {
        tracing::error!("Failed to get the next target time for recurring reminder: {:?}", id);
        return;
    };
            
    if let Err(e) = ctx.db_ctx.reminders.update_reminder_time(id, utc_dt, civil_dt).await {
        tracing::error!("Failed to update target time for recurring reminder: {:?}", e);
    }
    */
}

/*
async fn prepare_for_sending(ctx: Arc<super::BotContext>, reminder_id: i64, room_id: &RoomId) -> anyhow::Result<(Room, RoomSettings)> {
    // Check if the reminder was cancelled.
    let reminder_status = ctx.db_ctx.reminders.get_reminder_status(reminder_id.clone()).await;
    match reminder_status {
        Ok(s) => {
            match s {
                ReminderStatus::Cancelled || ReminderStatus::Sent => return anyhow::anyhow!("Err"),
                _ => ()
            }
        },
        Err(e) => {
            tracing::error!("Failed to check reminder #{} status: {:?}", reminder.id, e);
            return Err(e.into());
        }
    }

    // Try to get the room.
    let room = match ctx.client.get_room(&reminder.data.room_id) {
        Some(r) => r,
        None => {
            tracing::warn!("Room {} was not found for reminder #{}", &reminder.data.room_id, &reminder.id);
        
            if let Err(e) = ctx.db_ctx.reminders.update_reminder_status(reminder.id, ReminderStatus::Missed).await {
                tracing::error!("Failed to update status for reminder: {:?}", e);
            }

            return anyhow::anyhow!("Err");
        }
    };

    // Get Settings.
    let room_settings = ctx.settings_service.load_room(&reminder.data.room_id, Some(&room)).await;

    Ok((room, room_settings))
}
*/

// ===== Restore =====
/// Restores all future (or missed) reminders from the database.
pub async fn restore_reminders(ctx: Arc<super::BotContext>) -> anyhow::Result<()> {
    // Get reminders
    let raw_reminders = ctx.db_ctx.reminders.get_by_status(ReminderStatus::Pending).await?;

    // Make a Vec for reminders.
    let reminders = super::list::from_raw_reminders(ctx.clone(), raw_reminders)?;

    // HashMap for missed reminders
    let mut missed_by_room: HashMap<OwnedRoomId, Vec<Reminder>> = HashMap::new();
    let mut missed_by_room_recurring: HashMap<OwnedRoomId, Vec<Reminder>> = HashMap::new();
    let len = reminders.len();

    // Distribute reminders into scheduled and missed ones.
    for reminder in reminders {
        // In the future, not missed.
        if reminder.data.utc_dt > Timestamp::now() {
            schedule_reminder(
                ctx.clone(), 
                reminder.clone()
            ).await;
        // In the past, missed.
        } else {
            match reminder.data.schedule {
                ScheduleType::Once => {
                    missed_by_room
                        .entry(reminder.data.room_id.clone())
                        .or_default()
                        .push(reminder);
                }
                ScheduleType::Recurring(ref cron) => {
                    missed_by_room_recurring
                        .entry(reminder.data.room_id.clone())
                        .or_default()
                        .push(reminder);
                }
            }
        }
    }

    let missed_len = missed_by_room.len();
    let missed_recurring_len = missed_by_room_recurring.len();

    tracing::info!("Restored reminders: {}", len - missed_len - missed_recurring_len);

    // If we have missed reminders in HashMap
    if missed_len == 0 && missed_recurring_len == 0 {
        tracing::info!("No missed reminders");
        return Ok(());
    }

    tracing::info!(
        "Sending a summary for {} missed reminders and for {} missed recurring reminders that will be sent next time",
        missed_len, 
        missed_recurring_len
    );
    summary_missed(ctx.clone(), missed_by_room).await?;
    summary_missed_recurring(ctx.clone(), missed_by_room_recurring).await?;

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
                    if let Err(e) = ctx_clone.db_ctx.reminders.update_reminder_status_by_room(&room_id, ReminderStatus::Missed).await {
                        tracing::error!("Failed to update status to missed for room {}: {:?}", room_id, e);
                    }

                    // End of the spawn.
                    return;
                }
            };

            let room_settings = ctx_clone.settings_service.load_room(&room_id, None).await;

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
                        locale = room_settings.room_lang.as_ref(), 
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
                locale = room_settings.room_lang.as_ref(), 
                sum = summary
            );

            // Update status for the sent reminders.
            if room.send(RoomMessageEventContent::text_markdown(message)).await.is_ok() {
                let ids: Vec<i64> = reminders.iter().map(|r| r.id).collect();
                tracing::info!("Missed reminders #{:?} has been sent", ids);
                if let Err(e) = ctx_clone.db_ctx.reminders.update_reminders_status(ids, ReminderStatus::Missed).await {
                    tracing::error!("Failed to update status to sent for ids {}: {:?}", room_id, e);
                }
            }
        });
    }

    Ok(())
}

/// Sending a summary of missed recurring reminders for each room and queue up them again.
async fn summary_missed_recurring(
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
                    if let Err(e) = ctx_clone.db_ctx.reminders.update_reminder_status_by_room(&room_id, ReminderStatus::Missed).await {
                        tracing::error!("Failed to update status to missed for room {}: {:?}", room_id, e);
                    }

                    // End of the spawn.
                    return;
                }
            };

            let room_settings = ctx_clone.settings_service.load_room(&room_id, None).await;

            // Sorting by time and combining into a summary, can also be numbered.
            let mut sorted = reminders.clone();
            sorted.sort_by_key(|r| r.data.civil_dt);

            let summary = sorted
                .iter()
                .map(|r| {
                    let date = r.data.civil_dt.strftime("%d.%m.%Y").to_string();
                    let time = r.data.civil_dt.strftime("%H:%M").to_string();
                    let cron_pattern = match r.data.schedule {
                        ScheduleType::Recurring(ref cron) => cron.pattern.to_string(),
                        ScheduleType::Once => String::new()
                    };

                    let sum = t!(
                        "reminder.list-recurring", 
                        locale = room_settings.room_lang.as_ref(), 
                        text = r.data.text, 
                        date = date, 
                        time = time,
                        cron = cron_pattern
                    );
                    sum
                })
                .collect::<String>();

            // Prepare a final summary message.
            let message = t!(
                "reminder.missed-recurring", 
                locale = room_settings.room_lang.as_ref(), 
                sum = summary
            );

            // Send the summary.
            if room.send(RoomMessageEventContent::text_markdown(message)).await.is_ok() {
                let ids: Vec<i64> = reminders.iter().map(|r| r.id).collect();
                tracing::info!("Missed recurring reminders #{:?} has been sent in a summary", ids);
            }

            // Set new target times and schedule recurring reminders again.
            let mut ids: Vec<i64> = Vec::new();
            for reminder in &reminders {
                // Update reminder's target time to prevent new summary after bot reloading
                let ScheduleType::Recurring(ref cron) = reminder.data.schedule else { continue };
                
                if let Err(e) = update_recurring_time(ctx_clone.clone(), reminder.id.clone(), &cron, room_settings.room_tz.clone()).await {
                    tracing::error!("Failed to get or update target time for recurring reminder #{}: {:?}. It will be marked as missed and cancelled.", reminder.id, e);
                    
                    // Discard (mark as missed) to prevent its appearance each bot reloading
                    if let Err(e) = ctx_clone.db_ctx.reminders.update_reminder_status(reminder.id.clone(), ReminderStatus::Missed).await {
                        tracing::error!("Failed to mark recurring reminder as missed: {:?}", e);
                    }

                    continue;
                }
                
                /*
                let Ok((utc_dt, civil_dt)) = super::natural::build_cron_datetime(&cron, room_settings.room_tz.clone()).await else { continue };
                if let Err(e) = ctx_clone.db_ctx.reminders.update_reminder_time(reminder.id.clone(), utc_dt, civil_dt).await {
                    tracing::error!("Failed to update target time for recurring reminder: {:?}", e);
                }
                */

                // Schedule it.
                schedule_reminder(
                    ctx_clone.clone(), 
                    reminder.clone(),
                ).await;

                ids.push(reminder.id);
            }
            tracing::info!("Missed recurring reminders #{:?} has been queued for the next target time", ids);
            // ==
        });
    }

    Ok(())
}

// ===== Service =====
/// Check if reminder time as string can be parsed. Used in the cli.rs.
pub fn is_time_valid(time_str: &str) -> bool {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() != 2 { return false; }
    let h = parts[0].parse::<u8>().unwrap_or(99);
    let m = parts[1].parse::<u8>().unwrap_or(99);
    h < 24 && m < 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_time_valid() {
        assert_eq!(is_time_valid("24:00"), false);
    }
}
