# Matrix Server Operations
## Matrix Verification
Without verification, every bot message will be marked with an exclamation point in most clients.

<p>
	<img src=«/docs/assets/ElementX-Screenshot1.jpg" alt="ElementX Screenshot" width="480px">
</p>

For example, the Element X will warn: 
>Encrypted by a device not verified by its owner.

Users will also receive a warning before sending their first message to the bot.
### Enabling a New Account
The easiest way is to create a new account for the bot. The account will be backed up and verified automatically. You'll then see the *recovery key* in the bot logs, which you'll need to save. The key will also be saved in the `recovery.json` file in the bot's session folder (which should have been forwarded to the host in step 1 of [Quick Start](https://github.com/outarde/reminder-bot#quick-start)).

> [!IMPORTANT]
> Keep your recovery key in a safe place!

You can delete `recovery.json` after setup. It is created only to ensure that you do not lose the recovery key, which cannot be obtained without resetting the user’s crypto identity on the Matrix Authentication Service side.
### Enabling a Previously Used Account
To verify a new device, you will need a *recovery key* you received when logging in through another device or new matrix account. Recovery by *passphrase* is not supported and will likely never be supported, as it encrypts the same recovery key.
#### Step 1. Set Your Recovery Key
First, pass the recovery key. Here are the methods for passing the recovery key, in descending order of priority:
1. Write it as a command flag: `recover --recovery-key=your-recovery-key`.
2. Write the recovery key in the `.env` file: `MATRIX_RECOVERY=your-recovery-key`.
3. Move the `recovery.json` file to the bot session folder if you have already run the container on another machine and obtained a recovery key or recovered your account using it.
#### Step 2. Run the Command
Then run the recovery command. In `docker-compose.yml` add:
```yaml
command: ["recover"]
```
Or, if you want to specify the key directly:
```yaml
command: ["recover", "--recovery-key", "your-recovery-key"]
```

If verification is successful, you will see a corresponding message in the logs and the recovery key will be written to the `recovery.json` file. After this, remove `command` from `docker-compose.yml` and restart the bot.

## Verification and Recovery Options
### Verification with Re-creation of Backup
You can use the `recover` command with the `--fix-backup` flag to automatically create a new backup if the previous one is missing chat encryption keys (key backup), as described in the [Matrix Rust SDK documentation](https://docs.rs/matrix-sdk/latest/matrix_sdk/encryption/recovery/struct.Recovery.html#method.recover_and_fix_backup). This is likely useful if the bot started some chats on a new device before verification. The recovery key will also be required. Remove the `--recovery-key` or `-r` flag from the commands below if you don't need to pass the key directly.

Example for Docker:
```yaml
command: ["recover", "--recovery-key=your-recovery-key", "--fix-backup"]
```
Same as:
```yaml
command: recover -r your-recovery-key --fix-backup
```
Or in a list:
```yaml
command:
- recover
- -r
- your-recovery-key
- --fix-backup
```
## Verify Your Other Devices
>[!NOTE]
>This section is under construction but the bot can verify your other devices by the command `verify-some-device`. Use `devices-list` to show devices of the bot account.