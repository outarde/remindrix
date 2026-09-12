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
    Zoned, Span, tz::TimeZone, 
    civil::{DateTime as CivilDateTime, Date}
};
use tokio_rusqlite::Connection;
use regex::Regex;
use std::{string::ToString, sync::{OnceLock, Arc}};
use rust_i18n::t;
use strum_macros::{Display, EnumString};
use clap::Parser;

// app crates
use crate::config::BotConfig;
use crate::db::ReminderRepository;
use crate::reminder::{ReminderData, ReminderStatus, ReminderError};
use crate::settings::{RoomTimezoneContent, SettingsManager};
use crate::parsers::{
    ParsedDate, ParsedTime,
    resolve_date, resolve_date_interval, resolve_time, resolve_time_interval, resolve_target_dt,
    parse_room
};
use crate::natural::{
    process_natural_reminder
};
use crate::reactions::{
    MessageReaction, 
    send_welcome_message, 
    calculate_durations, send_reaction, send_digits_reaction
};

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

/// Parse i18n, like months names
#[derive(Debug)]
pub struct I18nManager {
    pub cmd_remind: String,

    pub months: Vec<String>, 
    pub today: String,
    pub tomorrow: String,
    pub morning: String,
    pub afternoon: String,
    pub evening: String,

    pub days: Vec<String>,
    pub times: Vec<String>,
    pub prepositions: Vec<String>,
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
    pub fn parse_month(&self, input: &str) -> Option<i8> {
        // If number
        if let Ok(m) = input.parse::<i8>() {
            if (1..=12).contains(&m) {
                return Some(m);
            }
        }

        // If name
        let idx = self.months.iter()
            .position(|name| {
                (name == input || name.starts_with(input)) && input.len() >= 3
            })?;

        Some((idx + 1) as i8)
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
    pub user_id: OwnedUserId,
    // pub room_id: OwnedRoomId,
    pub ctx: Arc<super::BotContext>,
    pub settings: SettingsManager,
    pub i18n: Arc<I18nManager>,
}

impl CommandContext {
    pub async fn new(user_id: OwnedUserId, room: Room, ctx: Arc<super::BotContext>) -> Self {
        let settings = SettingsManager::new(&room, Some(user_id.clone()), &ctx).await;
        let i18n = ctx.get_i18n_manager(&settings.room_lang).await;
        // let room_id = room.room_id().to_owned();

        Self { room, user_id, ctx, settings, i18n } 
    }

    // getters
    // cmd_ctx.ctx.bot_config
    pub fn bot_config(&self) -> &super::config::BotConfig {
        &self.ctx.bot_config
    }
    pub fn reminders(&self) -> Arc<ReminderRepository> {
        self.ctx.reminders.clone()
    }
    // Check if room has more than 2 active (joined and invitees) members
    pub fn is_room_group(&self) -> bool {
        self.room.active_members_count() > 2 as u64
    }
}

/// CLI commands for new reminders.
#[derive(Parser, Debug)]
// #[command(no_binary_name = true)]
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
    pub time: Option<String>,
    #[arg(long)]
    pub hour: Option<String>,
    #[arg(long)]
    pub min: Option<String>,
    
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

/// Erros in the CLi processing.
#[derive(Debug, Display)]
pub enum CliError {
    ClapError(clap::Error),
    NaturalFallback,
    Reminder(ReminderError),
}
impl From<clap::Error> for CliError {
    fn from(err: clap::Error) -> Self {
        CliError::ClapError(err)
    }
}
impl From<ReminderError> for CliError {
    fn from(err: ReminderError) -> Self {
        CliError::Reminder(err)
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
    let cmd_ctx = CommandContext::new(event.sender.clone(), room.clone(), ctx.clone()).await;

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

    // Parse a command
    let Some((command, args)) = BotCommand::parse(&body, &cmd_ctx) else { 
        return;
    };

    // Call the command
    let result = match command {
        BotCommand::Remind => {
            handle_remind(&args, event.clone(), cmd_ctx.clone()).await
        }
        BotCommand::List => {
            Ok(())
        }
        BotCommand::Tz => {
            handle_tz(&args, event.clone(), cmd_ctx.clone()).await
        }
    };

    // If there is an error
    if let Err(err) = result {
        // super::reactions::send_error(err, &cmd_ctx);
        let err_msg = t!(err.to_string()); 
        let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(err_msg)).await;
    }
}

// ===== Handlers =====
/// Handle new reminder router.
pub async fn handle_remind(
    args_str: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> anyhow::Result<()> {

    let args: Vec<&str> = args_str.split_whitespace().collect();
    // clap requires some command at the first place
    let mut clap_input = vec!["remind"];
    clap_input.extend(&args);

    // Try to parse in CLI mode at first.
    // try_parse_from() from clap only throws an error when there is a parsing error, 
    // not when there are empty values, so we put it in another function.
    match process_cli_reminder(clap_input, event.clone(), cmd_ctx.clone()).await {
        Ok(_) => Ok(()),
        Err(CliError::ClapError(clap_err)) => {
            // If user wants to print --help
            if clap_err.kind() == clap::error::ErrorKind::DisplayHelp {
                let help_text = clap_err.render().to_string();
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(help_text)).await;
                Ok(())
            }
            else { process_natural_reminder(args_str, event.clone(), cmd_ctx.clone()).await }
        },
        Err(CliError::NaturalFallback) => {
            process_natural_reminder(args_str, event.clone(), cmd_ctx.clone()).await
        },
        Err(CliError::Reminder(err)) => {
            Err(err.into())
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
        return Err(CliError::Reminder(ReminderError::EmptyText));
    }
    let text = args.text.join(" ").to_string();

    // Settings for the room for which the reminder was delegated or intended.
    let target_settings = if let Some(to) = args.to.as_deref() {
        let room = parse_room(&to, &cmd_ctx).await?;
        SettingsManager::new(&room, None, &cmd_ctx.ctx).await
    } else { cmd_ctx.settings.clone() };

    // Set room_tz from settings.
    let room_tz = target_settings.room_tz.clone();

    // Parse date.
    let date: ParsedDate = if args.interval {
        resolve_date_interval(&args, &room_tz)?
    } else {
        resolve_date(&args, &room_tz)?
    };

    // Parse time.
    let time: ParsedTime = if args.interval {
        resolve_time_interval(&args, &room_tz)?
    } else {
        resolve_time(&args)?
    };

    // Get times.
    let (utc_dt, civil_dt) = resolve_target_dt(date, time, &room_tz)?;

    // Fill a structure.
    let reminder_data = ReminderData {
        utc_dt,
        civil_dt,
        text,
        created_by: cmd_ctx.user_id.clone(),
        settings: target_settings.into()
    };

    // Saving.
    let reminder = cmd_ctx.reminders().save_reminder(reminder_data).await?;
    tracing::info!("Reminder {} saved", reminder.id);
    // let reminder = reminder_data.save(cmd_ctx.ctx.db.clone()).await?;
    // let reminder = super::reminder::save_reminder_data(cmd_ctx.ctx.db.clone(), reminder_data.clone()).await?;

    // Scheduling.
    super::reminder::schedule_reminder(cmd_ctx.ctx.clone(), reminder.clone()).await;
        
    // Send success reaction or message to the room.
    super::reactions::send_success(event, &cmd_ctx, reminder.data, args.interval).await;

    Ok(())
}

/// Handle changing time zone.
async fn handle_tz(
    body: &str,
    ev: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> anyhow::Result<()> {
    // Update timezone if we have one in the input.
    if !body.is_empty() {
        // Parse user's input timezone code
        let input_tz = super::settings::parse_tz(&body)?;

        // If user's input timezone is equal to current room timezone
        if input_tz == cmd_ctx.settings.room_tz {
            let msg = t!("tz.not-set"); 
            let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(msg)).await;
        }
        else {
            let input_tz_name = cmd_ctx.settings.set_room_tz(&cmd_ctx, input_tz).await?;

            if cmd_ctx.bot_config().send_reactions {
                let _ = send_reaction(ev.event_id.clone(), &cmd_ctx, MessageReaction::Check).await;
            } else {
                let msg = t!("tz.set", tz = input_tz_name); 
                let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(msg)).await;
            }
        }
    }
    // Send current timezone.
    else {
        let msg = t!("tz.current", tz = &cmd_ctx.settings.room_tz_name);
        let _ = cmd_ctx.room.send(RoomMessageEventContent::text_markdown(msg)).await;
    }

    Ok(())
}

/// Auto-join.
pub async fn on_stripped_state_member(
    room_member: StrippedRoomMemberEvent,
    room: Room,
    ctx: Arc<super::BotContext>,
) {
    if room_member.state_key != ctx.bot_id {
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
        let cmd_ctx = CommandContext::new(room_member.sender, room, ctx).await;
        let _ = send_welcome_message(cmd_ctx).await;

        // let _ = room.send(RoomMessageEventContent::text_plain("/remind")).await.unwrap();
    });
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
