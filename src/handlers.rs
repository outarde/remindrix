use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Client, Room, RoomState,
    ruma::{
        RoomId, OwnedRoomId,
        events::room::{
            member::StrippedRoomMemberEvent, 
            message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
        }
    }
};
//use ruma::events::room::{SyncOrStrippedState};
use tokio::time::{Duration, sleep};
use chrono::{Days, NaiveDateTime, DateTime, Utc, TimeZone, Datelike, LocalResult};
use chrono_tz::Tz;
use tokio_rusqlite::Connection;

use regex::Regex;
use std::sync::{OnceLock, Arc};
use rust_i18n::t;

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
    fn parse(text: &str, bot_config: BotConfig) -> Option<(Self, String)> {
        if !text.starts_with('/') && !text.starts_with('!') {
            // Return None if bot can be activated only with command
            // and return "remind" command if can be activated without command
            if bot_config.on_command {
                return None;
            } else {
                return Some((BotCommand::Remind, text.to_string()))
            }
        }
        
        let remaining: String = text.chars().skip(1).collect();
        let mut parts = remaining.splitn(2, ' ');

        // command and args from text
        let cmd_str = parts.next()?.to_lowercase();
        let args = parts.next().unwrap_or("").to_string();

        // i18n aliases for commands
        let i18n_command = t!("reminder.command");

        if bot_config.remind_commands.contains(&cmd_str) || &i18n_command == &cmd_str {
            Some((BotCommand::Remind, args))
        } else if bot_config.list_commands.contains(&cmd_str) {
            Some((BotCommand::List, args))
        } else if bot_config.tz_commands.contains(&cmd_str) {
            Some((BotCommand::Tz, args))
        } else {
            None
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

/// Errors for build_datetime_str
#[derive(Debug)]
enum ReminderDateError {
    InvalidMonth,
    TimeInPast,
    InvalidTime,
    SummerTime,
}

/// i18n keys
impl ReminderDateError {
    fn as_i18n_key(&self) -> &'static str {
        match self {
            ReminderDateError::InvalidMonth => "reminder.error.month",
            ReminderDateError::TimeInPast => "reminder.error.past-time",
            ReminderDateError::InvalidTime => "reminder.error.time",
            ReminderDateError::SummerTime => "reminder.error.summer-time",
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

    // getter
    pub fn bot_config(&self) -> &super::config::BotConfig {
        &self.ctx.bot_config
    }
}

/// Parsed data of user message for newly reminder
#[derive(Debug)]
struct ParsedReminder {
    text: String,
    year: String,
    month: String,
    day: String,
    hour: String,
    min: String,
}

/// Reply to incoming message
pub async fn on_room_message(
    event: OriginalSyncRoomMessageEvent, 
    room: Room, 
    ctx: Arc<super::BotContext>
) {
    // check if we joined the room
    if room.state() != RoomState::Joined { return; }
    // check if message is not from bot account
    // can be changed if multi-step interaction with bot will be presented
    if event.sender == ctx.bot_id { return; }

    // check for text type of message
    let MessageType::Text(text_content) = &event.content.msgtype else { return };
    let mut body = text_content.body.trim().to_string();

    // if ctx.bot_config.on_mention is true, 
    // check for if bot was mentioned in public rooms (where more than 2 members joined or invited)
    if ctx.bot_config.on_mention && room.active_members_count() > 2 {
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
    let Some((command, args)) = BotCommand::parse(&body, ctx.bot_config.clone()) else { 
        return;
    };

    // Command Context
    let cmd_ctx = CommandContext::new(room.clone(), ctx.clone()).await;

    match command {
        BotCommand::Remind => {
            handle_remind(&args, cmd_ctx).await;
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

/// Change TZ
pub async fn handle_tz(
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

            let msg = t!("tz.set", tz = input_tz.to_string()); 
            let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(msg)).await;
        }
    }
    // Send current timezone.
    else {
        let msg = t!("tz.current", tz = &cmd_ctx.settings.room_tz.to_string());
        let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(msg)).await;
    }
}

/// New reminder.
pub async fn handle_remind(
    body: &str, 
    cmd_ctx: CommandContext,
) {
    // i18n and timezone
    // let i18n = ctx.get_i18n_manager(&ctx.bot_config.lang).await;
    // let room_tz = super::settings::get_room_tz(room.clone(), &ctx).await;

    // let cmd_ctx = CommandContext::new(&room, ctx).await;

    // Make regular expression
    let re = build_reminder_regex(&cmd_ctx.i18n);

    // If regular expression found some groups
    if let Some(caps) = re.captures(body) {

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
        let (utc_time, naive_time) = match build_datetime_utc(&reminder_data, &cmd_ctx) {
            Ok((ut, nt)) => (ut, nt),
            Err(err) => {
                let err_msg = t!(err.as_i18n_key()); 
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(err_msg)).await;
                
                tracing::error!("Date and time validation error: {:?} for {:?}", err, reminder_data);
                return;
            }
        };

        // Save to DB and schedule if ok.
        match super::reminder::save_reminder_to_db_utc(
            &cmd_ctx, 
            reminder_data.text, 
            naive_time.clone(), 
            utc_time.clone()
        ).await {
            Ok(new_reminder) => {
                super::reminder::schedule_reminder_utc(cmd_ctx.ctx.clone(), new_reminder).await;

                let date_str = naive_time.format("%d.%m.%Y");
                let reminder_mes = t!("reminder.saved", date = date_str, hour = reminder_data.hour, min = reminder_data.min);
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(reminder_mes)).await;
            }
            Err(err) => {
                tracing::error!("SQLite error: {:?}", err);
            }
        }
    } 
    // Welcome message
    else {
        send_welcome_message(cmd_ctx).await;
    }
}

/// Send welcome message with help to the room
async fn send_welcome_message(cmd_ctx: CommandContext) {
    let tomorrow = Utc::now().with_timezone(&cmd_ctx.settings.room_tz).date_naive() + Days::new(1);
    let (month_str, month_str_truncated) = &cmd_ctx.i18n.format_month(&tomorrow.month()).unwrap();

    let welcome_type = if cmd_ctx.ctx.bot_config.on_command {
        "welcome.on_command"
    } else { "welcome.on_command_off" };

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

/// Auto-join
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
        regex_str.push_str(r"))?\s+(((?P<hour>\d{2}):(?P<min>\d{2}))|(?P<time_natural>");
        regex_str.push_str(&i18n.times.join("|"));
        regex_str.push_str(r")))?\s+(?P<text>.+)$");
        //regex_str.push_str(r")|(?P<time_interval>(?<gap>\d{1,2})\s(?<step>минут|часов)) ))?\s+(?P<text>.+)$");

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
        (super::config::DEFAULT_MORNING_TIME.to_string(), "00".to_string())
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
    let user_dt = match cmd_ctx.settings.room_tz.from_local_datetime(&naive_dt) {
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(_dt1, dt2) => {
            // To Winter Time
            dt2 
        }
        LocalResult::None => {
            // To Summer Time
            return Err(ReminderDateError::SummerTime); 
        }
    };
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

/// Clean message from makrdown link with user mention
// or can be implemented with input.strip_prefix()
fn clean_from_mention(text: &str) -> String {
    let re = MENTION_REGEX.get_or_init(|| {
        Regex::new(r"\[@[^\]]+\]\(https://matrix\.to/#/[^)]+\)").unwrap()
    });

    re.replace(text, "").trim().to_string()
}
