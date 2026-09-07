# Installation and Configuration
## Required Settings
Required settings are stored as environment variables. These are the parameters responsible for authorization. Without these, you can only run commands that do not connect to the Matrix server.

| Variable            | Description                                                                                                   | Example                    |
| :------------------ | :------------------------------------------------------------------------------------------------------------ | :------------------------- |
| `MATRIX_HOMESERVER` | Matrix homeserver address.                                                                                    | `https://matrix.org`       |
| `MATRIX_USERNAME`   | Bot's account username. Create a user via Matrix Authentication Service (MAS): `docker exec matrix-auth mas-cli manage register-user USERNAME --password PASSWORD`                                                                                       | `@reminder-bot:matrix.org` |
| `MATRIX_PASSWORD`   | Bot's account password.                                                                                       | `mypassword`               |
| `MATRIX_TOKEN`      | Authentication token instead of a username and password. Generate a token via [Element Admin](https://github.com/element-hq/element-admin) or other service. | `mpt_mytoken`              |
| `MATRIX_RECOVERY`      | A recovery key that you enter only when you want to verify your account and then keep in a safe place. For more information, see the dedicated [Matrix account page](https://github.com/outarde/reminder-bot/tree/main/docs/matrix.md). | `recovery_key`              |

You can set variables in the [docker-compose.yml](https://github.com/outarde/reminder-bot/blob/main/docker/docker-compose.yml):
```yaml
environment:
  - MATRIX_HOMESERVER=https://matrix.org
  - MATRIX_TOKEN=mpt_mytoken
```
Or in the [.env file](https://github.com/outarde/reminder-bot/blob/main/docker/example.env) in the root of the Docker container folder:
```
MATRIX_HOMESERVER=https://matrix.org
MATRIX_USERNAME=@reminder-bot:matrix.org
MATRIX_PASSWORD=mypassword
```
---
The bot also has an optional variable `MATRIX_DEVICE` that specifies the device name. This name is displayed on the user's device list page and in the admin panel and does not affect the bot's use. Its default value is `reminder-bot-device`.

## Bot’s Optional Settings
Additional settings are stored in the `config.yml`/`config.yaml` file in a folder or volume that you have bound to the `/app/data/reminder_bot` folder inside the container. The repository has default [config.example.yml](https://github.com/outarde/reminder-bot/blob/main/docker/config.example.yml).

<details>
<summary>Interactive configurator</summary>

The interactive configurator walks you through a series of questions to create a settings file and saves it to disk. Run it via the command line:
```
docker run --rm -it -v ~/bot-data:/app/data/reminder_bot ghcr.io/outarde/reminder-bot:latest config-setup
```
The settings file will be saved in `~/bot-data` - in this case, in the folder in the user's home directory.

If you have already created a bot container, run the command where `reminder-bot` is the name of your container:

`docker exec -it reminder-bot config-setup`

Confirm that you want to create or overwrite a settings file.
</details>

| Field             | Description                                                                                                                                                                                          | Default        |
| :---------------- | :--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | :------------- |
| `lang`            | Language for bot commands and messages. See below for a list of available languages. Applies to all users.                                                                                           | `en`           |
| `remind_commands` | Aliases that override the standard bot invocation command. These are formatted as a list. The command for the selected language is available regardless of this variable.                            | `[ "remind" ]` |
| `list_commands`   | Same for the reminders list command. Does not have a localized version.                                                                                                                                                                | `[ "list" ]`   |
| `tz_commands`     | Same for the timezone set up command. Does not have a localized version.                                                                                                                                                                | `[ "tz" ]`     |
| `on_command`      | Activate the bot only when a prefix (slash `/` or exclamation mark `!`) is presented at the beginning of a message. | `true`         |
| `on_command_group`      | The same as above but for rooms with more than two active or invited participants. | `true`         |
| `on_mention`      | Activation of the bot in chats with more than two active or invited participants only when it is mentioned: `@reminder-bot:matrix.org ...`.                                                          | `false`        |
| `quick_remind`      | If enabled, any text sent to the bot, except for other commands, will be treated as a new reminder. The setting does not apply to group rooms to prevent false positives.                                                         | `true`        |
| `remind_undated`      | Create reminders without requiring a date or time. Set the default morning time for today or tomorrow if today is too late. **Caution**: when enabled along with the `quick_remind` option, this will create reminders from any text sent to the bot.                                                          | `false`        |
| `send_reactions`      | The bot will send emoji reactions instead of success messages, but errors will remain in text format.                                                          | `true`        |
| `send_digits_reactions`      | The bot will send reactions in the form of a number of the longest duration before the reminder time. The Matrix doesn't allow two identical reactions to be sent to the same message, so numbers like 11 and 22 are replaced with the timer emoji ⏲️.                                                          | `true`        |
| `tz`         | Default time zone for all rooms on the server. Use [TZ identifier base](https://en.wikipedia.org/wiki/List_of_tz_database_time_zones). | `Europe/Paris`        |
| `morning`         | The time that is considered morning. Please follow the format `%H:%M`, otherwise you will see a general error `Error parsing regex` only when the bot tries to access the variables.                 | `09:00`        |
| `afternoon`       | The time that is considered afternoon.                                                                                                                                                               | `14:00`        |
| `evening`         | The time that is considered evening.                                                                                                                                                                 | `19:00`        |

> [!NOTE]
>  New versions may contain incompatible configuration changes. We'll reflect these in the releases notes.

## Language
### List of Available Languages
**From v0.4.0:**
- `en` English 🇬🇧,
- `de` German 🇩🇪,
- `fr` French 🇫🇷,
- `it` Italian 🇮🇹,
- `es` Spanish 🇪🇸,
- `sv` Swedish, aka IKEAish 🇸🇪,
- `pl` Polish 🇵🇱,
- `cs` Czech 🇨🇿,
- `fi` Finnish 🇫🇮,
- `ja` Japanese 🇯🇵,
- `zh` Chinese Simplified 🇨🇳,
- `ru` Russian 🇷🇺,
- `uk` Ukrainian 🇺🇦.

If you notice an incorrect translation or would like to request an other language, please [report it](https://github.com/outarde/reminder-bot/issues).
### Using a Custom Translation File
A custom translation file is a great way to add a language that isn't yet in the bot, or to customize an existing translation to suit your needs, for a themed homeserver or special occasion 🎃!

**Step one.** Create a `locales` folder in the folder already bound to `/app/data/reminder_bot`.

**Step two.** Create an `app.yml` file inside it. 

**Step three.** Add your translations, checking the keys from the [default localization file](https://github.com/outarde/reminder-bot/blob/main/locales/app.yml) and using the [standard language codes](https://en.wikipedia.org/wiki/List_of_ISO_639_language_codes).

Example:
```yaml
welcome.on_command: 
  en: >
    I'm a reminder bot. Creating a reminder is easy: 
    `!remind %{date} [at] 15:30 <reminder text>`. 
    I'll take care of the rest!
welcome.on_command_off: 
  en: >
    I'm a reminder bot. Creating a reminder is easy: 
    `%{date} [at] 15:30 <reminder text>`. 
    I'll take care of the rest!
reminder.command: 
  en: remind
reminder.saved: 
  en: ⏲️ Remind you on %{date} at %{hour}:%{min}
reminder.list: 
  en: >
    %{text} on %{date} at %{time}
reminder.new: 
  en: >
    🟢 Don't forget: %{text}
reminder.missed: 
  en: |
    ⚠️ You missed something: 
    %{sum}
reminder.error.month: 
  en: The thirteenth month!
reminder.error.time: 
  en: Oh, the times!
reminder.error.past-time: 
  en: Time is in the past, no reminder needed!
reminder.error.summer-time:
  en: This time does not exist in your time zone due to seasonal change.

# We want to use the default values for next keys, 
# so we don't include them in the file.
# months: 
#  en: ...
```
> [!TIP]
>  The `>` symbol means to remove all line breaks, the `|` symbol means to keep line breaks.

The bot will first search for a translation in your file, and then in the standard one.

If you've created a translation file that you'd like to share with the community, please make a pull request to the `/docs/locales` folder of this repository.

## What's Next
Read about the tools for working [with Matrix account](https://github.com/outarde/reminder-bot/tree/main/docs/matrix.md) (including device verification) or skip straight to the page about the intricacies [of using the bot](https://github.com/outarde/reminder-bot/tree/main/docs/usage.md).
