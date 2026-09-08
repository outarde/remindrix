use matrix_sdk::{
    ruma::{
        OwnedRoomId, OwnedUserId
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

use crate::settings::SettingsManager;
use crate::handlers::{CommandContext, RemindArgs, CliError};
use crate::reminder::ReminderError;


#[derive(Debug, Clone)]
pub struct ReminderData {
    pub utc_dt: Zoned,
    pub civil_dt: CivilDateTime,
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
    pub interval: Span,
}

/// If we know date in CLI and want to parse it.
pub fn resolve_date(args: &RemindArgs, room_tz: &TimeZone) -> Result<ParsedDate, ReminderError> {
    let today = Zoned::now().with_time_zone(room_tz.clone()).date();

    let mut is_auto: bool = false;

    // Try to get date from --date
    let (day, month, year) = if let Some(date_str) = &args.date {
        let parts: Vec<&str> = date_str.split(['.', '/', '-']).collect();
        
        match parts.as_slice() {
            [d, m, y] => (d.to_string(), m.to_string(), y.to_string()),
            [d, m] => (d.to_string(), m.to_string(), today.year().to_string()),
            [d] => adjust_month_for_day(d, &today, &room_tz)?,
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
        (Some(d), None, None) => adjust_month_for_day(d, &today, &room_tz)?,
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
pub fn resolve_date_interval(args: &RemindArgs, room_tz: &TimeZone) -> Result<ParsedDate, ReminderError> {
    // Mutability way
    let mut date = Zoned::now().with_time_zone(room_tz.clone());

    if let Some(d) = &args.day {
        let d = d.parse::<i64>().unwrap_or(0);
        date = date.checked_add(d.days())?;
    }

    if let Some(m) = &args.month {
        let m = m.parse::<i64>().unwrap_or(0);
        date = date.checked_add(m.months())?;
    }

    if let Some(y) = &args.year {
        let y = y.parse::<i64>().unwrap_or(0);
        date = date.checked_add(y.years())?;
    }

    let civil_d = date.date();

    Ok(ParsedDate {
        day: civil_d.day().to_string(), 
        month: civil_d.month().to_string(), 
        year: civil_d.year().to_string(),
        is_auto: false
    })
}

/// If we know time in CLI and want to parse it.
pub fn resolve_time(
    args: &RemindArgs,
) -> Result<ParsedTime, ReminderError> {
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
                return Err(ReminderError::InvalidTimeFormat);
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
        interval: Span::new()
    })
}

/// If we want to calculate time interval from CLI.
pub fn resolve_time_interval(args: &RemindArgs, room_tz: &TimeZone) -> Result<ParsedTime, ReminderError> {
    let now = Zoned::now().with_time_zone(room_tz.clone());
    let mut delta = Span::new();

    // Add hours
    if let Some(h) = &args.hour {
        if let Ok(hours) = h.parse::<i64>() {
            // Or: Span::new().hours(hours)
            delta = delta.checked_add(hours.hours())?;
        }
    }

    // Add minutes
    if let Some(m) = &args.min {
        if let Ok(minutes) = m.parse::<i64>() {
            delta = delta.checked_add(minutes.minutes())?;
        }
    }

    Ok(ParsedTime {
        hour: now.hour().to_string(),
        min: now.minute().to_string(),
        interval: delta
    })
}

/// The last time related method is to get the time in utc and naive for the DB 
/// and the time offset if a **time** interval was given.
pub fn resolve_target_dt(
    date: ParsedDate,
    time: ParsedTime,
    tz: &TimeZone
) -> Result<(Zoned, CivilDateTime), ReminderError> {
    // Set CivilDateTime.
    let y = date.year.parse::<i16>().map_err(|_| ReminderError::InvalidDateFormat)?;
    let m = date.month.parse::<i8>().map_err(|_| ReminderError::InvalidDateFormat)?;
    let d = date.day.parse::<i8>().map_err(|_| ReminderError::InvalidDateFormat)?;
    let hh = time.hour.parse::<i8>().map_err(|_| ReminderError::InvalidTimeFormat)?;
    let mm = time.min.parse::<i8>().map_err(|_| ReminderError::InvalidTimeFormat)?;

    let civil_dt = CivilDateTime::new(y, m, d, hh, mm, 0, 0)
        .map_err(|_| ReminderError::InvalidDateTimeFormat)?;

    // Convert it to Zoned.
    let user_dt = civil_dt.to_zoned(tz.clone())?;

    // Apply Span if it exists and update CivilDateTime.
    let (user_dt, civil_dt) = if !time.interval.is_zero() {
        let dt = user_dt.checked_add(time.interval)?;
        (dt.clone(), dt.datetime())
        /*
        match user_dt.checked_add(time.interval) {
            Ok(dt) => {
                (dt, dt.datetime())
            }
            Err(_) => return Err(ReminderError::UnsafeDateTime)
        }
        */
    } else {
        (user_dt, civil_dt)
    };

    // Get datetime in the UTC time zone.
    let utc_dt = user_dt.with_time_zone(TimeZone::UTC);

    // Checking that the time is in the future.
    let (utc_dt, civil_dt) = if utc_dt <= Zoned::now().with_time_zone(TimeZone::UTC) {
        if date.is_auto {
            let dt = user_dt.checked_add(1.days())?;
            (dt.with_time_zone(TimeZone::UTC), dt.datetime())
        }
        else { 
            Err(ReminderError::TimeInPast)?
        }
    } else { (utc_dt, civil_dt) };

    Ok((utc_dt, civil_dt))
}

// ===== Service =====
/// Try to get SettingsManager with target room_id, room settings
/// for the room for which the reminder was delegated.
// NOTE: Can be changed to try_get_target_context if we need more information 
// and don't want to transfer it to the settings (I18nManager, Room entity, OwnedRoomId)
pub async fn try_get_target_settings(to: &str, cmd_ctx: &CommandContext) -> Result<SettingsManager, ReminderError> {
    let room_to: OwnedRoomId = to.try_into()
        .map_err(|_| ReminderError::InvalidDelegationRoomFormat)?;

    let target_settings = match cmd_ctx.ctx.client.get_room(&room_to) {
        Some(room) => {
            SettingsManager::new(&room, None, &cmd_ctx.ctx).await
        },
        None => {
            return Err(ReminderError::NoDelegatedRoom);
        }
    };

    Ok(target_settings)
}

/// Helper function to get the same day a month from d_str.
fn adjust_month_for_day(d_str: &str, today: &Date, room_tz: &TimeZone) -> Result<(String, String, String), ReminderError> {
    // Get date number.
    let d = d_str.parse::<i8>().unwrap_or(1);

    let target_date = if d < today.day() {
        let current_moment = Zoned::now().with_time_zone(room_tz.clone());
        current_moment.checked_add(1.months())?.date()
    } else { 
        today.clone()
    };

    Ok((d.to_string(), target_date.month().to_string(), target_date.year().to_string()))
}
