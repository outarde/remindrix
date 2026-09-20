/*!
* Working with time involves a fundamental ambiguity in this module. 
* On the one hand, when calculating the absolute date (resolve_date) and month, 
* if only the day is specified (adjust_day_fro_month), 
* we calculate the absolute day, because if the day becomes larger or smaller 
* due to a change in time zone, the correspondingly changed time will not be taken into account, 
* and we will receive a reminder that will arrive a day earlier (perplexity) 
* or a day later (indignation). However, time cannot be integrated into these calculations 
* (now the current time is taken, but it can either indicate a time that, unlike the current one, 
* does not fall into time zone change, or vice versa), 
* only if you don’t rewrite the logic for parsing the date and time and trying to glue them together 
* to parse them as numbers, and then perform date formation as additions.
*/

use matrix_sdk::{
    Room,
    ruma::OwnedRoomId
};
use anyhow::Result;
use jiff::{
    Zoned, Span, ToSpan, tz::TimeZone, Timestamp,
    civil::{DateTime as CivilDateTime, Date}
};
use std::string::ToString;

use crate::handlers::RemindArgs;
use crate::context::{CommandContext};
use crate::reminder::{ReminderError, DayPeriod};

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
    pub period: DayPeriod,
    pub interval: Span,
}

/// If we know date in CLI and want to parse it.
pub fn resolve_date(args: &RemindArgs, room_tz: &TimeZone) -> Result<ParsedDate, ReminderError> {
    let today = Zoned::now().with_time_zone(room_tz.clone()).date();
    let mut is_auto: bool = false;

    // Try to get date from --date
    let (day, month, year) = if let Some(date_str) = &args.date {
        parse_date_parts(&date_str, &today, true)?
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
pub fn resolve_date_interval(args: &RemindArgs, room_tz: &TimeZone) -> Result<ParsedDate, ReminderError> {
    // Mutability way
    let mut date = Zoned::now().with_time_zone(room_tz.clone());

    // Try to get date from --date
    if let Some(date_str) = &args.date {
        let (d, m, y) = parse_date_parts(&date_str, &date.date(), false)?;

        if let Ok(days) = d.parse::<i64>() {
            date = date.checked_add(days.days())?;
        }

        if let Ok(months) = m.parse::<i64>() {
            date = date.checked_add(months.months())?;
        }

        if let Ok(years) = y.parse::<i64>() {
            date = date.checked_add(years.years())?;
        }
    }

    // Get from -d, -m, -y
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
    room_tz: &TimeZone,
    default_time: (String, String),
) -> Result<ParsedTime, ReminderError> {
    // Try to get from --time
    let (hour, min, period) = match &args.time {
        Some(time_str) => {
            parse_time_parts_ext(&time_str)?
        },
        None => (String::new(), String::new(), DayPeriod::No)
    };
    
    // Try to get from --hour, --min
    let (hour, min) = match (&args.hour, &args.min) {
        (Some(h), Some(m)) => (h.clone(), m.clone()),
        (Some(h), None) => (h.clone(), "00".to_string()),
        (None, Some(m)) => {
            let now = Zoned::now().with_time_zone(room_tz.clone());
            adjust_hour_for_min(&m, &now)?
            // return Err(ReminderError::InvalidTimeFormat)
        },
        (None, None) => {
            // Check if the time has already been written to prevent overwriting.
            if hour.is_empty() {
                let (h, m) = default_time;
                (h, m)
            } else { (hour, min) }
        },
    };

    Ok(ParsedTime {
        hour,
        min,
        period,
        interval: Span::new()
    })
}

/// If we want to calculate time interval from CLI.
pub fn resolve_time_interval(args: &RemindArgs, room_tz: &TimeZone) -> Result<ParsedTime, ReminderError> {
    let now = Zoned::now().with_time_zone(room_tz.clone());
    let mut delta = Span::new();

    // Try to get from --time
    if let Some(time_str) = &args.time {
        let (h, m, _) = parse_time_parts_ext(&time_str)?;

        if let Ok(hours) = h.parse::<i64>() {
            delta = delta.checked_add(hours.hours())?;
        }
        if let Ok(minutes) = m.parse::<i64>() {
            delta = delta.checked_add(minutes.minutes())?;
        }
    };

    // Get from --hour and --min
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
        period: DayPeriod::No,
        interval: delta
    })
}

/// The last time related method is to get the time in utc and naive for the DB 
/// and the time offset if a **time** interval was given.
pub fn resolve_target_dt(
    date: ParsedDate,
    time: ParsedTime,
    tz: &TimeZone
) -> Result<(Timestamp, CivilDateTime), ReminderError> {
    // Set CivilDateTime.
    // or we can use strtime(): https://docs.rs/jiff/0.2.35/jiff/fmt/strtime/index.html
    let y = date.year.parse::<i16>().map_err(|_| ReminderError::InvalidDateFormat)?;
    let m = date.month.parse::<i8>().map_err(|_| ReminderError::InvalidDateFormat)?;
    let d = date.day.parse::<i8>().map_err(|_| ReminderError::InvalidDateFormat)?;
    let hh = time.hour.parse::<i8>().map_err(|_| ReminderError::InvalidTimeFormat)?;
    let mm = time.min.parse::<i8>().map_err(|_| ReminderError::InvalidTimeFormat)?;

    // Check if time is post meridiem (pm)
    let hh = match time.period {
        DayPeriod::Pm => {
            if hh < 12 {
                hh + 12
            } else { hh }
        },
        DayPeriod::Am => {
            if hh < 12 {
                hh
            } else { 0 }
        },
        _ => hh
    };

    let civil_dt = CivilDateTime::new(y, m, d, hh, mm, 0, 0)
        .map_err(|_| ReminderError::InvalidDateTime)?;

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
    let utc_dt = user_dt.timestamp();

    // Checking that the time is in the future.
    let (utc_dt, civil_dt) = if utc_dt <= Timestamp::now() {
        if date.is_auto {
            let dt = user_dt.checked_add(1.days())?;
            (dt.timestamp(), dt.datetime())
        }
        else { 
            Err(ReminderError::TimeInPast(user_dt))?
        }
    } else { (utc_dt, civil_dt) };

    Ok((utc_dt, civil_dt))
}

// ===== Service =====
/// Get OwnedRoomId and return Room from &str. Used for delegation in process_cli().
pub async fn parse_room(to: &str, cmd_ctx: &CommandContext) -> Result<Room, ReminderError> {
    let parsed_room: OwnedRoomId = to.try_into()
        .map_err(|_| ReminderError::InvalidDelegationRoomFormat)?;
    let room = cmd_ctx.ctx.client.get_room(&parsed_room)
        .ok_or(ReminderError::NoDelegatedRoom)?;
    Ok(room)
}

/// Helper function to get the same day a month from d_str.
/// It's Date, not Zoned because we do not need to account for the possibility of shifting to a 
/// different date when the user wants a specific day within the current or another month.
fn adjust_month_for_day(d_str: &str, today: &Date) -> Result<(String, String, String), ReminderError> {
    // Get date number.
    // We do not check whether such a date exists (or it is 32th), 
    // so that we can verify everything at resolve_target_dt() later.
    let d = d_str.parse::<i8>().map_err(|_| ReminderError::InvalidDateFormat)?;

    let target_date = if d < today.day() {
        // let current_moment = Zoned::now().with_time_zone(room_tz.clone());
        today.checked_add(1.months())?
    } else { 
        today.clone()
    };

    Ok((d.to_string(), target_date.month().to_string(), target_date.year().to_string()))
}

/// Helper function to get the nearest hour for the given minute.
/// We use the "zone" type rather than "time" because if a user specifies minutes without an hour, 
/// they have not specified a date; this ensures that any time offsets resulting from the switch 
/// between daylight saving and standard time are handled correctly relative to the current date. 
/// Unlike the adjust_month_for_day(), this approach does not require changing 
/// dates or accounting for a date (time in adjust_month_for_day) other than the current one.
fn adjust_hour_for_min(m_str: &str, now: &Zoned) -> Result<(String, String), ReminderError> {
    let m = m_str.parse::<i8>().map_err(|_| ReminderError::InvalidTimeFormat)?;

    let target_hour = if m < now.minute() {
        now.checked_add(1.hours())?.hour()
    } else { 
        now.hour()
    };

    Ok((target_hour.to_string(), m.to_string()))
}

/// Parse day, month and year from the &str.
fn parse_date_parts(date_str: &str, today: &Date, adjust: bool) -> Result<(String, String, String), ReminderError> {
    let parts: Vec<&str> = date_str.split(['.', '/', '-']).collect();
        
    match parts.as_slice() {
        [d, m, y] => Ok((d.to_string(), m.to_string(), y.to_string())),
        [d, m] => {
            Ok({
                if adjust { (d.to_string(), m.to_string(), today.year().to_string()) }
                else { (d.to_string(), m.to_string(), "00".to_string()) }
            })
        },
        [d] => {
            Ok({
                if adjust { adjust_month_for_day(d, &today)? } 
                else { (d.to_string(), "00".to_string(), "00".to_string()) }
            })
        },
        _ => {
            Err(ReminderError::InvalidDateFormat)
        }
    }
}

/// Parse hours and minutes from the &str %H:%M.
fn _parse_time_parts(time_str: &str) -> Result<(String, String), ReminderError> {
    let parts: Vec<&str> = time_str.split(&[':', '.']).collect();

    if parts.len() == 2 {
        Ok((parts[0].to_string(), parts[1].to_string()))
    }
    else if parts.len() == 1 {
        Ok((parts[0].to_string(), "00".to_string()))
    }
    else {
        Err(ReminderError::InvalidTimeFormat)
    }
}

/// Parse hours and minutes from the &str %H:%M with the DayPeriod support.
fn parse_time_parts_ext(time_str: &str) -> Result<(String, String, DayPeriod), ReminderError> {
    // Find am/pm.
    let lower_str = time_str.to_lowercase();
    let period = if lower_str.contains("am") { // or we can use ends_with()
        DayPeriod::Am
    } else if lower_str.contains("pm") {
        DayPeriod::Pm
    } else {
        DayPeriod::No
    };

    // Sanitize string.
    // this makes it impossible to use dot notation
    let sanitized: String = time_str
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ':' || *c == '.')
        .collect();

    // Split it.
    let parts: Vec<&str> = sanitized.split(&[':', '.']).collect();

    match parts.len() {
        2 => Ok((parts[0].to_string(), parts[1].to_string(), period)),
        1 => Ok((parts[0].to_string(), "00".to_string(), period)),
        _ => Err(ReminderError::InvalidTimeFormat)
    }
}
