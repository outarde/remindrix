use matrix_sdk::{
    deserialized_responses::{TimelineEvent, TimelineEventKind},
    Room, RoomState,
    room::reply::{Reply, EnforceThread},
    ruma::{
        room_version_rules::RedactionRules,
        serde::Raw,
        UserId, OwnedEventId, EventId,
        events::{
            OriginalSyncMessageLikeEvent, AnyTimelineEvent, AnySyncTimelineEvent, AnyMessageLikeEvent,
            AnySyncMessageLikeEvent,
            reaction::{OriginalSyncReactionEvent, SyncReactionEvent, ReactionEvent, ReactionEventContent},
            room::{
                member::StrippedRoomMemberEvent, 
                message::{MessageType, OriginalSyncRoomMessageEvent, ReplyWithinThread, AddMentions},
                redaction::SyncRoomRedactionEvent
            },
            relation::RelationType,
        }
    }
};
use anyhow::Result;
use tokio::time::{Duration, sleep};
use jiff::{civil::time, ToSpan};
use regex::Regex;
use std::{string::ToString, str::FromStr, sync::{OnceLock, Arc}, borrow::Cow};
use rust_i18n::t;
use clap::Parser;

// app crates
use crate::context::CommandContext;
use crate::reminder::{ReminderData, DelegationType, ReminderError};
use crate::list::{ListScopes, ListOrder, StateKey, PageState};
use crate::settings_service::{SettingsService};
use crate::settings::{
    SettingError, 
    SettingUpdate, SettingKey
};
use crate::parsers::{
    ParsedDate, ParsedTime,
    resolve_date, resolve_date_interval, resolve_time, resolve_time_interval, resolve_target_dt,
    parse_room
};
use crate::natural::{
    process_natural_reminder
};
use crate::messaging::ReactionButton;

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
                error.to_local(locale)
            },
        }
    }
}

/// List of commands for the bot in a chat.
enum BotCommand {
    Remind,
    List,
    Settings,
    Beep,
}

/// List of reaction-buttons commands.
enum BotReactionsCommand {
    NextListPage,
    PrevListPage,
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
        } else if cmd_ctx.bot_config().settings_commands.contains(&cmd_str) {
            Some((BotCommand::Settings, args))
        } else if &cmd_str == "beep" || &cmd_str == "egg" {
            Some((BotCommand::Beep, args))
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

    /// The **room to delegate** the reminder to, in the `!unique_room_code:homeserver_url` format. 
    /// You can get it from the share menu in the Element X client
    #[arg(long)]
    pub room: Option<String>,
    /// The **user to delegate** the reminder to, in the `@user_name:homeserver_url` format.
    /// You can even mention yourself so that a mention notification appears in the group chat
    #[arg(long)]
    pub user: Option<String>,
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
            self.room.as_ref(),
            // self.repeat.as_ref(),
        ];

        options.iter().find(|opt| opt.is_some()).is_some()
    }
}

/// List of reminders.
#[derive(Parser, Debug)]
pub struct ListArgs {
    pub scope: String,
    pub page: Option<i32>,
    pub size: Option<i32>,
    pub order: Option<String>,
}

/// Bot settings.
#[derive(Parser, Debug)]
enum SettingsArgs {
    /// **Time zone** for the room.
    Tz { 
        tz: Option<String>,
    },
    /// **Language** for the room for commands recognition and responses.
    Lang { 
        lang_key: Option<String>,
    },
    /// **Default times values** recognized in all rooms on a per-user basis.
    Time {
        #[arg(long)]
        default: Option<String>,
        #[arg(long)]
        morning: Option<String>,
        #[arg(long)]
        afternoon: Option<String>,
        #[arg(long)]
        evening: Option<String>,
    },
}

/// Erros in the CLi processing.
#[derive(Debug)]
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
            handle_list(&args, event.clone(), cmd_ctx.clone()).await
        }
        BotCommand::Settings => {
            handle_settings(&args, event.clone(), cmd_ctx.clone()).await
        }
        BotCommand::Beep => {
            cmd_ctx.msng.text_md("🪺").await;
            Ok(())
        }
    };

    // If there is an error
    if let Err(err) = result {
        // or use unwrap_err() and call to_local()
        let msg = err.to_local(&cmd_ctx.settings.room.room_lang);
        cmd_ctx.msng.text_md(&msg).await;
    }
}

/*
/// Handler for message or reaction redaction.
pub async fn on_redaction(
    event: SyncRoomRedactionEvent, 
    room: Room, 
    ctx: Arc<super::BotContext>
) {
    if room.state() != RoomState::Joined { return; }

    // Get the EventId that event redacts
    if let Some(redacted_event_id) = event.redacts(&RedactionRules::V11) {
        println!("EVENT 1 {:?}", redacted_event_id);

        let filter = vec![RelationType::Annotation, RelationType::Reference];
        let super_event = room.load_or_fetch_event_with_relations(redacted_event_id, Some(filter), None).await;
        println!("EVENT 2 {:?}", super_event);

        // { event: msg_event_kind }
        if let Ok(timeline_event) = room.event(redacted_event_id, None).await {
            let raw_event = timeline_event.kind.into_raw();
            // let TimelineEventKind::Decrypted(decrypted_event) = timeline_event.kind else { return };
            


            // let AnySyncTimelineEvent::MessageLike(msg) = msg_event.kind;
            // let TimelineEventKind::PlainText { event: ev } = msg_event.kind else { return };




            // if let Ok(AnyTimelineEvent::MessageLike(msg_event)) = decrypted_event.event.deserialize() {
            if let Ok(AnySyncTimelineEvent::MessageLike(msg_event)) = raw_event.deserialize() {
                let AnySyncMessageLikeEvent::Reaction(reaction_event) = msg_event else {return};
                // let SyncReactionEvent::Redacted(redacted_event) = reaction_event else {return};
                // let RedactedMessageLikeEvent::Redacted(redacted_event) = reaction_event else {return};
                // println!("EVENT {:?}", reaction_event.into_full_event(room.room_id().to_owned()));
                println!("EVENT {:?}", reaction_event.as_original());
                todo!();
            }
        }
    }
}
*/

/// React to reaction.
pub async fn on_reaction(
    event: OriginalSyncReactionEvent, 
    room: Room, 
    ctx: Arc<super::BotContext>
) {
    if room.state() != RoomState::Joined { return; }
    if event.sender == ctx.bot_id { return; }

    // Get related_to event_id and reaction's emoji.
    let reaction_content = &event.content;
    
    let target_event_id = &reaction_content.relates_to.event_id;
    let emoji = &reaction_content.relates_to.key;

    // let mut emoji = reaction_content.relates_to.key.to_string();
    // emoji.retain(|c| c != '\u{FE0F}');

    // Routing.
    //** Why are only the reactions previously added by the bot recognized, 
    // while new reactions from users are not? This is because user-added reactions
    // include the `\u{FE0F}` character following the emoji. We intentionally do not 
    // strip this character, as doing so ensures the desired behavior for reaction-based buttons. */
    
    let reaction: Option<ReactionButton> = emoji.parse().ok(); // or from_str(&emoji).
    let key = match reaction {
        Some(ReactionButton::Next) => BotReactionsCommand::NextListPage,
        Some(ReactionButton::Previous) => BotReactionsCommand::PrevListPage,
        None => return
    };

    // Command Context
    let cmd_ctx = CommandContext::new(event.sender.clone(), room.clone(), ctx.clone()).await;

    // If it is bot's reaction, update a reaction-button state and exit.
    // It's disabled until we'll need reaction-buttons' ids to redact (remove) them.
    /*
    if event.sender == ctx.bot_id {
        let _ = handle_reaction_update(event.event_id, &target_event_id, key, cmd_ctx.clone()).await;
        return;
    }
    */

    // Call the command of a emoji's key.
    let result = match key {
        BotReactionsCommand::NextListPage | BotReactionsCommand::PrevListPage => {
            handle_list_flipping(event.clone(), target_event_id, key, cmd_ctx.clone()).await
        }
    };

    // If there is an error
    if let Err(err) = result {
        // or use unwrap_err() and call to_local()
        let msg = err.to_local(&cmd_ctx.settings.room.room_lang);
        cmd_ctx.msng.text_md(&msg).await;
    }
}

// ===== Handlers =====
// ===== Reminders =====
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
    let (room_settings, delegation) = match (args.room.as_deref(), args.user.as_deref()) {
        (Some(room), Some(uid)) => {
            let room = parse_room(&room, &cmd_ctx).await?;
            let settings = cmd_ctx.ctx.settings_service.load_room(room.room_id(), None).await;
            let uid = UserId::parse(uid).map_err(|_| ReminderError::InvalidUserId)?;
            (settings, DelegationType::User(uid))
        },
        (Some(room), None) => {
            let room = parse_room(&room, &cmd_ctx).await?;
            let settings = cmd_ctx.ctx.settings_service.load_room(room.room_id(), None).await;
            (settings, DelegationType::Room)
        },
        (None, Some(uid)) => {
            let uid = UserId::parse(uid).map_err(|_| ReminderError::InvalidUserId)?;
            (cmd_ctx.settings.room.clone(), DelegationType::User(uid))
        },
        (None, None) => (cmd_ctx.settings.room.clone(), DelegationType::Personal)
    };

    // Set room_tz from settings.
    let tz = room_settings.room_tz.clone();

    // Parse date.
    let date: ParsedDate = if args.interval {
        resolve_date_interval(&args, &tz)?
    } else {
        resolve_date(&args, &tz)?
    };

    // Parse time.
    let time: ParsedTime = if args.interval {
        resolve_time_interval(&args, &tz)?
    } else {
        let default_time = cmd_ctx.ctx.settings_service.get_fallback_time(SettingKey::DefaultTime);
        resolve_time(&args, &tz, default_time)?
    };

    // Get times.
    let (utc_dt, civil_dt) = resolve_target_dt(date, time, &tz)?;

    // Fill a structure.
    let reminder_data = ReminderData {
        room_id: room_settings.room_id.clone(),
        utc_dt,
        civil_dt,
        tz,
        text,
        created_by: cmd_ctx.user_id.clone(),
        delegation,
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

// ===== List =====
/// List of reminders.
pub async fn handle_list(
    args_str: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> Result<(), OutputError> {
    let args_vec: Vec<&str> = args_str.split_whitespace().collect();
    let mut clap_input = vec!["list"];
    clap_input.extend(&args_vec);
    
    let args = match ListArgs::try_parse_from(clap_input) {
        Ok(a) => a,
        Err(_) => {
            let stats = cmd_ctx.ctx.db_ctx.list.get_list_statistics(cmd_ctx.room.room_id()).await?;
            cmd_ctx.send_list_help_message(stats).await;
            
            return Ok(());
        }
    };

    // Check scope and page.
    let scope = ListScopes::from_str(&args.scope).map_err(|_| ReminderError::ListUnknownScope)?;
    let page = args.page.unwrap_or(1);
    let size = args.size.unwrap_or(cmd_ctx.bot_config().list_size);
    let order = args.order.map_or_else(|| {
        Ok(match scope {
            ListScopes::Pending => ListOrder::Asc,
            _ => ListOrder::Desc
        })
    }, |val| {
        ListOrder::from_str(&val).map_err(|_| ReminderError::ListUnknownOrder)
    })?;

    let page_state = PageState { scope, page, size, order};

    // Get Reminder's and sort them by date (to a HashMap), then get localized summary list items.
    let raw_reminders = super::list::get_raw_list(&cmd_ctx.ctx.db_ctx.list, cmd_ctx.room.room_id(), &page_state, false).await?;
    let count = raw_reminders.len();
    // Get a message.
    let message = super::list::get_list_message_by_date(raw_reminders, &page_state, &cmd_ctx).await?;

    // Get event id for the sent message.
    let event_id = cmd_ctx.msng.text_md_long_with_event_id(&message).await.map_err(|_| ReminderError::SendError)?;
    
    // Save event to message_states.
    let _ = cmd_ctx.ctx.db_ctx.list.set_list_page(&event_id, cmd_ctx.room.room_id(), &page_state).await?;

    // Send and save button-reactions.
    super::list::set_list_buttons(&cmd_ctx, &event_id, &page_state.page, &page_state.size, count as i32).await;

    Ok(())
}

/*
/// Set connection between message and reaction in the mesage_states table.
pub async fn _handle_reaction_update(
    event_id: OwnedEventId,
    target_event_id: &EventId,
    key: BotReactionsCommand,
    cmd_ctx: CommandContext,
) -> Result<(), OutputError> {
    let state_key = match key {
        BotReactionsCommand::NextListPage => StateKey::ListButtonNext,
        BotReactionsCommand::PrevListPage => StateKey::ListButtonPrev,
        // _ => return Ok(())
    };

    cmd_ctx.ctx.db_ctx.list.set_page_button(
        target_event_id,
        cmd_ctx.room.room_id(),
        state_key,
        ButtonState { event_id }
    ).await?;

    Ok(())
}
*/

/// Handle flipping through pages of reminders list via reaction-buttons.
// A note on safety. If forwarded messages did not change their `event_id`,
// it would be possible to forward a message with an associated `PageState`
// and view reminders for a room that the user is not a part of.
// The check based on `room_id`, which ensures that a `PageState` can only be retrieved
// for an `event_id` within the current room, provides an additional safeguard.
// However, the error message returned is generic: ReminderError::ListNoState.
// Also, when messages are forwarded, they lose their reactions, which minimizes the chance of an error.
async fn handle_list_flipping(
    _event: OriginalSyncMessageLikeEvent<ReactionEventContent>,
    target_event_id: &EventId,
    key: BotReactionsCommand,
    cmd_ctx: CommandContext,
) -> Result<(), OutputError> {
    let room_id = cmd_ctx.room.room_id();
    let page = cmd_ctx.ctx.db_ctx.list.get_list_page(&target_event_id, &room_id).await?;

    // Deserialize PageState.
    let mut page_state = if let Some(p) = page { p }
    // This branch could be caused by data corruption or absence (due to cleanup process, for example)
    // and **not** in case of next or prev reaction sent by user on a non-list message (see comment on on_reaction()).
    else {
        cmd_ctx.msng.react(target_event_id.to_owned(), super::messaging::MessageReaction::Cross).await;
        Err(ReminderError::ListNoState)?
    };

    // Update PageState. Define next page and its state on the basis of key.
    page_state.page = match key {
        BotReactionsCommand::NextListPage => page_state.page + 1,
        _ => {
            if page_state.page > 1 { page_state.page - 1 }
            else { 1 as i32 }
        }
    };

    // Get Reminder's and sort them by date (to a HashMap), then get localized summary list items.
    let raw_reminders = super::list::get_raw_list(&cmd_ctx.ctx.db_ctx.list, &room_id, &page_state, true).await?;
    let count = raw_reminders.len();

    // Get message.
    let message = super::list::get_list_message_by_date(raw_reminders, &page_state, &cmd_ctx).await?;

    // Send a new message or edit the current.
    if cmd_ctx.bot_config().messages_for_pages {
        let event_id = cmd_ctx.msng.text_md_with_event_id(&message).await.map_err(|_| ReminderError::SendError)?;
        
        // Remove previous PageState and then set new ones
        let _ = cmd_ctx.ctx.db_ctx.list.remove_list_page(&target_event_id, &room_id).await; // mute db error since there is automatic cleaning
        let _ = cmd_ctx.ctx.db_ctx.list.set_list_page(&event_id, &room_id, &page_state).await?;

        // Remove ListButtons and set & save new
        super::list::remove_list_buttons_from_mes(&cmd_ctx, &target_event_id).await;
        super::list::set_list_buttons(&cmd_ctx, &event_id, &page_state.page, &page_state.size, count as i32).await;
    }
    else if cmd_ctx.bot_config().thread_for_pages {
        let reply = Reply {
            event_id: target_event_id.to_owned(),
            enforce_thread: EnforceThread::Threaded(ReplyWithinThread::No),
            add_mentions: AddMentions::No
        };
        let _ = cmd_ctx.msng.reply_with_event_id(&message, reply).await.map_err(|_| ReminderError::SendError)?;
        let _ = cmd_ctx.ctx.db_ctx.list.set_list_page(&target_event_id, &room_id, &page_state).await?;
    }
    else {
        let _ = cmd_ctx.msng.edit_message(&target_event_id, &message).await.map_err(|_| ReminderError::SendError)?;
        let _ = cmd_ctx.ctx.db_ctx.list.set_list_page(&target_event_id, &room_id, &page_state).await?;
    }

    Ok(())
}

// ====== Settings =====
/// Room (optional, user) Settings.
pub async fn handle_settings(
    args_str: &str,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> Result<(), OutputError> {
    let args_vec: Vec<&str> = args_str.split_whitespace().collect();
    let mut clap_input = vec!["settings"];
    clap_input.extend(&args_vec);
    let args = SettingsArgs::try_parse_from(clap_input);

    let result = match args {
        Ok(SettingsArgs::Tz {tz}) => handle_tz_setting(tz, event, cmd_ctx.clone()).await?,
        Ok(SettingsArgs::Lang {lang_key}) => handle_lang_setting(lang_key, cmd_ctx.clone()).await?,
        Ok(SettingsArgs::Time {default, morning, afternoon, evening}) => {
            let updates = vec![
                (SettingKey::DefaultTime, default),
                (SettingKey::Morning, morning),
                (SettingKey::Afternoon, afternoon),
                (SettingKey::Evening, evening),
            ];
            handle_time_setting(event, cmd_ctx.clone(), updates).await?
        },
        Err(_) => {
            let msg = t!(
                "settings.help",
                locale = &cmd_ctx.settings.room.room_lang,
                set_cmd = cmd_ctx.bot_config().settings_commands.join(" | "),
            );
            cmd_ctx.msng.text_md_long(&msg).await;
            return Ok(());
        },
    };

    Ok(result)
}

/// Handle changing time zone.
async fn handle_tz_setting(
    tz_str: Option<String>,
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
) -> Result<(), OutputError> {
    // Parse user's input timezone code
    let tz = match tz_str {
        None => {
            let msg = t!(
                "tz.current", 
                locale = &cmd_ctx.settings.room.room_lang,
                tz = &cmd_ctx.settings.room.room_tz_name,
                cmd_list = cmd_ctx.ctx.bot_config.settings_commands.join(" | ")
            );
            cmd_ctx.msng.text_md(&msg).await;
            return Ok(());
        },
        Some(t) => SettingsService::parse_tz(&t)?
    };

    // If user's input timezone is equal to current room timezone
    if tz == cmd_ctx.settings.room.room_tz {
        return Err(ReminderError::TzNotSet.into());
    }

    // Get name
    let tz_name = tz.iana_name().ok_or(SettingError::InvalidTzFormat)?.to_string();

    // Update by Matrix State Event.
    // suppress the error
    let _res = cmd_ctx.ctx.settings_service.set_matrix_tz(&cmd_ctx.room, &tz_name).await;

    // Update in DB
    let new_setting = SettingUpdate { 
        key: SettingKey::Timezone,
        value: tz_name.clone(),
    };
    cmd_ctx.ctx.settings_service.set_room_settings(
        &cmd_ctx.room.room_id(),
        vec![new_setting],
        &cmd_ctx.user_id,
    ).await?;

    cmd_ctx.send_tz_success(event.clone(), &tz_name).await;

    Ok(())
}

/// Handle language settings.
pub async fn handle_lang_setting(
    lang_key: Option<String>,
    cmd_ctx: CommandContext,
) -> Result<(), SettingError> {
    // Send list of languages.
    let lang = match lang_key {
        Some(l) => l,
        None => {
            // Send a list of available languages.
            let msg_list = t!("settings.lang-list", locale = &cmd_ctx.settings.room.room_lang);
            cmd_ctx.msng.text_md(&msg_list).await;
            let msg = t!(
                "settings.lang-help", 
                locale = &cmd_ctx.settings.room.room_lang, 
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
    if &cmd_ctx.settings.room.room_lang == &lang { return Err(SettingError::LanguageNotSet) };

    // Set new language and send message IN NEW LANGUAGE if it was successful.
    let new_setting = SettingUpdate { 
        key: SettingKey::Lang,
        value: lang.to_string(),
    };
    cmd_ctx.ctx.settings_service.set_room_settings(
        &cmd_ctx.room.room_id(),
        vec![new_setting],
        &cmd_ctx.user_id,
    ).await?;
    let msg = t!("settings.lang-set", locale = lang);
    cmd_ctx.msng.text_md(&msg).await;

    Ok(())
}

/// Handle time settings.
pub async fn handle_time_setting(
    event: OriginalSyncRoomMessageEvent,
    cmd_ctx: CommandContext,
    settings: Vec<(SettingKey, Option<String>)>,
) -> Result<(), SettingError> {
    // Filter Some(String) and validate its format.
    let valid_settings: Result<Vec<SettingUpdate>, SettingError> = settings
        .into_iter()
        .filter(|(_, v)| v.is_some())
        .map(|(k, v)| {
            let val = v.unwrap();
            if super::reminder::is_time_valid(&val) {
                Ok(SettingUpdate { 
                    key: k, 
                    value: val.into(),
                })
            } else {
                Err(SettingError::InvalidTimeFormat)
            }
        })
        .collect();
    let valid_settings = valid_settings?;

    // Send list of default times if vec is empty.
    if valid_settings.is_empty() {
        cmd_ctx.send_default_times().await;
        return Ok(())
    };

    cmd_ctx.ctx.settings_service.set_user_settings(
        &cmd_ctx.user_id, 
        valid_settings,
        &cmd_ctx.user_id,
    ).await?;
    cmd_ctx.send_setting_success(event, "settings.time-set").await;

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
