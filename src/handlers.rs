use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Client, Room, RoomState,
    ruma::{
        room_id,
        OwnedUserId, RoomId, OwnedRoomId, OwnedEventId,
        events::{
            reaction::{ReactionEventContent, OriginalSyncReactionEvent}, relation::Annotation,
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
use std::{string::ToString, sync::{OnceLock, Arc}, borrow::Cow};
use rust_i18n::t;
use strum_macros::{Display, EnumString};
use clap::{Parser, Subcommand};

// app crates
use crate::config::BotConfig;
use crate::context::{CommandContext, I18nManager};
use crate::db::ReminderRepository;
use crate::reminder::{ReminderData, ReminderStatus, ReminderError};
use crate::settings::{RoomTimezoneContent, SettingsManager, SettingError};
use crate::parsers::{
    ParsedDate, ParsedTime,
    resolve_date, resolve_date_interval, resolve_time, resolve_time_interval, resolve_target_dt,
    parse_room
};
use crate::natural::{
    process_natural_reminder
};
use crate::messaging::{
    RoomMessenger, MessageReaction,
};

static MENTION_REGEX: OnceLock<Regex> = OnceLock::new();

/// Abstract layer for the error messages.
#[derive(Debug)]
pub enum OutputError {
    Reminder(ReminderError),
    Setting(SettingError),
}

impl From<ReminderError> for OutputError {
    fn from(err: ReminderError) -> Self {
        OutputError::Reminder(err)
    }
}

impl From<SettingError> for OutputError {
    fn from(err: SettingError) -> Self {
        OutputError::Setting(err)
    }
}

impl OutputError {
    /// In some cases, to obtain the localized error text, 
    /// we need to pass its value, not just its name as a key by thiserror.
    pub fn to_local(&self, locale: &str) -> String {
        match self {
            /*
            OutputError::Reminder(error) => {
                error.to_local(locale)
            },
            */
            /*
            OutputError::Reminder(error) => {
                match error
            },
            */
            OutputError::Reminder(error) => {
                error.to_local(locale)
            },
            OutputError::Setting(error) => {
                let msg = t!(error.to_string(), locale = locale);
                msg.to_string()
            }
        }
    }
}

/// List of commands for the bot in a chat.
enum BotCommand {
    Remind,
    List,
    Tz,
    Settings,
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

        // Clean &str from \n and remove suffix if it was found. 
        let remaining: String = text.replace('\n', " ").chars().skip(skip_num).collect();
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
        } else if cmd_ctx.bot_config().settings_commands.contains(&cmd_str) {
            Some((BotCommand::Settings, args))
        } else {
            // Return "remind" command for fast only-remind creations only in private rooms.
            if cmd_ctx.bot_config().quick_remind && !cmd_ctx.is_room_group() {
                Some((BotCommand::Remind, text.to_string()))
            } else { return None; }
        }
    }
}

/// **CLI** (Command Lined Interface) or **Pro mode** allows you to create reminders 
/// using syntax similar to that used in the terminal
#[derive(Parser, Debug)]
// #[command(no_binary_name = true)]
pub struct RemindArgs {
    /// **Reminder date** as numbers, without spaces. 
    /// Supported characters as separators: `.`, `/`, `-`. A day without a month or year can be specified
    #[arg(long)]
    pub date: Option<String>,
    /// **Day** as a number
    #[arg(short, long)]
    pub day: Option<String>,
    /// **Month** as a number
    #[arg(short, long)]
    pub month: Option<String>,
    /// **Year** as a number
    #[arg(short, long)]
    pub year: Option<String>,

    /// **Time** as a number, without spaces. The colon character `:` is supported as a separator
    #[arg(short, long)]
    pub time: Option<String>,
    /// **Hour** as a number
    #[arg(long)]
    pub hour: Option<String>,
    /// **Minutes** as a number
    #[arg(long)]
    pub min: Option<String>,
    
    /// Reminder **text**
    pub text: Vec<String>,

    /// The room to **delegate** the reminder to, in the `!unique_room_code:homeserver_url` format. 
    /// You can get it from the share menu in the Element X client
    #[arg(long)]
    pub to: Option<String>,
    /// Are the time and date an **interval**
    #[arg(short, long)]
    pub interval: bool,

    // CRON's style
    // #[arg(short, long)]
    // pub repeat: Option<String>,
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
            // self.repeat.as_ref(),
        ];

        options.iter().find(|opt| opt.is_some()).is_some()
    }
}

#[derive(Parser, Debug)]
enum SettingsArgs {
    Lang { 
        // #[arg(short, long)]
        lang_key: Option<String>,
    },
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
        BotCommand::Settings => {
            handle_settings(&args, event.clone(), cmd_ctx.clone()).await
        }
    };

    // If there is an error
    if let Err(err) = result {
        // or use unwrap_err() and call to_local()
        let msg = err.to_local(&cmd_ctx.settings.room_lang);
        cmd_ctx.msng.text_md(&msg).await;
    }
}

/// React to reaction.
pub async fn on_reaction(
    event: OriginalSyncReactionEvent, 
    room: Room, 
    ctx: Arc<super::BotContext>
) {
    if room.state() != RoomState::Joined { return; }

    let sender = &event.sender;
    let reaction_content = &event.content;
    
    let target_event_id = &reaction_content.relates_to.event_id;
    
    let emoji = &reaction_content.relates_to.key;

    println!(
        "User {} set reaction {} to event with id {} in room {}",
        sender,
        emoji,
        target_event_id,
        room.room_id()
    );
}

// ===== Handlers =====
/// Handle new reminder router.
pub async fn handle_remind(
    args_str: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> Result<(), OutputError> {

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
            // If user wants to print --help.
            if clap_err.kind() == clap::error::ErrorKind::DisplayHelp {
                let help_text = clap_err.render().to_string();
                cmd_ctx.msng.text_md_long(&help_text).await;
                Ok(())
            }
            else { 
                process_natural_reminder(args_str, event.clone(), cmd_ctx.clone()).await.map_err(|e| e.into())
            }
        },
        Err(CliError::NaturalFallback) => {
            process_natural_reminder(args_str, event.clone(), cmd_ctx.clone()).await.map_err(|e| e.into())
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
        resolve_time(&args, &room_tz, target_settings.default_time.clone())?
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
    tracing::info!("Reminder #{} saved", reminder.id);

    // Scheduling.
    super::reminder::schedule_reminder(cmd_ctx.ctx.clone(), reminder.clone()).await;
        
    // Send success reaction or message to the room.
    cmd_ctx.send_reminder_success(event, reminder.data, args.interval).await;

    Ok(())
}

/// Handle changing time zone.
async fn handle_tz(
    body: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> Result<(), OutputError> {
    // Update timezone if we have one in the input.
    if !body.is_empty() {
        // Parse user's input timezone code
        let input_tz = super::settings::parse_tz(&body)?;

        // If user's input timezone is equal to current room timezone
        if input_tz == cmd_ctx.settings.room_tz {
            return Err(ReminderError::TzNotSet.into());
        }
        else {
            let input_tz_name = cmd_ctx.settings.set_room_tz(&cmd_ctx, input_tz).await?;
            cmd_ctx.send_tz_success(event.clone(), &input_tz_name).await;
        }
    }
    // Send current timezone.
    else {
        let msg = t!("tz.current", locale = &cmd_ctx.settings.room_lang, tz = &cmd_ctx.settings.room_tz_name);
        cmd_ctx.msng.text_md(&msg).await;
    }

    Ok(())
}

/// Room (optional, user) Settings.
pub async fn handle_settings(
    args_str: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> Result<(), OutputError> {
    let args_vec: Vec<&str> = args_str.split_whitespace().collect();
    let mut clap_input = vec!["settings"];
    clap_input.extend(&args_vec);
    let args = SettingsArgs::try_parse_from(clap_input).map_err(|_| SettingError::NoCommand)?;

    let result = match args {
        SettingsArgs::Lang {lang_key} => handle_lang_settings(lang_key, event.clone(), cmd_ctx.clone()).await?,
        /*
        Err(_) => {
            let msg = t!("settings.help", locale = &cmd_ctx.settings.room_lang);
            cmd_ctx.msng.text_md_long(&msg).await;
            return Ok(());
        },
        */
        _ => (),
    };

    Ok(result)
}

/// Handle language settings.
pub async fn handle_lang_settings(
    lang_key: Option<String>,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> Result<(), SettingError> {
    // Send list of languages.
    let lang = match lang_key {
        Some(l) => l,
        None => {
            // Send a list of available languages.
            let msg = t!(
                "settings.lang-list", 
                locale = &cmd_ctx.settings.room_lang, 
                cmd = cmd_ctx.bot_config().settings_commands.join("|")
            );
            cmd_ctx.msng.text_md_long(&msg).await;
            return Ok(());
        }
    };

    // Check if the language is supported.
    let languages = rust_i18n::available_locales!();
    if !languages.contains(&Cow::from(lang.as_str())) { return Err(SettingError::NoLanguage) };

    // Check if language is not a current one.
    if &cmd_ctx.settings.room_lang == &lang { return Err(SettingError::LanguageNotSet) };

    // Set new language and send message if it was successful.
    cmd_ctx.settings.set_language_universal(&cmd_ctx, &lang).await?;
    let msg = t!("settings.lang-set", locale = lang);
    cmd_ctx.msng.text_md(&msg).await;

    Ok(())
}

// ===== Auto-join =====
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
        cmd_ctx.send_welcome_message().await;
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
