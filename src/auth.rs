use std::{
    path::{Path, PathBuf},
};

use matrix_sdk::{
    Client, Error, LoopCtrl,
    authentication::matrix::MatrixSession,
    encryption::{recovery::RecoveryState, EncryptionSettings},
    config::SyncSettings,
    ruma::{
        api::client::filter::FilterDefinition,
        device_id,
    },
};
use jiff::{
    Timestamp
};
use rand::{RngExt, distr::Alphanumeric, rng};
use serde::{Deserialize, Serialize};
use tokio::fs;
use colored::Colorize;

// app crates
use crate::config;

/// Default initial_device_display_name to prevent "magic values" in the code.
pub const DEVICE_NAME: &str = "reminder-bot-device";

/// The data needed to re-build a client.
#[derive(Debug, Serialize, Deserialize)]
struct ClientSession {
    /// The URL of the homeserver of the user.
    homeserver: String,

    /// The path of the database.
    db_path: PathBuf,

    /// The passphrase of the database.
    passphrase: String,
}

/// The full session to persist.
#[derive(Debug, Serialize, Deserialize)]
struct FullSession {
    /// The data to re-build the client.
    client_session: ClientSession,

    /// The Matrix user session.
    user_session: MatrixSession,

    /// The latest sync token.
    ///
    /// It is only needed to persist it when using `Client::sync_once()` and we
    /// want to make our syncs faster by not receiving all the initial sync
    /// again.
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
}


/// This module provides all authentication and recovering functions.
/// Most of the code is taken from the example persist_session and documentation.
/// Will be divided into auth.rs and recovery.rs.

/// Restore a previous session.
pub async fn restore_session(session_file: &Path) -> anyhow::Result<(Client, Option<String>)> {
    println!("Previous session found in '{}'", session_file.to_string_lossy());

    // The session was serialized as JSON in a file.
    let serialized_session = fs::read_to_string(session_file).await?;
    let FullSession { client_session, user_session, sync_token } =
        serde_json::from_str(&serialized_session)?;

    // Build the client with the previous settings from the session.
    let client = Client::builder()
        .homeserver_url(client_session.homeserver)
        .sqlite_store(client_session.db_path, Some(&client_session.passphrase))
        .build()
        .await?;

    println!("Restoring session for {}…", user_session.meta.user_id);

    // Restore the Matrix user session.
    client.restore_session(user_session).await?;

    Ok((client, sync_token))
}

/// Login with a new device.
pub async fn login(
    data_dir: &Path, 
    session_file: &Path,
    config: &config::MatrixConfig,
    config_recovery_key: Option<&str>,
) -> anyhow::Result<Client> {
    println!("No previous session found, logging in…");

    let homeserver = if let Some(hs) = &config.homeserver {
        hs
    }
    else {
        tracing::error!("Empty matrix homeserver URL.");
        return Err(anyhow::anyhow!("Enter your matrix homeserver URL."));
    };

    let (client, client_session) = build_client(data_dir, &homeserver).await?;
    let matrix_auth = client.matrix_auth();

    let device_name = config.device.as_deref().unwrap_or(DEVICE_NAME);

    match match (config.username.as_deref(), config.password.as_deref(), config.token.as_deref()) {
        (Some(u), Some(p), _) => {
            println!("Logging in as {u}...");
            matrix_auth
                .login_username(u, p)
                .initial_device_display_name(device_name)
                .await
        }
        (_, _, Some(t)) => {
            println!("Logging in with token...");
            // token: &str
            matrix_auth
                .login_token(t) 
                .initial_device_display_name(device_name)
                .await
        }
        _ => return Err(anyhow::anyhow!("Enter username and password or token")),
    } {
        Ok(_) => {
            println!("Logged in successfully!");
        }
        Err(error) => {
            println!("Error logging in: {error}");
            println!("Please try again\n");
        }
    }

    // Persist the session to reuse it later.
    // This is not very secure, for simplicity. If the system provides a way of
    // storing secrets securely, it should be used instead.
    // Note that we could also build the user session from the login response.
    let user_session = matrix_auth.session().expect("A logged-in client should have a session");
    let serialized_session =
        serde_json::to_string(&FullSession { client_session, user_session, sync_token: None })?;
    fs::write(session_file, serialized_session).await?;

    println!("Session persisted in {}", session_file.to_string_lossy());

    // After logging in, verifying this session
    let recovery = client.encryption().recovery();
    match recovery.enable().wait_for_backups_to_upload().await {
        Ok(recovery_key) => {
            tracing::info!("Save your recovery key: {}", &recovery_key.bold().bright_blue());
            save_recovery_key(&recovery_key).await?;
        }
        Err(error) => {
            if let Some(key) = config_recovery_key {
                let recovery = client.encryption().recovery();
                match recovery.recover(key).await {
                    Ok(_) => {
                        save_recovery_key(key).await?;
                        tracing::info!("Verification completed successfully from existed recovery key!")
                    },
                    Err(e) => tracing::error!(
                        "Recovery key was presented, but verification failed. \
                        Please try again using CLI. Error: {}", e
                    )
                }
            } else {
                tracing::warn!("Verification failed and recovery key is not presented: {:?}", error);
            }
        }
    }

    Ok(client)
}

/// Build a new client.
async fn build_client(data_dir: &Path, homeserver: &str) -> anyhow::Result<(Client, ClientSession)> {
    let mut rng = rng();

    // Generating a subfolder for the database is not mandatory, but it is useful if
    // you allow several clients to run at the same time. Each one must have a
    // separate database, which is a different folder with the SQLite store.
    let db_subfolder: String =
        (&mut rng).sample_iter(Alphanumeric).take(7).map(char::from).collect();
    let db_path = data_dir.join(db_subfolder);

    // Generate a random passphrase.
    let passphrase: String =
        (&mut rng).sample_iter(Alphanumeric).take(32).map(char::from).collect();

    // Check Homeserver.
    //let homeserver = homeserver.expect("No homeserver URL");
    //let homeserver_url = Url::parse(homeserver);

    println!("\nChecking homeserver…");

    match Client::builder()
        // &str
        .homeserver_url(homeserver)
        .with_encryption_settings(EncryptionSettings {
            auto_enable_cross_signing: true,
            auto_enable_backups: true,
            ..Default::default()
        })
        // We use the SQLite store, which is enabled by default. This is the crucial part to
        // persist the encryption setup.
        // Note that other store backends are available and you can even implement your own.
        .sqlite_store(&db_path, Some(&passphrase))
        .build()
        .await
    {
        // &str to.owned() -> String
        Ok(client) => return Ok((client, ClientSession { homeserver: homeserver.to_owned(), db_path, passphrase })),
        Err(error) => match &error {
            matrix_sdk::ClientBuildError::AutoDiscovery(_)
            | matrix_sdk::ClientBuildError::Url(_)
            | matrix_sdk::ClientBuildError::Http(_) => {
                println!("Error checking the homeserver: {error}");
                println!("Please try again\n");
                Err(anyhow::anyhow!("Homeserver error: {}", error))
            }
            _ => {
                // Forward other errors, it's unlikely we can retry with a different outcome.
                return Err(error.into());
            }
        },
    }
}

/// Setup the client
pub async fn get_sync(
    client: &Client,
    initial_sync_token: &Option<String>,
    session_file: &Path,
) -> anyhow::Result<SyncSettings> {
    println!("Launching a first sync to ignore past messages…");

    // Enable room members lazy-loading, it will speed up the initial sync a lot
    // with accounts in lots of rooms.
    // See <https://spec.matrix.org/v1.6/client-server-api/#lazy-loading-room-members>.
    let filter = FilterDefinition::with_lazy_loading();

    let mut sync_settings = SyncSettings::default().filter(filter.into());

    // We restore the sync where we left.
    // This is not necessary when not using `sync_once`. The other sync methods get
    // the sync token from the store.
    if let Some(sync_token) = initial_sync_token {
        sync_settings = sync_settings.token(sync_token);
    }

    // Let's ignore messages before the program was launched.
    // This is a loop in case the initial sync is longer than our timeout. The
    // server should cache the response and it will ultimately take less time to
    // receive.
    loop {
        match client.sync_once(sync_settings.clone()).await {
            Ok(response) => {
                // This is the last time we need to provide this token, the sync method after
                // will handle it on its own.
                sync_settings = sync_settings.token(response.next_batch.clone());
                persist_sync_token(session_file, response.next_batch).await?;
                break;
            }
            Err(error) => {
                println!("An error occurred during initial sync: {error}");
                println!("Trying again…");
            }
        }
    }

    println!("{}", "The client is ready!".bold().green());

    // Now that we've synced, let's attach a handler for incoming room messages.
    //client.add_event_handler(on_room_message);

    Ok(sync_settings)
}

/// Starting sync loop
pub async fn sync(
    client: Client,
    sync_settings: SyncSettings,
    session_file: &Path,
) -> anyhow::Result<()> {

    println!("Listening to new messages…");

    // Now that we've synced, let's attach a handler for incoming room messages.
    //client.add_event_handler(on_room_message);

    // This loops until we kill the program or an error happens.
    client
        .sync_with_result_callback(sync_settings, |sync_result| async move {
            let response = sync_result?;

            // We persist the token each time to be able to restore our session
            persist_sync_token(session_file, response.next_batch)
                .await
                .map_err(|err| Error::UnknownError(err.into()))?;

            Ok(LoopCtrl::Continue)
        })
        .await?;

    Ok(())
}

/// Persist the sync token for a future session.
/// Note that this is needed only when using `sync_once`. Other sync methods get
/// the sync token from the store.
async fn persist_sync_token(session_file: &Path, sync_token: String) -> anyhow::Result<()> {
    let serialized_session = fs::read_to_string(session_file).await?;
    let mut full_session: FullSession = serde_json::from_str(&serialized_session)?;

    full_session.sync_token = Some(sync_token);
    let serialized_session = serde_json::to_string(&full_session)?;
    fs::write(session_file, serialized_session).await?;

    Ok(())
}


// === RECOVERY MODULE ===

/// Recover current device with recovery key
pub async fn recover_device(client: Client, recovery_key: &str) -> anyhow::Result<()> {
    tracing::info!("Verification process...");

    //let recovery_key: &str =;

    let recovery = client.encryption().recovery();

    match recovery.recover(recovery_key).await {
        Ok(_) => {
            save_recovery_key(recovery_key).await?;
            tracing::info!("Verification completed successfully!")
        },
        Err(e) => tracing::error!("The recovery key is invalid: {}", e)
    }

    match recovery.state() {
        RecoveryState::Enabled => println!("Successfully recovered all the E2EE secrets."),
        RecoveryState::Disabled => println!("Error recovering, recovery is disabled."),
        RecoveryState::Incomplete => println!("Couldn't recover all E2EE secrets."),
        _ => unreachable!("We should know our recovery state by now"),
    };

    Ok(())
}

/// Recover current device with recovery key using a method that also restores
/// the integrity of the backup. Can be used if verification already exists.
pub async fn recover_device_with_fix(client: Client, recovery_key: &str) -> anyhow::Result<()> {
    tracing::info!("Verification with fixing backup errors process...");

    let recovery = client.encryption().recovery();

    match recovery.recover_and_fix_backup(recovery_key).await {
        Ok(_) => {
            save_recovery_key(recovery_key).await?;
            tracing::info!("Verification and backup fixing completed successfully!")
        },
        Err(e) => tracing::error!("The recovery key is invalid: {}", e)
    }

    match recovery.state() {
        RecoveryState::Enabled => println!("Successfully recovered all the E2EE secrets."),
        RecoveryState::Disabled => println!("Error recovering, recovery is disabled."),
        RecoveryState::Incomplete => println!("Couldn't recover all E2EE secrets."),
        _ => unreachable!("We should know our recovery state by now"),
    };

    Ok(())
}

/// Print list of account devices with names and verification stasues
pub async fn devices_list(client: Client) -> anyhow::Result<()> {
    //<&UserId>::try_from(&config.username.as_deref()).unwrap()
    let own_user_id = client.user_id().unwrap();
    let devices = client.encryption().get_user_devices(own_user_id).await?;
    let own_device = client.encryption().get_own_device().await.unwrap_or(None).unwrap();
    let own_device_id = own_device.device_id();

    println!("Device {} is now running. List of devices for user {}:\n", 
        own_device_id.to_string().bold(), 
        own_user_id.to_string().bold()
    );

    for device in devices.devices() {
        println!(
            "Device {} with session name {} {}",
            device.device_id().to_string().bold(),
            device.display_name().unwrap().to_string().bold(),
            if device.is_verified() {"is verified".green()} else {"is not verified".yellow()}
        );

        if device.is_verified_with_cross_signing() {
            println!(
                "{}\n", "This device is verified with cross-signing".green()
            );
        } else {
            println!(
                "{}\n", "This device is not verified with cross-signing".yellow()
            );
        }

        // TODO: time device was added
        /*
        let millis: i64 = i64::from(device.first_time_seen_ts().get());

        //
        let secs = millis / 1000;
        let nsecs = ((millis % 1000) * 1_000_000) as u32;
        
        //
        let datetime_local: DateTime<Local> = DateTime::from_timestamp(secs, nsecs)
            .unwrap()
            .with_timezone(&Local);

        println!("Device was registred: {}\n", datetime_local.to_string());
        */
    }

    println!("\
        Use command reminder-bot verify-device <device-id> to verify some of your own device \
        and command reminder-bot recover <recovery-key> for your current bot's device to recover your \
        cross-signing key on which depends device verification\
    ");

    Ok(())
}

/// Verify some of your devices by device ID.
/// To verify current device use recover_device methdod.
pub async fn verify_some_device(client: Client, device_id: &str) -> anyhow::Result<()> {
    //<&UserId>::try_from(&config.username.as_deref()).unwrap()
    let own_user_id = client.user_id().unwrap();
    let device = client.encryption().get_device(own_user_id, device_id!(device_id)).await?;

    if let Some(device) = device {
        // TODO: check errors?
        device.verify().await?;
        tracing::info!("{:?}", device.is_verified());
    }

    Ok(())
}

// Useful for passphrases-type key that is not used in current bot implementation.
/*
pub async fn get_key_from_store(client: Client, recovery_key_or_passphrase: &str) -> anyhow::Result<()> {
    let secret_store = client
    .encryption()
    .secret_storage()
    .open_secret_store(recovery_key_or_passphrase)
    .await?;
    println!("Your recovery key is: {}", secret_store.secret_storage_key().bold());

    save_recovery_key(recovery_key_or_passphrase).await?;

    Ok(())
}
*/

/// Rotate the secret storage key and re-upload all the secrets to the SecretStore
pub async fn reset_recovery(client: Client) -> anyhow::Result<()> {
    let recovery = client.encryption().recovery();

    match recovery.reset_key().await {
        Ok(new_recovery_key) => {
            save_recovery_key(&new_recovery_key).await?;
            tracing::info!("You new recovery key is {}", new_recovery_key.bold().bright_blue());
        }
        Err(error) => {
            tracing::error!("Can not verify device with this key");
            // RecoveryError
            return Err(error.into())
        }
    }

    match recovery.state() {
        RecoveryState::Enabled => println!("Successfully recovered all the E2EE secrets."),
        RecoveryState::Disabled => println!("Error recovering, recovery is disabled."),
        RecoveryState::Incomplete => println!("Couldn't recover all E2EE secrets."),
        _ => unreachable!("We should know our recovery state by now"),
    };

    Ok(())
}

/// Reset the recovery key but first import all the secrets from secret storage
pub async fn reset_recovery_with_backup(
    client: Client,
    //cli_key: Option<&str>,
    //auth_config: &config::MatrixConfig, 
    //rec_config: &Option<config::RecoveryConfig>,
    recovery_key: &str,
) -> anyhow::Result<()> {
    let recovery = client.encryption().recovery();

    //let recovery_key = pick_recovery_key(cli_key, auth_config, rec_config).await?;

    match recovery.recover_and_reset(recovery_key).await {
        Ok(new_recovery_key) => {
            save_recovery_key(&new_recovery_key).await?;
            tracing::info!("You new recovery key is {}", new_recovery_key.bold().bright_blue());
        }
        Err(error) => {
            tracing::error!("Can not verify device with this key");
            // RecoveryError type of Error
            return Err(error.into())
        }
    }

    match recovery.state() {
        RecoveryState::Enabled => println!("Successfully recovered all the E2EE secrets."),
        RecoveryState::Disabled => println!("Error recovering, recovery is disabled."),
        RecoveryState::Incomplete => println!("Couldn't recover all E2EE secrets."),
        _ => unreachable!("We should know our recovery state by now"),
    };

    Ok(())
}

/// Save recovery key to recovery.json.
// TODO: reshape to AppConfig structure
async fn save_recovery_key(recovery_key: &str) -> anyhow::Result<()> {
    let data = config::RecoveryConfig {
        recovery_key: recovery_key.to_string(),
        created_at: Timestamp::now().to_string(),
    };

    let data_dir = dirs::data_dir().expect("No data_dir directory found").join(super::APP_FOLDER);
    let recovery_file = data_dir.join("recovery.json");

    let recovery_data = serde_json::to_string(&data)?;
    fs::write(recovery_file, recovery_data).await?;

    Ok(())
}

/// Checking AppConfig with preloaded .env and recovery.json 
/// and additional first-priority key (as planned, from CLI in most cases).
pub async fn pick_recovery_key(
    cli_key: Option<&str>,
    auth_config: &config::MatrixConfig, 
    rec_config: &Option<config::RecoveryConfig>,
    //sources: [Option<&str>;3]
) -> Option<String> {
    /*
    let recovery_key = auth_config.recovery.as_deref()
        .or_else(|| rec_config.as_ref().map(|rc| rc.recovery_key.as_str()))
        .or_else(|| cli_key.copied());
    */

    // One iterator with each value as Option<&str>
    let sources = [
        cli_key,
        auth_config.recovery.as_deref(),
        rec_config.as_ref().map(|rc| rc.recovery_key.as_str()),
    ];

    // Find first Some
    let recovery_key = sources.into_iter().find_map(|opt| opt);

    match recovery_key {
        Some(key) => {
            return Some(key.to_owned())
        }
        None => {
            //return Err(anyhow::anyhow!("No recovery key found"))
            return None
        }
    }
}

/// Printing recovery key from the file
pub async fn recall_recovery_key() -> anyhow::Result<()> {
    let data_dir = dirs::data_dir().expect("No data_dir directory found").join(super::APP_FOLDER);
    let recovery_file = data_dir.join("recovery.json");

    if recovery_file.exists() {
        // The session was serialized as JSON in a file.
        let serialized_recovery_file = fs::read_to_string(recovery_file).await?;
        let data: config::RecoveryConfig = serde_json::from_str(&serialized_recovery_file)?;
        println!("You recovery key is {}, created at {}", data.recovery_key.bold().bright_blue(), data.created_at);
    } else {
        println!("You don't have active recovery key");
    }

    println!("\
        If you have recovery key and your device is not verified you should provide \
        active key via reminder-bot recover <recovery-key> or reset it via \
        reminder-bot reset-recovery-key.");

    Ok(())
}