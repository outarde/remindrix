use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Client, Room, RoomState,
    ruma::{
        room_id,
        OwnedUserId, RoomId, OwnedRoomId, OwnedEventId,
        events::{
            reaction::ReactionEventContent, relation::Annotation,
            room::{
                member::StrippedRoomMemberEvent, 
                message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
            }
        }
    }
};
use anyhow::Result;
use tokio::time::{Duration, sleep};
use chrono::{
    Days, Months,
    NaiveDateTime, NaiveDate, NaiveTime,
    DateTime, Utc, TimeDelta, TimeZone, 
    Datelike, Timelike, LocalResult
};
use chrono_tz::Tz;
use tokio_rusqlite::Connection;
use regex::Regex;
use std::{string::ToString, sync::{OnceLock, Arc}};
use rust_i18n::t;
use strum_macros::{Display, EnumString};
use clap::Parser;

// app crates
use crate::config::BotConfig;
use crate::reminder::{ReminderStatus, ReminderError};
use crate::settings::{RoomTimezoneContent, SettingsManager};
use crate::handlers::{
    CommandContext, I18nManager
};
use crate::parsers::{
    naive_to_datetime
};
use crate::reactions::{
    MessageReaction, 
    send_welcome_message, 
    calculate_durations, send_reaction, send_digits_reaction
};

// Compile regex only once
static REMINDER_REGEX: OnceLock<Regex> = OnceLock::new();

/// Parsed data of user message for new reminder.
#[derive(Debug)]
struct ParsedReminder {
    text: String,
    year: String,
    month: String,
    day: String,
    hour: String,
    min: String,
    is_auto: bool
}

/// Day options in natural language.
enum NaturalDay {
    Today,
    Tomorrow,
}

impl NaturalDay {
    fn from_str(text: &str, i18n_today: &str, i18n_tomorrow: &str) -> Option<Self> {
        if text == i18n_today {
            Some(NaturalDay::Today)
        } else if text == i18n_tomorrow {
            Some(NaturalDay::Tomorrow)
        } else {
            None
        }
    }
}

/// Time options in natural language.
enum NaturalTime {
    Morning,
    Afternoon,
    Evening,
}

impl NaturalTime {
    fn from_str(text: &str, i18n: &I18nManager) -> Option<Self> {
        if text == i18n.morning {
            Some(NaturalTime::Morning)
        } else if text == i18n.afternoon {
            Some(NaturalTime::Afternoon)
        } else if text == i18n.evening {
            Some(NaturalTime::Evening)
        } else {
            None
        }
    }
}

// ===== Natural Pipeline =====
/// Create reminder with regular expression recognition.
/// Original pipeline with base capability.
pub async fn process_natural_reminder(
    args_str: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) {
    // Make regular expression
    let re = build_reminder_regex(&cmd_ctx.ctx, &cmd_ctx.i18n);

    // If regular expression found some groups
    if let Some(caps) = re.captures(args_str) {

        // Parsed Data
        let reminder_data = match parse_reminder_data(
            &caps, 
            &cmd_ctx
        ) {
            Some(data) => data,
            None => {
                tracing::error!("Error parsing regex: {:?}", caps);
                return;
            }
        };

        // Times
        let (utc_dt, naive_dt) = match build_datetime_utc(&reminder_data, &cmd_ctx) {
            Ok((ut, nt)) => (ut, nt),
            Err(err) => {
                let err_msg = t!(err.to_string()); 
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(err_msg)).await;
                
                tracing::error!("Date and time validation error: {:?} for {:?}", err, reminder_data);
                return;
            }
        };

        // Save to DB.
        match super::reminder::save_reminder_to_db_utc(
            &cmd_ctx, 
            reminder_data.text, 
            naive_dt.clone(), 
            utc_dt.clone()
        ).await {
            Ok(new_reminder) => {
                // Schedule it.
                super::reminder::schedule_reminder_utc(cmd_ctx.ctx.clone(), new_reminder).await;

                // Send success reaction or message to the room.
                if cmd_ctx.bot_config().send_reactions {
                    // Send digits reaction or one emoji.
                    if cmd_ctx.bot_config().send_digits_reactions {
                        let digits = calculate_durations(&utc_dt);
                        let _ = send_digits_reaction(event.event_id.clone(), &cmd_ctx, digits).await;
                    }
                    else {
                        let _ = send_reaction(event.event_id.clone(), &cmd_ctx, MessageReaction::Timer).await;
                    }
                } else {
                    let date_str = naive_dt.format("%d.%m.%Y");
                    let reminder_mes = t!("reminder.saved", date = date_str, hour = reminder_data.hour, min = reminder_data.min);
                    let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(reminder_mes)).await;
                }
            }
            Err(err) => {
                tracing::error!("SQLite error: {:?}", err);
            }
        }
    } 
    // Welcome message.
    else {
        let _ = send_reaction(event.event_id.clone(), &cmd_ctx, MessageReaction::Cross).await;
        send_welcome_message(cmd_ctx).await;
    }
}

// ===== Parsers and Constructions Methods for Reminders =====
/// Build regular expression
fn build_reminder_regex(
    ctx: &Arc<super::BotContext>,
    i18n: &Arc<I18nManager>,
) -> &'static Regex {
    REMINDER_REGEX.get_or_init(|| {
        let mut regex_str = String::with_capacity(256); 
        
        // [^\.\-\s]{1,15} in ?P<month> can be replaced with white list of months names
        regex_str.push_str(r"^(?i)(?:(?P<datetime>(?P<day>\d{1,2})(?:\s|\.|\/|-)(?P<month>[^\.\-\s]{1,15}|\d{2})(?:\s|\.|\/|-)?(?P<year>\d{4})?)|(?P<day_natural>");
        regex_str.push_str(&i18n.days.join("|"));
        regex_str.push_str(r")\s+)");

        if ctx.bot_config.remind_undated {
            regex_str.push_str(r"?");
        }

        regex_str.push_str(r"(?:(?:(?<prep>at|");
        regex_str.push_str(&i18n.prepositions.join("|"));
        regex_str.push_str(r")\s+)?((?P<hour>\d{2}):(?P<min>\d{2})|(?P<time_natural>");
        regex_str.push_str(&i18n.times.join("|"));
        regex_str.push_str(r")))?+\s?(?P<text>.+)$");
        // regex_str.push_str(r")|(?P<time_interval>(?<gap>\d{1,2})\s(?<step>minutes|hours)) ))?\s+(?P<text>.+)$");

        Regex::new(&regex_str).unwrap()
    })
}

/// Parse regex captions to ParsedReminder
fn parse_reminder_data(
    caps: &regex::Captures,
    cmd_ctx: &CommandContext,
    //bot_config: &BotConfig,
    //room_tz: &Tz,
    //i18n: &Arc<I18nManager>,
) -> Option<ParsedReminder> {
    // Get current date for user's timezone
    let now_in_tz = Utc::now().with_timezone(&cmd_ctx.settings.room_tz);
    let today_date = now_in_tz.date_naive();

    let mut is_auto = false;
    
    // Day and month
    let (day, month) = if let (Some(d), Some(m)) = (caps.name("day"), caps.name("month")) {
        (d.as_str().to_string(), m.as_str().to_lowercase())
    } else if let Some(d_nat) = caps.name("day_natural") {
        let natural_day = NaturalDay::from_str(d_nat.as_str(), &cmd_ctx.i18n.today, &cmd_ctx.i18n.tomorrow)?;
        match natural_day {
            NaturalDay::Today => (today_date.format("%d").to_string(), today_date.format("%m").to_string()),
            NaturalDay::Tomorrow => {
                let tomorrow = today_date + Days::new(1);
                (tomorrow.format("%d").to_string(), tomorrow.format("%m").to_string())
            }
        }
    } else {
        is_auto = true;
        (today_date.format("%d").to_string(), today_date.format("%m").to_string())
    };

    // Year
    let year = caps.name("year")
        .map(|y| y.as_str().to_string())
        .unwrap_or_else(|| today_date.format("%Y").to_string());

    // Time
    let (hour, min) = if let (Some(h), Some(m)) = (caps.name("hour"), caps.name("min")) {
        (h.as_str().to_string(), m.as_str().to_string())
    } else if let Some(t_nat) = caps.name("time_natural") {
        let natural_time = NaturalTime::from_str(&t_nat.as_str().to_lowercase(), &cmd_ctx.i18n)?;
        let (h, m) = match natural_time {
            NaturalTime::Morning => &cmd_ctx.bot_config().morning.split_once(":")?,
            NaturalTime::Afternoon => &cmd_ctx.bot_config().afternoon.split_once(":")?,
            NaturalTime::Evening => &cmd_ctx.bot_config().evening.split_once(":")?,
        };
        (h.to_string(), m.to_string())
    } else {
        // TODO: Return +1 hour if day, month are today
        // let f_t = Local::now().checked_add_signed(TimeDelta::hours(1)).unwrap();
        // (f_t.format("%H").to_string(), f_t.format("%M").to_string())
        super::config::DEFAULT_MORNING_TIME
            .split_once(":")
            .map(|(h, m)| (h.to_string(), m.to_string()))
            .unwrap()
    };

    // Reminder's text
    let text = caps.name("text")?.as_str().to_string();

    Some(ParsedReminder { text, year, month, day, hour, min, is_auto })
}

/// Build final UTC DateTime for DB and validate its time in the future.
fn build_datetime_utc(
    data: &ParsedReminder, 
    cmd_ctx: &CommandContext
) -> Result<(DateTime<Utc>, NaiveDateTime), ReminderError> {
    // Parse to get month number.
    let month = if let Some(m) = cmd_ctx.i18n.parse_month(data.month.as_str()) {
        m.to_string()
    } else {
        return Err(ReminderError::InvalidMonth);
    };

    let datetime_string = format!("{}-{}-{} {}:{}:00", data.year, month.as_str(), data.day, data.hour, data.min);
    
    // Check if time can be parsed.
    let naive_dt = NaiveDateTime::parse_from_str(&datetime_string, "%Y-%m-%d %H:%M:%S")
        .map_err(|_| ReminderError::InvalidTime)?;

    // We convert it to DateTime and check that zone mapping has a single result.
    let user_dt = naive_to_datetime(naive_dt.clone(), &cmd_ctx.settings.room_tz);
    let utc_dt = user_dt.with_timezone(&Utc);

    // Checking that the time is in the future.
    let (utc_dt, naive_dt) = if utc_dt <= Utc::now() {
        if data.is_auto {
            let dt = user_dt.checked_add_days(Days::new(1)).ok_or(ReminderError::UnsafeDateTime)?;
            (dt.with_timezone(&Utc), dt.naive_local())
        }
        else { return Err(ReminderError::TimeInPast); }
    } else { (utc_dt, naive_dt) };

    Ok((utc_dt, naive_dt))
}
