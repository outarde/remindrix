use matrix_sdk::{
    ruma::{
        OwnedRoomId, OwnedUserId
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
use clap::Parser;

use crate::settings::{SettingsManager};
use crate::handlers::{CommandContext, RemindArgs, CliError};

/// Parsed data of user message for new reminder.
#[derive(Debug)]
pub struct ParsedReminder {
    pub text: String,
    pub year: String,
    pub month: String,
    pub day: String,
    pub hour: String,
    pub min: String,
}

#[derive(Debug, Clone)]
pub struct ReminderData {
    pub utc_dt: DateTime<Utc>,
    pub naive_dt: NaiveDateTime,
    pub text: String,
    pub created_by: OwnedUserId,
    pub settings: SettingsManager
}

pub struct ParsedDate {
    pub day: String,
    pub month: String,
    pub year: String,
    pub is_auto: bool,
}

pub struct ParsedTime {
    pub hour: String,
    pub min: String,
    // is_interval: bool,
    pub interval: TimeDelta,
}

/// If we know date in CLI and want to parse it.
pub fn resolve_date(args: &RemindArgs, room_tz: &Tz) -> Result<ParsedDate, CliError> {
    let today = Utc::now().with_timezone(room_tz).date_naive();

    let mut is_auto: bool = false;

    // Try to get date from --date
    let (day, month, year) = if let Some(date_str) = &args.date {
        let parts: Vec<&str> = date_str.split(['.', '/', '-']).collect();
        
        match parts.as_slice() {
            [d, m, y] => (d.to_string(), m.to_string(), y.to_string()),
            [d, m] => (d.to_string(), m.to_string(), today.year().to_string()),
            [d] => adjust_month_for_day(d, &today)?,
            _ => {
                is_auto = true;
                (today.day().to_string(), today.month().to_string(), today.year().to_string())
            }
        }
    } else {
        (String::new(), String::new(), String::new())
    };
    
    // Try to get date from -d, -m, -y
    let (day, month, year) = match (&args.day, &args.month, &args.year) {
        (Some(d), Some(m), Some(y)) => (d.clone(), m.clone(), y.clone()),
        (Some(d), Some(m), None) => (d.clone(), m.clone(), today.year().to_string()),
        (Some(d), None, None) => adjust_month_for_day(d, &today)?,
        _ => {
            if day.is_empty() {
                is_auto = true;
                (today.day().to_string(), today.month().to_string(), today.year().to_string())
            } else { (day, month, year) }
        }
    };

    Ok( ParsedDate {
        day,
        month,
        year,
        is_auto
    })
}

/// If we want to calculate an interval from days and months from CLI.
/// We check if there is a date. if there is, we calculate the interval from it. 
/// if not, we calculate the interval from time, and leave the date as today.
pub fn resolve_date_interval(args: &RemindArgs, room_tz: &Tz) -> Result<ParsedDate, CliError> {
    // Mutability way
    let mut date = Utc::now().with_timezone(room_tz);

    if let Some(d) = &args.day {
        let d = d.parse::<u64>().unwrap_or(0);
        date = date.checked_add_days(Days::new(d)).ok_or(CliError::UnsafeDateTime)?;
    }

    if let Some(m) = &args.month {
        let m = m.parse::<u32>().unwrap_or(0);
        date = date.checked_add_months(Months::new(m)).ok_or(CliError::UnsafeDateTime)?;
    }

    if let Some(y) = &args.year {
        let y = y.parse::<u32>().unwrap_or(0);
        date = date.checked_add_months(Months::new(y * 12)).ok_or(CliError::UnsafeDateTime)?;
    }

    Ok( ParsedDate {
        day: date.day().to_string(), 
        month: date.month().to_string(), 
        year: date.year().to_string(),
        is_auto: false
    })
}

/// If we know time in CLI and want to parse it.
pub fn resolve_time(
    args: &RemindArgs,
) -> Result<ParsedTime, CliError> {
    // let mut hour: String = String::new();
    // let mut min: String = String::new();

    // Try to get from --time
    let (hour, min) = match &args.time {
        Some(time_str) => {
            let parts: Vec<&str> = time_str.split(':').collect();

            if parts.len() == 2 {
                (parts[0].to_string(), parts[1].to_string())
            }
            else if parts.len() == 1 {
                (parts[0].to_string(), "00".to_string())
            }
            else {
                return Err(CliError::ValidationError("reminder.error.time-format".to_string()));
            }
        },
        None => (String::new(), String::new())
    };
    
    // Try to getn from -h, -m
    let (hour, min) = match (&args.hour, &args.min) {
        (Some(h), Some(m)) => (h.clone(), m.clone()),
        (Some(h), None) => (h.clone(), "00".to_string()),
        (None, Some(m)) => ("09".to_string(), m.clone()),
        (None, None) => {
            // Check if the time has already been written to prevent overwriting.
            if hour.is_empty() {
                // TODO: change to default morning time.
                ("09".to_string(), "00".to_string())
            } else { (hour, min) }
        },
    };

    Ok( ParsedTime {
        hour,
        min, 
        interval: TimeDelta::zero()
    })
}

/// If we want to calculate time interval from CLI.
pub fn resolve_time_interval(args: &RemindArgs, room_tz: &Tz) -> Result<ParsedTime, CliError> {
    // Get now datetime
    let now = Utc::now().with_timezone(room_tz);
    
    let mut delta = TimeDelta::zero();

    // Add hours
    if let Some(h) = &args.hour {
        if let Ok(hours) = h.parse::<i64>() {
            delta = delta + TimeDelta::try_hours(hours).ok_or(CliError::UnsafeDateTime)?;
        }
    }

    // Add minutes
    if let Some(m) = &args.min {
        if let Ok(minutes) = m.parse::<i64>() {
            delta = delta + TimeDelta::try_minutes(minutes).ok_or(CliError::UnsafeDateTime)?;
        }
    }

    // Shifting the current time
    // let future_time = now + delta;

    Ok( ParsedTime {
        hour: now.hour().to_string(),
        min: now.minute().to_string(),
        interval: delta
    })
}

/// The last time related method is to get the time in utc and naive for the DB 
/// and the time offset if a **time** interval was given.
// In short:
// parse str to naive
// get datetime from timezone
// add interval, if any 
// if the date is default and the time is generally in the past, add a day 
// check if the date is in the past 
// get utc, set naive again
// return utc and naive
pub fn resolve_target_dt(
    date: ParsedDate,
    time: ParsedTime,
    tz: &Tz
) -> Result<(DateTime<Utc>, NaiveDateTime), CliError> {
    // Set str with datetime.
    let dt_str = format!("{}-{}-{} {}:{}:00", date.year, date.month, date.day, time.hour, time.min);
    
    // Check if time can be parsed.
    let naive_dt = NaiveDateTime::parse_from_str(&dt_str, "%Y-%m-%d %H:%M:%S")
        .map_err(|_| CliError::ValidationError("reminder.error.datetime-format".to_string()))?;

    // We convert it to DateTime and check that zone mapping has a single result.
    let user_dt = naive_to_datetime(naive_dt.clone(), &tz);
    // Apply TimeDelta if it exists.
    let user_dt = match user_dt.checked_add_signed(time.interval) {
        Some(dt) => dt,
        None => return Err(CliError::UnsafeDateTime)
    };
    let now_dt = Utc::now().with_timezone(tz);

    // Checking that the time is in the future.
    let user_dt = if user_dt <= now_dt {
        if date.is_auto {
            user_dt.checked_add_days(Days::new(1)).ok_or(CliError::UnsafeDateTime)
        } else {
            Err(CliError::ValidationError("reminder.error.past-time".to_string()))
        }?
    } else { user_dt };

    // Get datetime in the UTC time zone.
    let utc_dt = user_dt.with_timezone(&Utc);

    // Set naive again.
    let naive_dt = user_dt.naive_local();

    Ok((utc_dt, naive_dt))
}

// ===== Service =====
/// Try to get SettingsManager with target room_id, room settings
/// for the room for which the reminder was delegated.
// NOTE: Can be changed to try_get_target_context if we need more information 
// and don't want to transfer it to the settings (I18nManager, Room entity, OwnedRoomId)
pub async fn try_get_target_settings(to: &str, cmd_ctx: &CommandContext) -> Result<SettingsManager, CliError> {
    let room_to: OwnedRoomId = to.try_into()
        .map_err(|_| CliError::ValidationError("reminder.delegation-room-format".to_string()))?;

    let target_settings = match cmd_ctx.ctx.client.get_room(&room_to) {
        Some(room) => {
            SettingsManager::new(&room, None, &cmd_ctx.ctx).await
        },
        None => {
            return Err(CliError::ValidationError("reminder.error.delegation-no-room".to_string()));
        }
    };

    Ok(target_settings)
}

/// Convert NaiveDateTime to chrono-tz Datetime<Tz> with checking for a change of season.
pub fn naive_to_datetime(dt_naive: NaiveDateTime, tz: &Tz) -> DateTime<Tz> {
    match tz.from_local_datetime(&dt_naive) {
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(dt_earliest, _) => dt_earliest,
        LocalResult::None => {
            // If the time falls within an hour missed due to the clock change,
            // we move it forward by 1 hour to get out of the "hole".
            let fixed_naive = dt_naive + TimeDelta::hours(1);
            tz.from_local_datetime(&fixed_naive).unwrap()
        }
    }
}

/// Helper function to get the same day a month from d_str.
fn adjust_month_for_day(d_str: &str, today: &NaiveDate) -> Result<(String, String, String), CliError> {
    // Get date number.
    let d = d_str.parse::<u32>().unwrap_or(1);

    let target_month_year = if d < today.day() { 
        match today.checked_add_months(Months::new(1)) {
            Some(d) => d,
            None => return Err(CliError::UnsafeDateTime)
        }
    } else { 
        today.clone()
    };
    Ok((d.to_string(), target_month_year.month().to_string(), target_month_year.year().to_string()))
}
