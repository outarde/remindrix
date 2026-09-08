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
use jiff::{
    Zoned, Span, ToSpan, tz::TimeZone, 
    civil::{DateTime as CivilDateTime, Date}
};
use tokio_rusqlite::Connection;
use regex::Regex;
use std::{string::ToString, sync::{OnceLock, Arc}};
use rust_i18n::t;
use strum_macros::{Display, EnumString};

// app crates
use crate::config::BotConfig;
use crate::reminder::{ReminderStatus, ReminderError};
use crate::settings::{RoomTimezoneContent, SettingsManager};
use crate::handlers::{
    CommandContext, I18nManager
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
            Ok(data) => data,
            Err(e) => {
                tracing::error!("Error: {} while parsing regex: {:?}", e, caps);
                return;
            }
        };

        // Times
        let (utc_dt, civil_dt) = match build_datetime_utc(&reminder_data, &cmd_ctx) {
            Ok((ut, ct)) => (ut, ct),
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
            civil_dt.clone(), 
            utc_dt.clone()
        ).await {
            Ok(new_reminder) => {
                // Schedule it.
                super::reminder::schedule_reminder_utc(cmd_ctx.ctx.clone(), new_reminder).await;

                // Send success reaction or message to the room.
                if cmd_ctx.bot_config().send_reactions {
                    // Send digits reaction or one emoji.
                    if cmd_ctx.bot_config().send_digits_reactions {
                        return;
                        // let digits = calculate_durations(&utc_dt);
                        // let _ = send_digits_reaction(event.event_id.clone(), &cmd_ctx, digits).await;
                    }
                    else {
                        let _ = send_reaction(event.event_id.clone(), &cmd_ctx, MessageReaction::Timer).await;
                    }
                } else {
                    let date_str = civil_dt.strftime("%d.%m.%Y").to_string();
                    let reminder_mes = t!("reminder.saved", date = &date_str, hour = reminder_data.hour, min = reminder_data.min);
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
        // if we do not need shortenings.
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
) -> Result<ParsedReminder, ReminderError> {
    // Get current date for user's timezone
    let now = Zoned::now().with_time_zone(cmd_ctx.settings.room_tz.clone());
    // If date is set to default
    let mut is_auto = false;
    
    // Day and month
    let (day, month) = if let (Some(d), Some(m)) = (caps.name("day"), caps.name("month")) {
        (d.as_str().to_string(), m.as_str().to_lowercase())
    } else if let Some(d_nat) = caps.name("day_natural") {
        let natural_day = NaturalDay::from_str(d_nat.as_str(), &cmd_ctx.i18n.today, &cmd_ctx.i18n.tomorrow);
        match natural_day {
            Some(NaturalDay::Today) => (now.day().to_string(), now.month().to_string()),
            Some(NaturalDay::Tomorrow) => {
                let tomorrow = now.checked_add(1.days())?.date();
                (tomorrow.day().to_string(), tomorrow.month().to_string())
            },
            None => return Err(ReminderError::InvalidDateFormat)
        }
    } else {
        is_auto = true;
        (now.day().to_string(), now.month().to_string())
    };

    // Year
    let year = caps.name("year")
        .map(|y| y.as_str().to_string())
        .unwrap_or_else(|| now.year().to_string());

    // Time
    let (hour, min) = if let (Some(h), Some(m)) = (caps.name("hour"), caps.name("min")) {
        (h.as_str().to_string(), m.as_str().to_string())
    } else if let Some(t_nat) = caps.name("time_natural") {
        let natural_time = NaturalTime::from_str(&t_nat.as_str().to_lowercase(), &cmd_ctx.i18n);
        let (h, m) = match natural_time {
            Some(NaturalTime::Morning) => &cmd_ctx.bot_config().morning.split_once(":")
                .unwrap_or(super::config::DEFAULT_MORNING_TIME.split_once(":").unwrap()),
            Some(NaturalTime::Afternoon) => &cmd_ctx.bot_config().afternoon.split_once(":")
                .unwrap_or(super::config::DEFAULT_AFTERNOON_TIME.split_once(":").unwrap()),
            Some(NaturalTime::Evening) => &cmd_ctx.bot_config().evening.split_once(":")
                .unwrap_or(super::config::DEFAULT_EVENING_TIME.split_once(":").unwrap()),
            None => return Err(ReminderError::InvalidTimeFormat)
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
    let text = caps.name("text").ok_or(ReminderError::EmptyText)?.as_str().to_string();

    Ok(ParsedReminder { text, year, month, day, hour, min, is_auto })
}

/// Build final UTC DateTime for DB and validate its time in the future.
fn build_datetime_utc(
    data: &ParsedReminder, 
    cmd_ctx: &CommandContext
) -> Result<(Zoned, CivilDateTime), ReminderError> {
    // Parse to get month number.
    let month = if let Some(m) = cmd_ctx.i18n.parse_month(data.month.as_str()) {
        m
    } else {
        return Err(ReminderError::InvalidMonth);
    };

    // Set CivilDateTime.
    let y = data.year.parse::<i16>().map_err(|_| ReminderError::InvalidDateFormat)?;
    let m = month;
    let d = data.day.parse::<i8>().map_err(|_| ReminderError::InvalidDateFormat)?;
    let hh = data.hour.parse::<i8>().map_err(|_| ReminderError::InvalidTimeFormat)?;
    let mm = data.min.parse::<i8>().map_err(|_| ReminderError::InvalidTimeFormat)?;

    let civil_dt = CivilDateTime::new(y, m, d, hh, mm, 0, 0)
        .map_err(|_| ReminderError::InvalidDateTimeFormat)?;

    // Convert it to Zoned.
    let user_dt = civil_dt.to_zoned(cmd_ctx.settings.room_tz.clone()).map_err(|_| ReminderError::UnsafeDateTime)?;
    let utc_dt = user_dt.with_time_zone(TimeZone::UTC);

    // Checking that the time is in the future.
    let (utc_dt, civil_dt) = if utc_dt <= Zoned::now().with_time_zone(TimeZone::UTC) {
        if data.is_auto {
            let dt = user_dt.checked_add(1.days())?;
            (dt.with_time_zone(TimeZone::UTC), dt.datetime())
        }
        else { 
            Err(ReminderError::TimeInPast)?
        }
    } else { (utc_dt, civil_dt) };

    Ok((utc_dt, civil_dt))
}
