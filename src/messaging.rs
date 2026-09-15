use matrix_sdk::{
    Room, 
    ruma::{
        OwnedEventId,
        events::{
            reaction::ReactionEventContent, relation::Annotation,
            room::message::{RoomMessageEventContent, OriginalSyncRoomMessageEvent}
        }
    }
};
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use rust_i18n::t;
use strum_macros::{Display, EnumString};
use jiff::{
    Zoned, Span, ToSpan, SpanTotal, SpanRound,
    tz::TimeZone, Timestamp, Unit,
    civil::{DateTime as CivilDateTime, Date}
};
use crate::handlers::{CommandContext, I18nManager};
use crate::reminder::ReminderData;

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
            1 => Self::CalendarMonth,
            2 => Self::CalendarMonth,
            3 => Self::ClockHour,
            4 => Self::Clock,
            _ => Self::Cross,
        }
    }
}

// ===== Messages =====
/// Messages and reactions.
#[derive(Clone, Debug)]
pub struct RoomMessenger {
    room: Room,
    i18n: Arc<I18nManager>,
}

impl RoomMessenger {
    pub fn new(room: Room, i18n: Arc<I18nManager>) -> Self {
        Self { room, i18n }
    }

    /// Send markdown message with the typing indciator (Typing Guard).
    pub async fn text_md(&self, text: &str) {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let _ = self.room.typing_notice(false).await;

        let _ = self.room.send(RoomMessageEventContent::text_markdown(text)).await;
    }

    /// Send markdown message with the typing indciator slightly longer than normal.
    pub async fn text_md_long(&self, text: &str) {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = self.room.typing_notice(false).await;

        let _ = self.room.send(RoomMessageEventContent::text_markdown(text)).await;
    }

    /// Send plain text message with the typing indciator.
    pub async fn text_plain(&self, text: &str) {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = self.room.typing_notice(false).await;

        let _ = self.room.send(RoomMessageEventContent::text_plain(text)).await;
    }

    // ===== Reactions =====
    /// Send reaction to the related event (message).
    pub async fn react(
        &self,
        event_id: OwnedEventId,
        emoji: MessageReaction
    ) {
        let annotation = Annotation::new(event_id, emoji.to_string());
        let content = ReactionEventContent::new(annotation);
        
        let _ = self.room.send(content).await;
    }
    /// Send multiple reactions.
    pub async fn react_bundle(
        &self,
        event_id: OwnedEventId,
        emojis: Vec<MessageReaction>
    ) {
        for k in emojis {
            let annotation = Annotation::new(event_id.clone(), k.to_string());
            let content = ReactionEventContent::new(annotation);
            
            let _ = self.room.send(content).await;
        }
    }
}
/*
impl Drop for TypingGuard {
    fn drop(&mut self) {
        let room = self.room.clone();
        tokio::spawn(async move {
            let _ = room.typing_notice(false).await;
        });
    }
}
*/

/*
async fn send_error(err: &ReminderError, cmd_ctx: &CommandContext) {
    let err_msg = t!(&err.to_string()); 
    let _ = cmd_ctx.room.send(RoomMessageEventContent::text_plain(err_msg)).await;
}
*/

// ===== Reactions =====
/// Send reaction to the related event (message).
pub async fn send_reaction(
    event_id: OwnedEventId, 
    room: &Room, 
    emoji_key: MessageReaction
) {
    let annotation = Annotation::new(event_id, emoji_key.to_string());
    let content = ReactionEventContent::new(annotation);
    
    let _ = room.send(content).await;
}

/// Send reactions with digits emoji with to the related event (message).
/// Calculates the time until an event occurs and selects the number of the 
/// largest non-empty dimension (days -> hours -> minutes).
pub async fn send_digits_reaction(
    event_id: OwnedEventId, 
    room: &Room,
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
            let _ = send_reaction(event_id.clone(), room, MessageReaction::from_digit_time_type(idx as u32)).await;

            let mut digits = Vec::new();
         
            while d > 0 {
                digits.push((d % 10) as u32);
                d /= 10;
            }
            // we don't need reverse vector as Matrix clients
            // display new reactions at the left of message bubble.
            // digits.reverse();

            for digit_emoji in digits {
                let _ = send_reaction(event_id.clone(), room, MessageReaction::from_digit(digit_emoji)).await;
            }

        },
        // so we can't send numbers with equal digits and send "check" emoji instead
        None => {
            let _ = send_reaction(event_id.clone(), room, MessageReaction::Timer).await;
        }
    }
}

// ====== ======
/// Calculates the time until an event occurs and selects the number of the 
/// largest non-empty dimension (days -> hours -> minutes).
pub async fn get_digits_emogi(
    event_id: OwnedEventId, 
    room: &Room,
    numbers: Vec<i32>
) -> Vec<MessageReaction> {
    // First positive number, whose remainder when divided by 11 is not 0.
    // (Matrix prevents sending the same reaction twice: status_code: 400, DuplicateAnnotation.)
    let first_positive = numbers
        .iter()
        .enumerate()
        .find(|&(_, &x)| x > 0 && x < 100)
        .and_then(|(idx, &x)| if x % 11 == 0 { None } else { Some((idx, x)) });

    match first_positive {
        Some((idx, mut d)) => {
            let _ = send_reaction(event_id.clone(), room, MessageReaction::from_digit_time_type(idx as u32)).await;

            let mut digits = Vec::new();
         
            while d > 0 {
                digits.push((d % 10) as u32);
                d /= 10;
            }
            // we don't need reverse vector as Matrix clients
            // display new reactions at the left of message bubble.
            // digits.reverse();

            let emojis = digits.iter().map(|&d| MessageReaction::from_digit(d)).collect();
            return emojis;

        },
        // so we can't send numbers with equal digits and send "check" emoji instead
        None => {
            return vec![MessageReaction::Timer];
        }
    }
}

//===== Time and Date Calculation =====
/// Calculate weeks, days, hours and minutes before some time
pub fn calculate_durations(utc_time: Timestamp) -> Vec<i32> {
    let now = Timestamp::now();
    let relative = now.to_zoned(TimeZone::UTC);
    
    // Get span
    let span = utc_time.since(now).unwrap();
    
    // Round for minutes with relative point to count months, too
    // let span = span.round(SpanRound::new().smallest(Unit::Minute).relative(&zdt_now)).unwrap();

    let numbers = vec![
        span.total((Unit::Month, &relative)).unwrap() as i32,
        span.total((Unit::Week, &relative)).unwrap() as i32, 
        span.total((Unit::Day, &relative)).unwrap() as i32,
        span.total((Unit::Hour, &relative)).unwrap() as i32,
        span.total((Unit::Minute, &relative)).unwrap() as i32
    ];
    // let numbers = numbers.iter().map(|&n| n as i32).collect();
    return numbers;
}
