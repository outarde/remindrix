use matrix_sdk::{
    Room, room::edit::EditedContent,
    room::reply::Reply,
    ruma::{
        EventId, OwnedEventId,
        events::{
            reaction::ReactionEventContent, relation::Annotation,
            room::message::{RoomMessageEventContent, RoomMessageEventContentWithoutRelation}
        },
        api::client::message::send_message_event::v3::Response
    }
};
use std::{sync::Arc, iter::once};
use tokio::time::Duration;
use strum_macros::{Display, EnumString};
use jiff::{
    tz::TimeZone, Timestamp, Unit
};
use crate::context::I18nManager;

// #[derive(strum_macros::Display)]
// #[strum(to_string = "")]
#[derive(PartialEq, EnumString, Display)]
#[strum(serialize_all = "snake_case")]
pub enum MessageReaction {
    #[strum(serialize = "👍")] ThumbsUp,
    #[strum(serialize = "✅")] Check,
    #[strum(serialize = "❌")] Cross,
    #[strum(serialize = "🟢")] Done,
    #[strum(serialize = "⏲️")] Timer, 
    #[strum(serialize = "🕒")] Clock,
    #[strum(serialize = "🕛")] ClockHour,
    #[strum(serialize = "📆")] CalendarDay,
    #[strum(serialize = "🗓️")] CalendarMonth,
    #[strum(serialize = "0️⃣")] Zero,
    #[strum(serialize = "1️⃣")] One,
    #[strum(serialize = "2️⃣")] Two,
    #[strum(serialize = "3️⃣")] Three,
    #[strum(serialize = "4️⃣")] Four,
    #[strum(serialize = "5️⃣")] Five,
    #[strum(serialize = "6️⃣")] Six,
    #[strum(serialize = "7️⃣")] Seven,
    #[strum(serialize = "8️⃣")] Eight,
    #[strum(serialize = "9️⃣")] Nine,
    #[strum(serialize = "⏩")] Next,
    #[strum(serialize = "⏪")] Previous,
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
    /// Months, ~~weeks~~ (not used), days, hours, minutes.
    fn from_digit_time_type(digit: u32) -> Self {
        match digit {
            0 => Self::CalendarMonth,
            // 1 => Self::CalendarMonth,
            1 => Self::CalendarDay,
            2 => Self::ClockHour,
            3 => Self::Clock,
            _ => Self::Cross,
        }
    }
}

#[derive(PartialEq, EnumString, Display)]
#[strum(serialize_all = "snake_case")]
pub enum ReactionButton {
    #[strum(serialize = "⏩")] Next,
    #[strum(serialize = "⏪")] Previous,
}

// ===== Messages =====
/// Messages and reactions.
#[derive(Clone, Debug)]
pub struct RoomMessenger {
    room: Room,
    // i18n: Arc<I18nManager>,
}

impl RoomMessenger {
    pub fn new(room: Room, _i18n: Arc<I18nManager>) -> Self {
        Self { room }
    }

    /// Send markdown message with the typing indciator (Typing Guard).
    pub async fn text_md(&self, text: &str) {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
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

    /// Send message with response (usually for event_id).
    pub async fn _text_md_long_with_response(&self, text: &str) -> anyhow::Result<Response> {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = self.room.typing_notice(false).await;

        // Response has type: `ruma::api::client::message::send_message_event::v3::Response`
        // or `matrix_sdk::ruma::api::ruma_client_api::message::send_message_event::v3::Response`.
        // `matrix_sdk::room::futures::SendMessageLikeEvent` is what you get before response.response.
        let response = self.room.send(RoomMessageEventContent::text_markdown(text)).await?;
        
        Ok(response.response)
    }

    /// Send message with returning event_id.
    pub async fn text_md_with_event_id(&self, text: &str) -> anyhow::Result<OwnedEventId> {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = self.room.typing_notice(false).await;

        // Full response has type: `ruma::api::client::message::send_message_event::v3::Response`
        // or `matrix_sdk::ruma::api::ruma_client_api::message::send_message_event::v3::Response`.
        // `matrix_sdk::room::futures::SendMessageLikeEvent` is what you get before response.response
        let response = self.room.send(RoomMessageEventContent::text_markdown(text)).await?;
        let event_id = response.response.event_id.to_owned();
        
        Ok(event_id)
    }

    /// Send text as a Reply.
    pub async fn reply_with_event_id(&self, text: &str, reply: Reply) -> anyhow::Result<OwnedEventId> {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = self.room.typing_notice(false).await;

        let content = self.room.make_reply_event(RoomMessageEventContentWithoutRelation::text_markdown(text), reply).await?;
        let response = self.room.send(content).await?;
        let event_id = response.response.event_id.to_owned();
        
        Ok(event_id)
    }

    /// Send message with returning event_id.
    pub async fn text_md_long_with_event_id(&self, text: &str) -> anyhow::Result<OwnedEventId> {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = self.room.typing_notice(false).await;

        // Full response has type: `ruma::api::client::message::send_message_event::v3::Response`
        // or `matrix_sdk::ruma::api::ruma_client_api::message::send_message_event::v3::Response`.
        // `matrix_sdk::room::futures::SendMessageLikeEvent` is what you get before response.response
        let response = self.room.send(RoomMessageEventContent::text_markdown(text)).await?;
        let event_id = response.response.event_id.to_owned();
        
        Ok(event_id)
    }

    /// Send plain text message with the typing indciator.
    pub async fn text_plain(&self, text: &str) {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = self.room.typing_notice(false).await;

        let _ = self.room.send(RoomMessageEventContent::text_plain(text)).await;
    }

    /// Edit message.
    pub async fn edit_message(
        &self,
        original_event_id: &EventId,
        new_text: &str,
    ) -> anyhow::Result<()> {
        let _ = self.room.typing_notice(true).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = self.room.typing_notice(false).await;

        let new_content = RoomMessageEventContentWithoutRelation::text_markdown(new_text);
        let edited_content = EditedContent::RoomMessage(new_content);

        let edit_event_content = self.room
            .make_edit_event(original_event_id, edited_content)
            .await?;

        let _ = self.room.send(edit_event_content).await?;

        Ok(())
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
    /// Send reaction to the related event (message).
    pub async fn react_with_event_id(
        &self,
        event_id: OwnedEventId,
        emoji: MessageReaction
    ) -> anyhow::Result<OwnedEventId> {
        let annotation = Annotation::new(event_id, emoji.to_string());
        let content = ReactionEventContent::new(annotation);
        
        let response = self.room.send(content).await?;
        let reaction_event_id = response.response.event_id.to_owned();
        
        Ok(reaction_event_id)
    }
    /// Send multiple reactions.
    pub async fn react_bundle(
        &self,
        event_id: OwnedEventId,
        emojis: Vec<MessageReaction>
    ) {
        for e in emojis {
            let annotation = Annotation::new(event_id.clone(), e.to_string());
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
/// Get digits emojis before utc_dt.
pub fn get_emojis_for_duration(
    numbers: Vec<i32>,
) -> Vec<MessageReaction> {
    let emojis = match get_digits(numbers) {
        Some((leading, digits)) => {
            once(MessageReaction::from_digit_time_type(leading as u32))
                .chain(digits.into_iter().map(|d| MessageReaction::from_digit(d)))
                .collect::<Vec<MessageReaction>>()
        },
        // so we can't send numbers with equal digits and send "check" emoji instead
        None => vec![MessageReaction::Timer]
    };

    return emojis;
}

//===== Calculations =====
/// Return the largest number as digits and its time dimension 
/// (months -> weeks -> days -> hours -> minutes).
pub fn get_digits(
    numbers: Vec<i32>
) -> Option<(usize, Vec<u32>)> {
    // First positive number, whose remainder when divided by 11 is not 0.
    // (Matrix prevents sending the same reaction twice: status_code: 400, DuplicateAnnotation.)
    let (idx, mut d) = numbers
        .iter()
        .enumerate()
        .find(|&(_, &x)| x > 0 && x < 100)
        .filter(|&(_, &x)| x % 11 != 0)
        .map(|(idx, &x)| (idx, x))?;

    // Get digits from the number if it exists.
    let mut digits = Vec::new();
    while d > 0 {
        digits.push((d % 10) as u32);
        d /= 10;
    }
    // we don't need reverse() as new reactions appear at the left of message bubble.
    Some((idx, digits))
}
/// Calculate weeks, days, hours and minutes before some time
pub fn calculate_durations(timestamp: Timestamp) -> Vec<i32> {
    let now = Timestamp::now();
    let relative = now.to_zoned(TimeZone::UTC);
    
    // Get span
    let span = timestamp.since(now).unwrap();
    
    // Round for minutes with relative point to count months, too
    // let span = span.round(SpanRound::new().smallest(Unit::Minute).relative(&zdt_now)).unwrap();

    let numbers = vec![
        span.total((Unit::Month, &relative)).unwrap() as i32,
        // span.total((Unit::Week, &relative)).unwrap() as i32, 
        span.total((Unit::Day, &relative)).unwrap() as i32,
        span.total((Unit::Hour, &relative)).unwrap() as i32,
        span.total((Unit::Minute, &relative)).unwrap() as i32
    ];
    // let numbers = numbers.iter().map(|&n| n as i32).collect();
    return numbers;
}
