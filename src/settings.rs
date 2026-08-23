use std::{
    sync::Arc
};
use anyhow::{Result, Context, anyhow};
use matrix_sdk::{
	deserialized_responses::SyncOrStrippedState,
	Room,
	ruma::events::{
		EmptyStateKey, macros::EventContent, 
		room::message::{RoomMessageEventContent}
	}
};
use serde::{Deserialize, Serialize};
// use tokio_rusqlite::Connection;
use chrono_tz::Tz;

use crate::handlers::I18nManager;

#[derive(Clone, Debug, Deserialize, Serialize, EventContent)]
#[ruma_event(type = "com.reminder-bot.room_timezone", kind = State, state_key_type = EmptyStateKey)]
pub struct RoomTimezoneContent {
    pub timezone: String,
}

/// Settings for each bot activation (command context).
#[derive(Debug)]
pub struct SettingsManager {
	// db: Arc<Connection>,
    pub room_tz: Tz,
    pub room_lang: String
}

impl SettingsManager {
    pub async fn new(room: &Room, ctx: &Arc<super::BotContext>) -> Self {
    	/*
        let mut settings = Self {
            room_tz: Tz::UTC,
            room_lang: ctx.bot_config.lang
        };

        settings.fetch_room_tz().await;
        settings
        */

        let room_tz = Self::fetch_room_tz(room, ctx).await;
    	Self { room_tz, room_lang: ctx.bot_config.lang.clone() }
	}

	/// Get timezone for the room.
	pub async fn fetch_room_tz(room: &Room, ctx: &Arc<super::BotContext>) -> Tz {
		let default_tz = match parse_tz(&ctx.bot_config.tz) {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!("Default timezone from config.yaml is incorrect: {}", err);
				super::config::DEFAULT_TZ.parse::<Tz>().unwrap()
			}
		};

	    // Raw JSON: Option<Raw<StateEvent<C>>>
	    if let Ok(Some(raw)) = room.get_state_event_static::<RoomTimezoneContent>().await {
	        // Pattern matching to Sync variant, not Stripped. SyncStateEvent
	        // https://docs.rs/matrix-sdk/latest/matrix_sdk/deserialized_responses/enum.SyncOrStrippedState.html
	        // Instead of pattern matching we can use Ok(state) = raw.deserialize(), 
	        // where state: SyncOrStrippedState<RoomTimezoneContent> and then
	        // as_sync() to get SyncStateEvent.
	        if let Ok(SyncOrStrippedState::Sync(sync_event)) = raw.deserialize() {
	            // Check if it is not Redacted
	            // https://docs.rs/ruma-events/0.34.0/ruma_events/enum.SyncStateEvent.html
	            if let Some(original) = sync_event.as_original() {
	                parse_tz(&original.content.timezone).unwrap_or(default_tz)
	            } else {
	                tracing::warn!("Redacted timezone can not be viewed.");
	                return default_tz;
	            }
	        } else { return default_tz; }
	    } else {
	    	return default_tz;
	    }
	}

	/// Set timezone for the room using Matrix custom events.
	pub async fn set_room_tz(
		// &mut self,
		self,
		room: &Room,
		_ctx: &Arc<super::BotContext>,
		tz: Tz,
	) -> Result<()> {
		// We can update it if we'll create it mutable in CommandContext,
		// but it isn't necessarily now.
		//self.room_tz = tz;

		let content = RoomTimezoneContent {
		    timezone: tz.to_string(),
		};

		// let state_key = client.user_id().unwrap().to_string(); 
		room.send_state_event(content).await?;

		Ok(())
	}

    /*
    /// Universal get
    pub async fn get_room_setting<T>(&self, room: &Room, default: T) -> T 
    where T: DeserializeOwned + Clone {
        // State event -> default
        return;
    }

    /// Univeral set
    pub async fn set_room_setting<T>(&self, room: &Room, content: T) -> Result<()> 
    where T: EventContent {
        // room.send_state_event(content).await.map_err(|e| e.into())
        return;
    }
    */
}

/// Get timezone for the room.
pub async fn get_room_tz(
	room: Room,
	ctx: &Arc<super::BotContext>,
) -> Tz {
	let default_tz = match parse_tz(&ctx.bot_config.tz) {
		Ok(t) => t,
		Err(err) => {
			tracing::warn!("Default timezone from config.yaml is incorrect: {}", err);
			super::config::DEFAULT_TZ.parse::<Tz>().unwrap()
		}
	};

    // Raw JSON: Option<Raw<StateEvent<C>>>
    if let Ok(Some(raw)) = room.get_state_event_static::<RoomTimezoneContent>().await {
        // Pattern matching to Sync variant, not Stripped. SyncStateEvent
        // https://docs.rs/matrix-sdk/latest/matrix_sdk/deserialized_responses/enum.SyncOrStrippedState.html
        // Instead of pattern matching we can use Ok(state) = raw.deserialize(), 
        // where state: SyncOrStrippedState<RoomTimezoneContent> and then
        // as_sync() to get SyncStateEvent.
        if let Ok(SyncOrStrippedState::Sync(sync_event)) = raw.deserialize() {
            // Check if it is not Redacted
            // https://docs.rs/ruma-events/0.34.0/ruma_events/enum.SyncStateEvent.html
            if let Some(original) = sync_event.as_original() {
                parse_tz(&original.content.timezone).unwrap_or(default_tz)
            } else {
                tracing::warn!("Redacted timezone can not be viewed.");
                return default_tz;
            }
        } else { return default_tz; }
    } else {
    	return default_tz;
    }
}

/// Set timezone for the room using Matrix custom events.
pub async fn set_room_tz(
	room: Room,
	_ctx: Arc<super::BotContext>,
	tz: Tz,
) -> Result<()> {
	let content = RoomTimezoneContent {
	    timezone: tz.to_string(),
	};

	// let state_key = client.user_id().unwrap().to_string(); 

	room.send_state_event(content).await?;

	Ok(())
}

/// Parse user input to Tz
pub fn parse_tz(tz_str: &str) -> Result<Tz> {
    tz_str.parse::<Tz>().with_context(|| format!("Invalid user timezone: {tz_str:?}"))
}
