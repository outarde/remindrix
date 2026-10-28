use std::{sync::Arc, str::FromStr, collections::{HashMap, BTreeMap}};
use std::cmp::Reverse;
use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    Room, ruma::{
        OwnedEventId, EventId,
        RoomId, OwnedRoomId, UserId, OwnedUserId
    }
};
use jiff::{
    Zoned, tz::TimeZone, Timestamp, Unit,
    civil::{DateTime as CivilDateTime, Date}
};
use anyhow::Result;
use strum_macros::{EnumString, Display};
use serde::{Deserialize, Serialize};
use rust_i18n::t;

use crate::context::CommandContext;
use crate::db::{DbContext, ListRepository};
use crate::reminder::{RawReminder, Reminder, ReminderData, ReminderStatus, ReminderError, DelegationType};

/// The lifespan of states, in days.
const STATE_LIFETIME_DAYS: i32 = 30;
/// The number of states that will be removed in a single cleanup cycle.
const STATE_EXPIRED_LIMIT: i32 = 50;

/// Reminders filters/scopes. Can be expanded by options "all" or "assigned".
/// Grouping is not featured.
#[derive(Clone, Debug, PartialEq, EnumString, Serialize, Deserialize)]
#[strum(serialize_all = "snake_case")]
pub enum ListScopes {
    #[strum(ascii_case_insensitive)]
    Pending = 0,
    #[strum(ascii_case_insensitive)]
    Sent = 1,
    #[strum(ascii_case_insensitive)]
    Missed = 2,
}

/// Order for list.
#[derive(Clone, Debug, PartialEq, EnumString, Serialize, Deserialize, Display)]
#[strum(serialize_all = "UPPERCASE")]
pub enum ListOrder {
    #[strum(ascii_case_insensitive)]
    Asc,
    #[strum(ascii_case_insensitive)]
    Desc,
}

/// Keys for states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, Display)]
#[strum(serialize_all = "snake_case")]
pub enum StateKey {
    Page,
    ListButtons,
    // Filter,
    // SelectedItem,
}

/// State of the current list of reminders. Bound to message with a list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageState { 
    pub scope: ListScopes, 
    pub page: i32, 
    pub size: i32,
    pub order: ListOrder,
}

/// A common way to record sub-events like reaction-buttons.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct _ReactionState {
    pub target_event_id: OwnedEventId,
    pub event_id: OwnedEventId,
    // pub key: StateKey,
}

//
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListButtons {
    pub message: OwnedEventId,
    pub prev_button: OwnedEventId,
    pub next_button: OwnedEventId,   
}

//
#[derive(Debug, Clone)]
pub struct RawListButtons {
    pub room_id: String,
    pub value: String,
}


/// List statistics.
#[derive(Debug)]
pub struct ReminderStats {
    pub total: i64,
    pub by_status: Vec<StatusCount>,
}

#[derive(Debug)]
pub struct StatusCount {
    pub status: ReminderStatus,
    pub count: i64,
}

pub struct RawStatRow {
    pub status_val: i64,
    pub count: i64,
    pub total: i64,
}

/// Get raw reminders by the one scope filter.
pub async fn get_raw_list(
    list_repo: &ListRepository,
    room_id: &RoomId,
    page_state: &PageState,
    is_flipping: bool,
) -> Result<Vec<RawReminder>, ReminderError> {
    let raw_reminders = list_repo.get_by_page(room_id, page_state.clone()).await?;
    
    // If there is no reminders, send an error depending on whether there are no reminders or they have run out.
    if raw_reminders.is_empty() {
        if page_state.page > 1 {
            if is_flipping { return Err(ReminderError::ListEnded.into()) }
            else { return Err(ReminderError::ListEmptyPage.into()) }
        }
        match page_state.scope {
            ListScopes::Pending => return Err(ReminderError::ListEmptyAffectionately.into()),
            _ => return Err(ReminderError::ListEmpty.into())
        }
    }

    Ok(raw_reminders)
}

/// RawReminder to Reminder
pub fn from_raw_reminders(ctx: Arc<super::BotContext>, raw_reminders: Vec<RawReminder>) -> Result<Vec<Reminder>> {
    // Make a Vec for reminders.
    let mut reminders = Vec::with_capacity(raw_reminders.len());

    for raw in raw_reminders {
        let id: i64 = raw.id;
        let room_id_str = raw.room_id.to_string();
        let created_by_str = raw.created_by.to_string();
        let text = raw.text;
        let utc_time_str = raw.utc_time;
        let room_tz_str = raw.tz;

        // Parse strings to OwnedRoomId and OwnedUserId
        let room_id = RoomId::parse(&room_id_str)?;
        let created_by = UserId::parse(&created_by_str)?;

        // Delegation data
        let delegation = match DelegationType::from_raw(&raw.delegation_kind, raw.target_user_id.as_deref()) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    reminder_id = raw.id,
                    delegation_kind = %raw.delegation_kind,
                    target_user = ?raw.target_user_id,
                    "invalid delegation in DB, falling back to Personal: {e}"
                );
                DelegationType::Personal
            }
        };

        // Get TZ
        let tz = ctx.settings_service.get_tz_or_fallback(Some(room_tz_str));

        // Parse Utc as Timestamp
        let timestamp: Timestamp = match utc_time_str.parse() {
            Ok(t) => t,
            // DateTime for the legacy (chrono) format of utc_time as 2026-09-06 04:00:00 UTC
            Err(_) => {
                let parts: Vec<&str> = utc_time_str.split(|c| c == '-' || c == ' ' || c == ':').collect();
                if parts.len() < 5 {
                    return Err(anyhow::anyhow!("Error restoring reminders with the legacy date format; delete the database to fix this."));
                }

                let target_time = CivilDateTime::new(
                    parts[0].parse().unwrap(), 
                    parts[1].parse().unwrap(), 
                    parts[2].parse().unwrap(),
                    parts[3].parse().unwrap(), 
                    parts[4].parse().unwrap(),
                    0,
                    0
                )?;

                let utc_time: Zoned = target_time.to_zoned(TimeZone::UTC)?;
                utc_time.timestamp()
            }
        };

        // Parse Civil from Timestamp -> Zoned.
        // let target_zoned = timestamp.in_tz(&room_tz_str);
        let dt_zoned = timestamp.to_zoned(tz.clone());
        let civil_dt = dt_zoned.datetime();

        // Prepare ReminderData.
        let reminder_data = ReminderData {
            room_id,
            utc_dt: timestamp,
            civil_dt,
            tz,
            text,
            created_by,
            delegation,
        };

        reminders.push(Reminder {
            id,
            data: reminder_data,
            status: ReminderStatus::Pending,
        });
    }

    Ok(reminders)
}

/// Group Vec<Reminder> to BTreeMap (for preserving date groups order) with Date as a key.
pub fn group_by_date(reminders: Vec<Reminder>) -> BTreeMap<Date, Vec<Reminder>> {
    let mut sorted_reminders: BTreeMap<Date, Vec<Reminder>> = BTreeMap::new();

    for reminder in reminders {
        sorted_reminders
            .entry(reminder.data.utc_dt.to_zoned(reminder.data.tz.clone()).date().clone())
            .or_default()
            .push(reminder);
    }

    sorted_reminders
}

/// Get list as a Vec<String> summary of grouped by date reminders with asc as default or desc order
/// (with the prerequisite that reminders in the Vec have been sorted already).
pub fn get_date_grouped_items(date_sorted_reminders: BTreeMap<Date, Vec<Reminder>>, locale: &str, order: &ListOrder) -> Vec<String> {
    let mut list_messages = Vec::new();
    // let mut list_messages = Vec::with_capacity(date_sorted_reminders.values().map(|v| v.len()).sum());

    match order {
        ListOrder::Asc => {
            for (date, reminders) in date_sorted_reminders {
                // let mut sorted = reminders.clone();
                // sorted.sort_by_key(|r| (r.data.civil_dt, r.id));

                list_messages.push(get_list_section_for_date(reminders, &date, &locale));
            }
        },
        ListOrder::Desc => {
            for (date, reminders) in date_sorted_reminders.iter().rev() {
                // If you need sort reminders in the each date-item, too
                let mut sorted = reminders.to_vec();
                sorted.sort_by_key(|r| (Reverse(r.data.civil_dt), r.id));
                // or:
                // sorted.sort_by(|a, b| b.cmp(a));

                list_messages.push(get_list_section_for_date(sorted, &date, &locale));
            }
        }
    }

    list_messages
}

/// Make message of reminders at one date.
fn get_list_section_for_date(reminders: Vec<Reminder>, date: &Date, locale: &str) -> String {
    let items = reminders
        .iter()
        .map(|r| {
            match r.data.delegation {
                DelegationType::Personal => {
                    let time = r.data.civil_dt.strftime("%H:%M").to_string();
                    let item = t!(
                        "list.by-date.item", 
                        locale = locale, 
                        text = r.data.text,
                        time = time
                    );
                    item
                },
                DelegationType::User(ref user_id) => {
                    let time = r.data.civil_dt.strftime("%H:%M").to_string();
                    let item = t!(
                        "list.by-date.item-user-delegated", 
                        locale = locale, 
                        text = r.data.text,
                        time = time,
                        to = &user_id.to_string(),
                        from = r.data.created_by
                    );
                    item
                },
                DelegationType::Room => {
                    let time = r.data.civil_dt.strftime("%H:%M").to_string();
                    let item = t!(
                        "list.by-date.item-room-delegated", 
                        locale = locale, 
                        text = r.data.text,
                        time = time,
                        from = r.data.created_by
                    );
                    item
                }
            }
            
        })
        .collect::<String>();

    // Prepare a final summary message.
    let message = t!(
        "list.by-date.section", 
        locale = locale,
        date = date.strftime("%d.%m.%Y"),
        items = items
    );

    return message.to_string();
}

/// Handles the entire process of retrieving the necessary reminders, grouping them, and returning the message.
pub async fn get_list_message_by_date(
    raw_reminders: Vec<RawReminder>,
    page_state: &PageState,
    cmd_ctx: &CommandContext
) -> Result<String, ReminderError> {
    // Get Reminder's and sort them by date (to a HashMap), then get localized summary list items.
    // let raw_reminders = get_raw_list(&cmd_ctx.ctx.db_ctx.reminders, cmd_ctx.room.room_id(), page_state.clone()).await?;
    let reminders = from_raw_reminders(cmd_ctx.ctx.clone(), raw_reminders).map_err(|_| ReminderError::ListError)?;
    let reminders_count = reminders.len();

    let sorted_reminders = group_by_date(reminders);
    let list = get_date_grouped_items(sorted_reminders, &cmd_ctx.settings.room.room_lang, &page_state.order);

    // Form a message.
    let scope_localized = t!(format!("list.scope-short.{}", page_state.scope.clone() as i32), locale = &cmd_ctx.settings.room.room_lang);
    let order_localized = t!(format!("list.order.{}", page_state.order.to_string()), locale = &cmd_ctx.settings.room.room_lang);

    let message = t!(
        "list.by-date", 
        locale = &cmd_ctx.settings.room.room_lang,
        scope_short = scope_localized,
        count = reminders_count,
        size = page_state.size,
        page = page_state.page,
        order = order_localized,
        list = list.join("\n")
    );
    
    Ok(message.to_string())
}

// ===== List-Messages =====
/// Get and remove ListButtons for the target_event_id.
pub async fn remove_list_buttons_from_mes(cmd_ctx: &CommandContext, target_event_id: &EventId) {
    let old_buttons = cmd_ctx.ctx.db_ctx.list.get_and_delete_page_buttons(&target_event_id, cmd_ctx.room.room_id()).await;
    
    // Mute DB error because it will not be obvious to users which event ended in an error.
    match old_buttons {
        Ok(buttons) => {
            if let Some(b) = buttons {
                let _ = cmd_ctx.room.redact(&b.prev_button, Some("Turn the page and move the reaction-button"), None).await;
                let _ = cmd_ctx.room.redact(&b.next_button, Some("Turn the page and move the reaction-button"), None).await;
            }
            // If None, the method in db.rs has already sent tracing::error!().
        }
        Err(error) => {
            if let ReminderError::Db(e) = error {
                tracing::error!("SQLite error while deleting page_buttons for event_id: {}. Error: {:?}", &target_event_id, e);
            }
        }
    }
}

/// Send & save buttons on the basis of the current page and remaining reminders that haven't been displayed yet.
pub async fn set_list_buttons(cmd_ctx: &CommandContext, event_id: &EventId, page: &i32, size: &i32, count: i32) {
    // Buttons are not displayed if there are no pages beyond the first one
    if count != *size && *page == 1 { return; }

    if let Some(buttons) = cmd_ctx.send_list_buttons(event_id).await {
        let res = cmd_ctx.ctx.db_ctx.list.set_page_buttons(&event_id, cmd_ctx.room.room_id(), buttons).await;

        // Mute DB error because adding buttons to a table only affects the ability to delete them later, 
        // whereas navigating between pages is handled using the message event_id itself.
        if let Err(e) = res {
            tracing::error!("SQLite error while insert page_buttons for event_id: {}. Error: {:?}", &event_id, e);
        }
    } else {
        tracing::error!("Can't display page buttons for the event_id: {:?}", &event_id);
    }
}

/// Clean-up task for old page and page_buttons.
pub async fn states_cleanup(ctx: Arc<super::BotContext>) -> anyhow::Result<()> {
    tracing::info!("Starting automatic page states cleanup...");

    let states_count = ctx.db_ctx.list.cleanup_page_states(&STATE_LIFETIME_DAYS, &STATE_EXPIRED_LIMIT).await?;
    let raw_buttons = ctx.db_ctx.list.cleanup_and_get_buttons_states(&STATE_LIFETIME_DAYS, &STATE_EXPIRED_LIMIT).await?;
    let raw_buttons_len = raw_buttons.len();
    
    if states_count > 0 {
        tracing::info!("{} pages have been successfully deleted", states_count);
    }

    // Make a HashMap for each room with buttons
    let mut cleanup_buttons: HashMap<OwnedRoomId, Vec<ListButtons>> = HashMap::new();

    for raw_button in raw_buttons {
        let room_id = RoomId::parse(&raw_button.room_id)?;
        let list_buttons = serde_json::from_str::<ListButtons>(&raw_button.value)?;

        cleanup_buttons
            .entry(room_id.clone())
            .or_default()
            .push(list_buttons);
    }

    let cleaned_rooms = cleanup_buttons.len();

    // Redact buttons from room
    for (room_id, list_buttons) in cleanup_buttons {
        let room = match ctx.client.get_room(&room_id) {
            Some(r) => r,
            None => {
                tracing::error!("Error removing ListButtons from messages. Can't load room with id: {:?}", room_id);
                continue;
            }
        };
        for button in list_buttons {
            let _ = room.redact(&button.prev_button, Some("Reaction-button has expired"), None).await;
            let _ = room.redact(&button.next_button, Some("Reaction-button has expired"), None).await;

            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }
    if cleaned_rooms > 0 {
        tracing::info!("{} reaction-button states from {} room(s) have been successfully redacted",
            raw_buttons_len,
            cleaned_rooms
        );
    }

    tracing::info!("Automatic page states cleanup completed successfully!");

    Ok(())
}
