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
        },
        api::client::typing::create_typing_event::v3::Typing
    }
};
use anyhow::Result;
use tokio::time::{Duration, sleep};
use jiff::{
    Zoned, Span, ToSpan, tz::TimeZone, 
    civil::{DateTime as CivilDateTime, Date}
};
use std::{string::ToString, sync::{OnceLock, Arc}};
use rust_i18n::t;

// app crates
use crate::config::BotConfig;
use crate::db::ReminderRepository;
use crate::reminder::{ReminderData, ReminderStatus, ReminderError};
use crate::settings::{RoomTimezoneContent, SettingsManager};
use crate::messaging::{
    RoomMessenger, MessageReaction,
};

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
    pub ctx: Arc<super::BotContext>,
    pub settings: SettingsManager,
    pub i18n: Arc<I18nManager>,
    pub msng: RoomMessenger,
}

impl CommandContext {
    pub async fn new(user_id: OwnedUserId, room: Room, ctx: Arc<super::BotContext>) -> Self {
        let settings = SettingsManager::new(&room, Some(user_id.clone()), &ctx).await;
        let i18n = ctx.get_i18n_manager(&settings.room_lang).await;
        let msng = RoomMessenger::new(room.clone(), i18n.clone());

        Self { room, user_id, ctx, settings, i18n, msng } 
    }

    // ===== Getters =====
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

    // ===== Messages =====
    /// Send welcome message with help to the room.
    pub async fn send_welcome_message(&self) {
        let tomorrow = Zoned::now()
            .with_time_zone(self.settings.room_tz.clone())
            .checked_add(1.days()).unwrap_or_else(|_| {
                Zoned::now().with_time_zone(TimeZone::UTC).checked_add(1.days()).unwrap()
            });
        let (month_str, month_str_truncated) = self.i18n.format_month(&(tomorrow.month() as u32)).unwrap();

        let welcome_type = if self.ctx.bot_config.quick_remind {
            "welcome.on_command_off"
        } else { "welcome.on_command" };

        let welcome_msg = t!(
            welcome_type,
            locale = &self.settings.room_lang,
            cmd_local = self.i18n.cmd_remind,
            cmd_list = self.ctx.bot_config.remind_commands.join("|"),
            cmd_tz_list = self.ctx.bot_config.tz_commands.join("|"),
            date = tomorrow.strftime("%d.%m.%Y").to_string(),
            date_slash = tomorrow.strftime("%d/%m/%Y").to_string(),
            date_hyphen = tomorrow.strftime("%d-%m").to_string(),
            date_d = tomorrow.day().to_string(),
            month = month_str,
            month_truncated = month_str_truncated,
            today = self.i18n.today,
            tomorrow = self.i18n.tomorrow,
            morning = self.i18n.morning,
            afternoon = self.i18n.afternoon,
            evening = self.i18n.evening
        );

        self.msng.text_md_long(&welcome_msg).await;
    }
    /// Send a message or reaction about a successfully created reminder to the room.
    pub async fn send_reminder_success(
        &self,
        event: OriginalSyncRoomMessageEvent, 
        reminder: ReminderData, 
        interval: bool
    ) {
        if self.bot_config().send_reactions {
            // Send digits reaction or one emoji if:
            // this setting is on, it is not an interval, it is not delegated.
            if self.bot_config().send_digits_reactions 
                && !interval 
                && &reminder.settings.room_id == &self.settings.room_id 
            {
                let numbers = super::messaging::calculate_durations(reminder.utc_dt);
                let emojis = super::messaging::get_emojis_for_duration(numbers);
                self.msng.react_bundle(event.event_id.clone(), emojis).await;
            }
            else {
                self.msng.react(event.event_id.clone(), MessageReaction::Timer).await;
            }
        } else {
            let date = reminder.civil_dt.strftime("%d.%m.%Y").to_string();
            let time = reminder.civil_dt.strftime("%H:%M").to_string();
            let msg = t!("reminder.saved", locale = &self.settings.room_lang, date = date, time = time);
            self.msng.text_plain(&msg).await;
        }
    }

    /// Send a message or reaction about a successfully created reminder to the room.
    pub async fn send_tz_success(
        &self,
        event: OriginalSyncRoomMessageEvent,
        tz: &str,
    ) {
        if self.bot_config().send_reactions {
            self.msng.react(event.event_id.clone(), MessageReaction::Check).await;
        } else {
            let msg = t!("tz.set", locale = &self.settings.room_lang, tz = tz); 
            self.msng.text_md(&msg).await;
        }
    }
}
