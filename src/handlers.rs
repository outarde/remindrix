use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Client, Room, RoomState,
    ruma::{
        room_id,
        RoomId, OwnedRoomId, OwnedEventId,
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
use crate::reminder::ReminderStatus;
use crate::settings::{RoomTimezoneContent, SettingsManager};

// Compile regex only once
static REMINDER_REGEX: OnceLock<Regex> = OnceLock::new();
static MENTION_REGEX: OnceLock<Regex> = OnceLock::new();

enum BotCommand {
    Remind,
    List,
    Tz,
}

impl BotCommand {
    fn parse(text: &str, cmd_ctx: &CommandContext) -> Option<(Self, String)> {
        // Numbers of chars to skip
        let mut skip_num = 1;

        // Check if it is no prefix and it is not required in config.
        if !text.starts_with('/') && !text.starts_with('!') {
            if cmd_ctx.bot_config().on_command { return None; };
            if cmd_ctx.is_room_group() && cmd_ctx.bot_config().on_command_group {
                return None;
            }
            skip_num = 0;
        }

        let remaining: String = text.chars().skip(skip_num).collect();
        let mut parts = remaining.splitn(2, ' ');

        // command and args from text
        // next()? returns None if no content in Vec
        let cmd_str = parts.next()?.to_lowercase();
        let args = parts.next().unwrap_or("").to_string();

        if cmd_ctx.bot_config().remind_commands.contains(&cmd_str) || &cmd_ctx.i18n.cmd_remind == &cmd_str {
            Some((BotCommand::Remind, args))
        } else if cmd_ctx.bot_config().list_commands.contains(&cmd_str) {
            Some((BotCommand::List, args))
        } else if cmd_ctx.bot_config().tz_commands.contains(&cmd_str) {
            Some((BotCommand::Tz, args))
        } else {
            // Return "remind" command for fast only-remind creations only in private rooms.
            if cmd_ctx.bot_config().quick_remind && !cmd_ctx.is_room_group() {
                Some((BotCommand::Remind, text.to_string()))
            } else { return None; }
        }
    }
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

/// Keys of i18n for build_datetime_str() reply in case of error.
#[derive(Debug, Display)]
enum ReminderDateError {
    #[strum(serialize = "reminder.error.month")]
    InvalidMonth,
    #[strum(serialize = "reminder.error.past-time")]
    TimeInPast,
    #[strum(serialize = "reminder.error.time")]
    InvalidTime,
    #[strum(serialize = "reminder.error.summer-time")]
    SummerTime,
}

// #[derive(strum_macros::Display)]
// #[strum(to_string = "")]
#[derive(PartialEq, EnumString, Display)]
#[strum(serialize_all = "snake_case")]
pub enum MessageReaction {
    #[strum(serialize = "👍")]
    ThumbsUp,
    #[strum(serialize = "✅")]
    Check,
    #[strum(serialize = "❌")]
    Cross,
    #[strum(serialize = "🟢")]
    Done,
    #[strum(serialize = "⏲️")]
    Timer,
    #[strum(serialize = "🕒")]
    Clock,
    #[strum(serialize = "0️⃣")]
    Zero,
    #[strum(serialize = "1️⃣")]
    One,
    #[strum(serialize = "2️⃣")]
    Two,
    #[strum(serialize = "3️⃣")]
    Three,
    #[strum(serialize = "4️⃣")]
    Four,
    #[strum(serialize = "5️⃣")]
    Five,
    #[strum(serialize = "6️⃣")]
    Six,
    #[strum(serialize = "7️⃣")]
    Seven,
    #[strum(serialize = "8️⃣")]
    Eight,
    #[strum(serialize = "9️⃣")]
    Nine,
}

impl MessageReaction {
    /// Turn digits to <MessageReaction>.
    fn from_digit(digit: u32) -> Self {
        match digit {
            0 => Self::Zero,
            1 => Self::One,
            2 => Self::Two,
            3 => Self::Three,
            4 => Self::Four,
            5 => Self::Five,
            6 => Self::Six,
            7 => Self::Seven,
            8 => Self::Eight,
            9 => Self::Nine,
            _ => Self::Cross,
        }
    }
}

/// Parse i18n, like months names
#[derive(Debug)]
pub struct I18nManager {
    cmd_remind: String,

    months: Vec<String>, 
    today: String,
    tomorrow: String,
    morning: String,
    afternoon: String,
    evening: String,

    days: Vec<String>,
    times: Vec<String>,
    prepositions: Vec<String>,
}

impl I18nManager {
    /// Create new Manager for locale
    pub fn new_for_locale(locale: &str) -> Self {
        let cmd_remind = t!("reminder.command", locale = locale).to_string();
        let i18n_months_str = t!("months", locale = locale);
        let months = i18n_months_str.split_whitespace().map(|s| s.to_string()).collect();

        let today = t!("dates.today", locale = locale).to_string();
        let tomorrow = t!("dates.tomorrow", locale = locale).to_string();
        let morning = t!("times.morning", locale = locale).to_string();
        let afternoon = t!("times.afternoon", locale = locale).to_string();
        let evening = t!("times.evening", locale = locale).to_string();
        let i18n_prepositions_str = t!("prepositions", locale = locale);

        let days = vec![today.clone(), tomorrow.clone()];
        let times = vec![morning.clone(), afternoon.clone(), evening.clone()];
        let prepositions = i18n_prepositions_str.split_whitespace().map(|s| s.to_string()).collect();

        Self { 
            cmd_remind,
            months,
            today,
            tomorrow,
            morning,
            afternoon,
            evening,
            days,
            times,
            prepositions,
        }
    }

    // User Input -> Month Number
    pub fn parse_month(&self, input: &str) -> Option<u8> {
        // If number
        if let Ok(m) = input.parse::<u8>() {
            if (1..=12).contains(&m) {
                return Some(m);
            }
        }

        // If name
        let idx = self.months.iter()
            .position(|name| {
                (name == input || name.starts_with(input)) && input.len() >= 3
            })?;

        Some((idx + 1) as u8)
    }

    // Number -> Month name, short name
    pub fn format_month(&self, month_num: &u32) -> Option<(String, String)> {
        if (1..=12).contains(month_num) {
            let name = self.months[(month_num - 1) as usize].clone();
            let short_name = name.get(0..3).unwrap_or(&month_num.to_string()).to_string();
            Some((name, short_name))
        } else {
            None
        }
    }
}

/// Context for current interaction with user.
#[derive(Clone, Debug)]
pub struct CommandContext {
    pub room: Room,
    pub room_id: OwnedRoomId,
    pub ctx: Arc<super::BotContext>,
    pub settings: SettingsManager,
    pub i18n: Arc<I18nManager>,
}

impl CommandContext {
    pub async fn new(room: Room, ctx: Arc<super::BotContext>) -> Self { 
        let settings = SettingsManager::new(&room, &ctx).await;
        let i18n = ctx.get_i18n_manager(&settings.room_lang).await;
        let room_id = room.room_id().to_owned();

        Self { room, room_id, ctx, settings, i18n } 
    }

    // getters
    // cmd_ctx.ctx.bot_config
    pub fn bot_config(&self) -> &super::config::BotConfig {
        &self.ctx.bot_config
    }
    // Check if room has more than 2 active (joined and invitees) members
    pub fn is_room_group(&self) -> bool {
        self.room.active_members_count() > 2 as u64
    }
}

/// Parsed data of user message for new reminder.
#[derive(Debug)]
struct ParsedReminder {
    text: String,
    year: String,
    month: String,
    day: String,
    hour: String,
    min: String,
}

/// CLI commands for new reminders.
#[derive(Parser, Debug)]
pub struct RemindArgs {
    #[arg(short, long)]
    pub day: Option<String>,
    #[arg(short, long)]
    pub month: Option<String>,
    #[arg(short, long)]
    pub year: Option<String>,
    #[arg(long)]
    pub date: Option<String>,

    #[arg(long)]
    pub hour: Option<String>,
    #[arg(long)]
    pub min: Option<String>,
    #[arg(long)]
    pub time: Option<String>,
    
    pub text: Vec<String>,

    #[arg(long)]
    pub to: Option<String>,
    #[arg(short, long)]
    pub interval: bool,
    #[arg(short, long)]
    pub repeat: Option<String>,
}

impl RemindArgs {
    /// Checks whether the fields of the structure that allow to determine 
    /// that the user actually entered a CLI command are Some.
    pub fn enough_options_are_some(&self) -> bool {
        let options = [
            self.day.as_ref(),
            self.month.as_ref(),
            self.year.as_ref(),
            self.date.as_ref(),
            self.hour.as_ref(),
            self.min.as_ref(),
            self.time.as_ref(),
            self.to.as_ref(),
            self.repeat.as_ref(),
        ];

        options.iter().find(|opt| opt.is_some()).is_some()
    }
}

/// Erros for CLi proccesing.
#[derive(Debug)]
enum CliError {
    ClapError(clap::Error),
    NaturalFallback,
    ValidationError(String),
}

// From for operator ?
impl From<clap::Error> for CliError {
    fn from(err: clap::Error) -> Self {
        CliError::ClapError(err)
    }
}
impl From<chrono::ParseError> for CliError {
    fn from(_err: chrono::ParseError) -> Self {
        CliError::ValidationError("reminder.error.date-format".to_string())
    }
}

/// CLI commands for time zone.
#[derive(Parser, Debug)]
pub struct TzArgs {
    pub set: Option<String>,
}

/// CLI commands for list of reminders.
#[derive(Parser, Debug)]
pub struct ListArgs {
    #[arg(short, long)]
    pub pending: bool,
}

// ===== Entry Point =====
/// Reply to incoming message
pub async fn on_room_message(
    event: OriginalSyncRoomMessageEvent, 
    room: Room, 
    ctx: Arc<super::BotContext>
) {
    // Check if we joined the room
    if room.state() != RoomState::Joined { return; }
    // We don't reply to our message
    // can be changed if multi-step interaction with bot will be presented
    if event.sender == ctx.bot_id { return; }

    // Check for text type of message
    let MessageType::Text(text_content) = &event.content.msgtype else { return };
    let mut body = text_content.body.trim().to_string();

    // Command Context
    let cmd_ctx = CommandContext::new(room.clone(), ctx.clone()).await;

    // if ctx.bot_config.on_mention is true, 
    // check if bot was mentioned in public rooms
    if ctx.bot_config.on_mention && cmd_ctx.is_room_group() {
        if let Some(mentions) = &event.content.mentions {
            // clean body from makrdown if there is mention
            if mentions.user_ids.contains(&ctx.bot_id) {
                body = clean_from_mention(&body);
            }
            else {
                return;
            }
        } else {
            return;
        }
    }

    // Parse command
    let Some((command, args)) = BotCommand::parse(&body, &cmd_ctx) else { 
        return;
    };

    match command {
        BotCommand::Remind => {
            handle_remind(&args, event.clone(), cmd_ctx).await;
        }
        BotCommand::List => {
            // handle_list(&room, &db).await;
            return;
        }
        BotCommand::Tz => {
            handle_tz(&args, event.clone(), cmd_ctx).await;
            return;
        }
    }
}

// ===== Handlers =====
/// Handle new reminder router.
pub async fn handle_remind(
    args_str: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) {

    let args: Vec<&str> = args_str.split_whitespace().collect();
    // clap requires some command at the first place
    let mut clap_input = vec!["remind"];
    clap_input.extend(&args);

    // Try to parse in CLI mode at first.
    // try_parse_from() from clap only throws an error when there is a parsing error, 
    // not when there are empty values, so we put it in another function.
    match process_cli_reminder(clap_input, event.clone(), cmd_ctx.clone()).await {
        Ok(_) => return,
        Err(CliError::ClapError(_err)) => {
            process_natural_reminder(args_str, event.clone(), cmd_ctx.clone()).await;
        },
        Err(CliError::NaturalFallback) => {
            process_natural_reminder(args_str, event.clone(), cmd_ctx.clone()).await;
        },
        Err(CliError::ValidationError(err)) => {
            let err_msg = t!(err); 
            let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(err_msg)).await;
        }
    }
}

/// Create reminder with CLI recognition.
/// Extended capabilities such as reminder for other user, intervals, etc.
pub async fn process_cli_reminder(
    args_str: Vec<&str>,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> Result<(), CliError> {
    let args = RemindArgs::try_parse_from(args_str)?;

    // Without this check, the parser will perceive any text as a --text parameter.
    if !args.enough_options_are_some() {
        return Err(CliError::NaturalFallback);
    }

    // Check if text is empty.
    if args.text.len() == 0 {
        return Err(CliError::ValidationError("reminder.error.empty-text".to_string()));
    }

    // Delegation.
    let target_room_id = if let Some(to) = args.to.as_deref() {
        let room_to: OwnedRoomId = to.try_into()
            .map_err(|_| CliError::ValidationError("reminder.delegation-room-format".to_string()))?;
        Some(room_to)
    } else { None };

    // Parse date.
    let (day, month, year) = if args.interval {
        resolve_date_interval(&args, &cmd_ctx.settings.room_tz).unwrap()
    } else {
        resolve_date(&args, &cmd_ctx.settings.room_tz).unwrap()
    };

    // Parse NaiveDate from parsed date.
    let date_str = format!("{}-{}-{}", year, month, day);
    let date_naive = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")?;

    // Get target datetime with parsed time and changed date if necessary.
    let target_dt = match match args.interval {
        true => resolve_time_interval(&args, &cmd_ctx.settings.room_tz, date_naive),
        false => resolve_time(&args, &cmd_ctx.settings.room_tz, date_naive)
    } {
        Ok(dt) => dt,
        // None => return Err(CliError::ValidationError("reminder.error.time".to_string()))
        Err(e) => return Err(e)
    };

    //////
    // Instead of build_datetime_utc
    // BUT it required because: WE HAVEN'T CHECKED TIME FORMAT like 26:00, etc??
    /*
    let naive_dt = target_dt.naive_local();
    let utc_dt = target_dt.with_timezone(&Utc);

    // Checking that the time is in the future
    if utc_dt <= Utc::now() {
        return Err(CliError::ValidationError("reminder.error.past-time".to_string()));
    }
    ///////
    */

    // Fill a structure.
    let reminder_data = ParsedReminder { 
        text: args.text.join(" ").to_string(), 
        year: target_dt.year().to_string(), 
        month: target_dt.month().to_string(), 
        day: target_dt.day().to_string(), 
        hour: target_dt.hour().to_string(), 
        min: target_dt.minute().to_string()
    };

    // Save reminder.
    let _ = process_saving(event, reminder_data, cmd_ctx, target_room_id).await;

    Ok(())
}

/// If we know time in CLI and want to parse it.
fn resolve_time(args: &RemindArgs, room_tz: &Tz, start_d_naive: NaiveDate) -> Result<(DateTime<Tz>), CliError> {
    // let now_naive = Utc::now().with_timezone(room_tz).date_naive().and_time(Utc::now().with_timezone(room_tz).time());
    let now_naive = Utc::now().with_timezone(room_tz).naive_local();

    let mut hour: String = String::new();
    let mut min: String = String::new();

    // Try to get from --time
    match &args.time {
        Some(time_str) => {
            let parts: Vec<&str> = time_str.split(':').collect();
            if parts.len() == 2 {
                (hour, min) = (parts[0].to_string(), parts[1].to_string());
            }
            else if parts.len() == 1 {
                (hour, min) = (parts[0].to_string(), "00".to_string());
            }
            else {
                return Err(CliError::ValidationError("reminder.error.time-format".to_string()));
            }
        },
        None => ()
    };
    
    // Try to getn from -h, -m
    match (&args.hour, &args.min) {
        (Some(h), Some(m)) => (hour, min) = (h.clone(), m.clone()),
        (Some(h), None) => (hour, min) = (h.clone(), "00".to_string()),
        (None, Some(m)) => (hour, min) = ("09".to_string(), m.clone()),
        (None, None) => {
            // Check if the time has already been written to prevent overwriting.
            if hour.is_empty() {
                (hour, min) = ("09".to_string(), "00".to_string())
            }
        },
    };

    let time = format!("{}:{}", hour, min);
    let time_naive = match NaiveTime::parse_from_str(&time, "%H:%M") {
        Ok(dt) => dt,
        Err(_) => return Err(CliError::ValidationError("reminder.error.time-format".to_string()))
    };
    let dt_naive = start_d_naive.and_time(time_naive);

    // date str "2026-8-28"
    // date naive 1 (start naive) 2026-08-28
    // now 2 is 2026-08-28T23:02:13.142228, now naive is 2026-08-28T22:02:13.142228
    // time "22:00"
    // after time dt_naive 2026-08-28T22:00:00
    // checkpoint
    // target_dt final 2026-08-29T22:00:00IST

    // date str "2026-8-28"
    // date naive 1 (start naive) 2026-08-28
    // now 2 is 2026-08-28T23:36:22.951168, naive local is 2026-08-28T23:36:22.951169
    // time "22:00"
    // after time dt_naive 2026-08-28T22:00:00
    // checkpoint
    // target_dt final 2026-08-29T22:00:00IST

    // If user's input date is today because of date-autofill, and reminder time is in the past, 
    // convert date to tomorrow.
    let dt_naive = if now_naive >= dt_naive && (args.date.is_none() || args.day.is_none()) {
        dt_naive + Days::new(1)
    } else { dt_naive };

    Ok(naive_to_datetime(dt_naive, &room_tz))
    // Ok((dt_naive, naive_to_datetime(dt_naive, &room_tz)))
}

/// If we want to calculate time interval from CLI with start date.
fn resolve_time_interval(args: &RemindArgs, room_tz: &Tz, start_d_naive: NaiveDate) -> Result<(DateTime<Tz>), CliError> {
    // Get start date with the current time
    let now_naive = Utc::now().with_timezone(room_tz).time();
    let start_dt_naive = start_d_naive.and_time(now_naive);
    
    // To DateTime<Tz> with checking for a change of season
    let start_dt = naive_to_datetime(start_dt_naive, &room_tz);
    
    let mut delta = TimeDelta::zero();

    // Add hours
    if let Some(h) = &args.hour {
        if let Ok(hours) = h.parse::<i64>() {
            delta = delta + TimeDelta::hours(hours);
        }
    }

    // Add minutes
    if let Some(m) = &args.min {
        if let Ok(minutes) = m.parse::<i64>() {
            delta = delta + TimeDelta::minutes(minutes);
        }
    }

    // If the interval is not specified, we return the start datetime
    if delta == TimeDelta::zero() {
        return Ok(start_dt);
    }

    // Shifting the current time
    let future_time = start_dt + delta;

    Ok(future_time)
}

/// If we know date in CLI and want to parse it.
fn resolve_date(args: &RemindArgs, room_tz: &Tz) -> Option<(String, String, String)> {
    let today = Utc::now().with_timezone(room_tz).date_naive();
    
    // helper function to get the same day a month from d_str
    let adjust_month_for_day = |d_str: &str| -> (String, String, String) {
        let d = d_str.parse::<u32>().unwrap_or(1);
        let target_date = if d < today.day() { 
            today + Months::new(1) 
        } else { today };
        (d.to_string(), target_date.month().to_string(), target_date.year().to_string())
    };

    // Try to get date from --date
    if let Some(date_str) = &args.date {
        let parts: Vec<&str> = date_str.split(['.', '/', '-']).collect();
        
        return match parts.as_slice() {
            [d, m, y] => Some((d.to_string(), m.to_string(), y.to_string())),
            [d, m] => Some((d.to_string(), m.to_string(), today.year().to_string())),
            [d] => Some(adjust_month_for_day(d)),
            _ => {
                let tmrw = today + Days::new(1);
                Some((tmrw.day().to_string(), tmrw.month().to_string(), tmrw.year().to_string()))
            }
        };
    }
    
    // Try to get date from -d, -m, -y
    match (&args.day, &args.month, &args.year) {
        (Some(d), Some(m), Some(y)) => Some((d.clone(), m.clone(), y.clone())),
        (Some(d), Some(m), None) => Some((d.clone(), m.clone(), today.year().to_string())),
        (Some(d), None, None) => Some(adjust_month_for_day(d)),
        _ => {
            // let tmrw = today + Days::new(1);
            Some((today.day().to_string(), today.month().to_string(), today.year().to_string()))
        }
    }
}

/// If we want to calculate an interval from days and months from CLI.
/// We check if there is a date. if there is, we calculate the interval from it. 
/// if not, we calculate the interval from time, and leave the date as today.
fn resolve_date_interval(args: &RemindArgs, room_tz: &Tz) -> Option<(String, String, String)> {
    // Shadowing way
    /*
    let date = Utc::now().with_timezone(room_tz).date_naive();
    
    let date = if let Some(d) = &args.day {
        let d = d.parse::<u64>().unwrap_or(0);
        date + Days::new(d)
    } else { 
        date
    };

    let date_with_int = if let Some(m) = &args.month {
        let m = m.parse::<u32>().unwrap_or(0);
        date + Months::new(m)
    } else { 
        date
    };
    */

    // Mutability way
    let mut date = Utc::now().with_timezone(room_tz).date_naive();

    if let Some(d) = &args.day {
        let d = d.parse::<u64>().unwrap_or(0);
        date = date + Days::new(d);
    }

    if let Some(m) = &args.month {
        let m = m.parse::<u32>().unwrap_or(0);
        date = date + Months::new(m);
    }

    return Some((date.day().to_string(), date.month().to_string(), date.year().to_string())); 
}

/// Save reminder with ParsedReminder.
async fn process_saving(
    event: OriginalSyncRoomMessageEvent, 
    reminder_data: ParsedReminder, 
    cmd_ctx: CommandContext,
    target_room_id: Option<OwnedRoomId>,
) {
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

    // 
    let target_room_id = match target_room_id {
        Some(id) => id,
        None => cmd_ctx.room_id.clone()
    };

    //
    // Instead of settings we can get CommandContext. It will be shorter and includes 
    // room and settings. Memory saving in the current version is only that we clone()
    // cmd_ctx.settings and not the whole cmd_ctx with room, etc.
    let target_settings = if target_room_id != cmd_ctx.room_id {
        match cmd_ctx.ctx.client.get_room(&target_room_id) {
            Some(room) => {
                SettingsManager::new(&room, &cmd_ctx.ctx).await
            },
            None => {
                let err_msg = t!("reminder.error.delegation-no-room"); 
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(err_msg)).await;
                return;
            }
        }
    } else { cmd_ctx.settings.clone() };

    /*
    let target_room = if let Some(id) = &target_room_id {
        match cmd_ctx.ctx.client.get_room(id) {
            Some(room) => room,
            None => {
                let err_msg = t!("reminder.error.delegation-no-room"); 
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(err_msg)).await;
                return;
            }
        }
    } else { cmd_ctx.room.clone() };

    let target_settings = SettingsManager::new(&target_room, &cmd_ctx.ctx).await;
    */

    // Save to DB.
    match super::reminder::save_reminder_to_db_extended(
        cmd_ctx.ctx.db.clone(),
        target_room_id,
        target_settings.room_tz, 
        naive_dt.clone(), 
        utc_dt.clone(),
        reminder_data.text,
    ).await {
        Ok(new_reminder) => {
            // Schedule it.
            // TODO: pass target_settings to schedule_reminder_utc for language compatibility
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

// ===== Natural Pipeline =====
/// Create reminder with regular expression recognition.
/// Original pipeline with base capability.
async fn process_natural_reminder(
    args_str: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) {
    // Make regular expression
    let re = build_reminder_regex(&cmd_ctx.i18n);

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
        println!("{:?}", reminder_data);
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
        send_welcome_message(cmd_ctx).await;
    }
}

/// Handle changing time zone.
async fn handle_tz(
    body: &str,
    ev: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) {
    // Update timezone if we have one in the input.
    if !body.is_empty() {
        // Parse user's input timezone code
        let input_tz = match super::settings::parse_tz(&body) {
            Ok(tz) => tz,
            Err(err) => {
                let err_msg = t!("tz.invalid-format"); 
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(err_msg)).await;
                
                tracing::error!("Invalid user timezone: {err:?}");
                return;
            }
        };

        // If user's input timezone is equal to current room timezone
        if input_tz == cmd_ctx.settings.room_tz {
            let msg = t!("tz.not-set"); 
            let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(msg)).await;
        }
        else {
            let _ = cmd_ctx.settings.set_room_tz(&cmd_ctx, input_tz).await;

            if cmd_ctx.bot_config().send_reactions {
                let _ = send_reaction(ev.event_id.clone(), &cmd_ctx, MessageReaction::Check).await;
            } else {
                let msg = t!("tz.set", tz = input_tz.to_string()); 
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(msg)).await;
            }
        }
    }
    // Send current timezone.
    else {
        let msg = t!("tz.current", tz = &cmd_ctx.settings.room_tz.to_string());
        let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(msg)).await;
    }
}

/// Auto-join.
pub async fn on_stripped_state_member(
    room_member: StrippedRoomMemberEvent,
    room: Room,
    ctx: Arc<super::BotContext>,
) {
    if room_member.state_key != ctx.client.user_id().unwrap() {
        return;
    }

    tokio::spawn(async move {
        println!("Autojoining room {}", room.room_id());
        let mut delay = 2;

        while let Err(err) = room.join().await {
            // retry autojoin due to synapse sending invites, before the
            // invited user can join for more information see
            // https://github.com/matrix-org/synapse/issues/4345
            tracing::error!("Failed to join room {} ({err:?}), retrying in {delay}s", room.room_id());

            sleep(Duration::from_secs(delay)).await;
            delay *= 2;

            if delay > 3600 {
                tracing::error!("Can't join room {} ({err:?})", room.room_id());
                break;
            }
        }

        tracing::info!("Successfully joined room {}", room.room_id());

        // Send welcome message.
        let cmd_ctx = CommandContext::new(room, ctx).await;
        send_welcome_message(cmd_ctx).await;

        // let _ = room.send(RoomMessageEventContent::text_plain("/remind")).await.unwrap();
    });
}

// ===== Special Messages =====
/// Send welcome message with help to the room.
async fn send_welcome_message(cmd_ctx: CommandContext) {
    let tomorrow = Utc::now().with_timezone(&cmd_ctx.settings.room_tz).date_naive() + Days::new(1);
    let (month_str, month_str_truncated) = &cmd_ctx.i18n.format_month(&tomorrow.month()).unwrap();

    let welcome_type = if cmd_ctx.ctx.bot_config.quick_remind {
        "welcome.on_command_off"
    } else { "welcome.on_command" };

    let welcome_msg = t!(
        welcome_type,
        cmd_local = &cmd_ctx.i18n.cmd_remind,
        cmd_list = &cmd_ctx.ctx.bot_config.remind_commands.join("|"),
        cmd_tz_list = &cmd_ctx.ctx.bot_config.tz_commands.join("|"),
        date = tomorrow.format("%d.%m.%Y").to_string(),
        date_slash = tomorrow.format("%d/%m/%Y").to_string(),
        date_hyphen = tomorrow.format("%d-%m").to_string(),
        date_d = tomorrow.format("%d").to_string(),
        month = month_str,
        month_truncated = month_str_truncated,
        today = &cmd_ctx.i18n.today,
        tomorrow = &cmd_ctx.i18n.tomorrow,
        morning = &cmd_ctx.i18n.morning,
        afternoon = &cmd_ctx.i18n.afternoon,
        evening = &cmd_ctx.i18n.evening
    );
    // for markdonw to text_html: use pulldown_cmark::{Parser, html};
    // let welcome_msg_html = markdown_to_html(&welcome_msg).await;

    let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(welcome_msg)).await.unwrap();
}

/// Send reaction to the related event (message).
async fn send_reaction(
    event_id: OwnedEventId, 
    cmd_ctx: &CommandContext, 
    emoji_key: MessageReaction
) {
    let annotation = Annotation::new(event_id, emoji_key.to_string());
    let content = ReactionEventContent::new(annotation);
    
    let _ = cmd_ctx.room.send(content).await;
}

/// Send reactions with digits emoji with to the related event (message).
/// Calculates the time until an event occurs and selects the number of the 
/// largest non-empty dimension (days -> hours -> minutes).
async fn send_digits_reaction(
    event_id: OwnedEventId, 
    cmd_ctx: &CommandContext,
    numbers: Vec<i64>
) {
    // First positive number, whose remainder when divided by 11 is not 0.
    // (Matrix prevents sending the same reaction twice: status_code: 400, DuplicateAnnotation.)
    let first_positive = numbers
        .iter()
        .find(|&&x| x > 0)
        .and_then(|&x| if x % 11 == 0 { None } else { Some(x) });

    match first_positive {
        Some(mut d) => {
            let _ = send_reaction(event_id.clone(), &cmd_ctx, MessageReaction::Clock).await;

            if d % 11 == 0 {

            }

            let mut digits = Vec::new();
         
            while d > 0 {
                digits.push((d % 10) as u32);
                d /= 10;
            }
            // we don't need reverse vector as Matrix clients
            // display new reactions at the left of message bubble.
            // digits.reverse();

            for digit_emoji in digits {
                let _ = send_reaction(event_id.clone(), &cmd_ctx, MessageReaction::from_digit(digit_emoji)).await;
            }

        },
        // so we can't send numbers with equal digits and send "check" emoji instead
        None => {
            let _ = send_reaction(event_id.clone(), &cmd_ctx, MessageReaction::Timer).await;
        }
    }

    /*
    for &mut d in numbers {
        if d > 0 {
            // Matrix prevents sending the same reaction twice
            // (status_code: 400, DuplicateAnnotation, message: "Can't send same reaction twice"),
            // so we can't send numbers with equal digits and send "check" emoji instead
            if d % 11 == 0 {
                let _ = send_reaction(event_id.clone(), &cmd_ctx, MessageReaction::Timer).await;
                break;
            }

            let mut digits = Vec::new();
         
            while d > 0 {
                digits.push((d % 10) as u32);
                d /= 10;
            }
            // we don't need reverse vector as Matrix clients
            // display new reactions at the left of message bubble.
            // digits.reverse();

            for digit_emoji in digits {
                let _ = send_reaction(event_id.clone(), &cmd_ctx, MessageReaction::from_digit(digit_emoji)).await;
            }

            let _ = send_reaction(event.event_id.clone(), &cmd_ctx, MessageReaction::Clock).await;
            
            break;
        };
    };
    */
}

// ===== Parsers and Constructions Methods for Reminders =====
/// Build regular expression
fn build_reminder_regex(
    i18n: &Arc<I18nManager>,
) -> &'static Regex {
    REMINDER_REGEX.get_or_init(|| {
        let mut regex_str = String::with_capacity(256); 
        
        // [^\.\-\s]{1,15} in ?P<month> can be replaced with white list of months names
        regex_str.push_str(r"^(?i)(?:(?P<datetime>(?P<day>\d{1,2})(?:\s|\.|\/|-)(?P<month>[^\.\-\s]{1,15}|\d{2})(?:\s|\.|\/|-)?(?P<year>\d{4})?)|(?P<day_natural>");
        regex_str.push_str(&i18n.days.join("|"));

        regex_str.push_str(r"))(?:(?:\s+(?<prep>at|");
        regex_str.push_str(&i18n.prepositions.join("|"));
        regex_str.push_str(r"))?\s+((?P<hour>\d{2}):(?P<min>\d{2})|(?P<time_natural>");
        regex_str.push_str(&i18n.times.join("|"));
        regex_str.push_str(r")))?+\s+(?P<text>.+)$");
        //regex_str.push_str(r")|(?P<time_interval>(?<gap>\d{1,2})\s(?<step>minutes|hours)) ))?\s+(?P<text>.+)$");

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
        return None;
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
        println!("natural time is {:?}", t_nat);
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

    Some(ParsedReminder { text, year, month, day, hour, min })
}

/// Build final UTC DateTime for DB and validate its time in the future.
fn build_datetime_utc(
    data: &ParsedReminder, 
    cmd_ctx: &CommandContext
) -> Result<(DateTime<Utc>, NaiveDateTime), ReminderDateError> {
    // Parse to get month number
    let month = if let Some(m) = cmd_ctx.i18n.parse_month(data.month.as_str()) {
        m.to_string()
    } else {
        return Err(ReminderDateError::InvalidMonth);
    };

    let datetime_string = format!("{}-{}-{} {}:{}:00", data.year, month.as_str(), data.day, data.hour, data.min);
    
    // Check if time can be parsed and in the future
    let naive_dt = NaiveDateTime::parse_from_str(&datetime_string, "%Y-%m-%d %H:%M:%S")
        .map_err(|_| ReminderDateError::InvalidTime)?;

    // We convert it to DateTime and check that zone mapping has a single result.
    let user_dt = naive_to_datetime(naive_dt.clone(), &cmd_ctx.settings.room_tz);
    let utc_dt = user_dt.with_timezone(&Utc);

    // Checking that the time is in the future
    if utc_dt <= Utc::now() {
        return Err(ReminderDateError::TimeInPast);
    }

    Ok((utc_dt, naive_dt))
}

/*
/// Parse markdown i18n str to html String for matrix format
async fn markdown_to_html(markdown: &str) -> String {
    let parser = Parser::new(&markdown);
    let mut buffer = String::new();
    html::push_html(&mut buffer, parser);
    buffer
}
*/

/*
async fn naive_date_with_tz(tz: &Tz) -> NaiveDateTime {
    return Utc::now().with_timezone(tz).date_naive();
    //return now_in_tz.date_naive();
}
*/
// ===== Service ======
/// Clean message from makrdown link with user mention
// or can be implemented with input.strip_prefix()
fn clean_from_mention(text: &str) -> String {
    let re = MENTION_REGEX.get_or_init(|| {
        Regex::new(r"\[@[^\]]+\]\(https://matrix\.to/#/[^)]+\)").unwrap()
    });

    re.replace(text, "").trim().to_string()
}
/// Calculate weeks, days, hours and minutes before some time
fn calculate_durations(utc_time: &DateTime<Utc>) -> Vec<i64> {
    let duration_to_wait = utc_time.signed_duration_since(Utc::now());
    let numbers = vec![
        duration_to_wait.num_weeks(), 
        duration_to_wait.num_days(), 
        duration_to_wait.num_hours(),
        duration_to_wait.num_minutes()
    ];
    return numbers;
}

/// Convert NaiveDateTime to chrono-tz Datetime<Tz> with checking for a change of season.
fn naive_to_datetime(dt_naive: NaiveDateTime, tz: &Tz) -> DateTime<Tz> {
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
