use matrix_sdk::{
    ruma::{
        OwnedEventId,
        events::{
            reaction::ReactionEventContent, relation::Annotation,
            room::message::{RoomMessageEventContent, OriginalSyncRoomMessageEvent}
        }
    }
};
use rust_i18n::t;
use strum_macros::{Display, EnumString};
use jiff::{
    Zoned, Span, ToSpan, tz::TimeZone, Timestamp,
    civil::{DateTime as CivilDateTime, Date}
};
use crate::handlers::CommandContext;
use crate::parsers::ReminderData;

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
    #[strum(serialize = "🕛")]
    ClockHour,
    #[strum(serialize = "📅")]
    CalendarDay,
    #[strum(serialize = "🗓️")]
    CalendarMonth,
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
    /// Returns the emoji corresponding to the interval measure type:
    /// Months, weeks, days, hours, minutes.
    fn from_digit_time_type(digit: u32) -> Self {
        match digit {
            0 => Self::CalendarMonth,
            1 => Self::CalendarDay,
            2 => Self::CalendarDay,
            3 => Self::ClockHour,
            4 => Self::Clock,
            _ => Self::Cross,
        }
    }
}

// ===== Special Messages =====
/// Send welcome message with help to the room.
pub async fn send_welcome_message(cmd_ctx: CommandContext) -> anyhow::Result<()> {
    let tomorrow = Zoned::now()
        .with_time_zone(cmd_ctx.settings.room_tz.clone())
        .checked_add(1.days())?;
    let (month_str, month_str_truncated) = &cmd_ctx.i18n.format_month(&(tomorrow.month() as u32)).unwrap();

    let welcome_type = if cmd_ctx.ctx.bot_config.quick_remind {
        "welcome.on_command_off"
    } else { "welcome.on_command" };

    let welcome_msg = t!(
        welcome_type,
        cmd_local = &cmd_ctx.i18n.cmd_remind,
        cmd_list = &cmd_ctx.ctx.bot_config.remind_commands.join("|"),
        cmd_tz_list = &cmd_ctx.ctx.bot_config.tz_commands.join("|"),
        date = tomorrow.strftime("%d.%m.%Y").to_string(),
        date_slash = tomorrow.strftime("%d/%m/%Y").to_string(),
        date_hyphen = tomorrow.strftime("%d-%m").to_string(),
        date_d = tomorrow.day().to_string(),
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

    Ok(())
}

/// Send a message or reaction about a successfully created reminder to the room.
pub async fn send_success(
    event: OriginalSyncRoomMessageEvent, 
    cmd_ctx: &CommandContext, 
    reminder: ReminderData, 
    interval: bool
) {
    if cmd_ctx.bot_config().send_reactions {
        // Send digits reaction or one emoji if it is not an interval.
        if cmd_ctx.bot_config().send_digits_reactions && !interval {
            let digits = calculate_durations(reminder.utc_dt.timestamp());
            let _ = send_digits_reaction(event.event_id.clone(), &cmd_ctx, digits).await;
        }
        else {
            let _ = send_reaction(event.event_id.clone(), &cmd_ctx, MessageReaction::Timer).await;
        }
    } else {
        let date_str = reminder.civil_dt.strftime("%d.%m.%Y").to_string();
        let hour_str = reminder.civil_dt.strftime("%H").to_string();
        let min_str = reminder.civil_dt.strftime("%M").to_string();
        let reminder_mes = t!("reminder.saved", date = date_str, hour = hour_str, min = min_str);
        let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(reminder_mes)).await;
    }
}

// ===== Reactions =====
/// Send reaction to the related event (message).
pub async fn send_reaction(
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
pub async fn send_digits_reaction(
    event_id: OwnedEventId, 
    cmd_ctx: &CommandContext,
    numbers: Vec<i32>
) {
    // First positive number, whose remainder when divided by 11 is not 0.
    // (Matrix prevents sending the same reaction twice: status_code: 400, DuplicateAnnotation.)
    let first_positive = numbers
        .iter()
        .enumerate()
        .find(|&(_, &x)| x > 0 && x < 100)
        .and_then(|(idx, &x)| if x % 11 == 0 { None } else { Some((idx, x)) });

    match first_positive {
        Some((idx, mut d)) => {
            let _ = send_reaction(event_id.clone(), &cmd_ctx, MessageReaction::from_digit_time_type(idx as u32)).await;

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

//===== Time and Date Calculation =====
/// Calculate weeks, days, hours and minutes before some time
pub fn calculate_durations(utc_time: Timestamp) -> Vec<i32> {
    let duration_to_wait = utc_time.since(Timestamp::now()).unwrap();
    let numbers = vec![
        duration_to_wait.get_months(),
        duration_to_wait.get_weeks(), 
        duration_to_wait.get_days(), 
        duration_to_wait.get_hours(),
        duration_to_wait.get_minutes().try_into().unwrap()
    ];
    return numbers;
}
