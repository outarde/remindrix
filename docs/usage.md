# Bot's Usage
## Actions Overview
### Create a Reminder in a Natural Mode
Example commands:
- `/remind 19.08.2026 at 10:00 buy milk`
- `/remind 19/08 pet a cactus` - create a reminder for August 19th of this year at default `morning` time.
- `/remind 19 August 21:30 plant a tree`
- `remind tomorrow evening be kind with people` - create a reminder with predefined `evening` time. The slash is not used if the option `on_command` is disabled in the settings.
- `19 feb afternoon to have a fantasy` - create a reminder if the `quick_remind` option in the settings is on.
- `/errinern einen Stuhl reparieren` - creates a reminder for the localized (German) command, if that language is selected, for default `morning` time this or the next day, since neither time nor date is specified and, as expected, `remind_undated` is turned on in the settings.

> [!WARNING]
> Currently, the American format of writing the month and then the day are not supported, as is the 12-hour system without am/pm labels.

### Change the Language
You can change the room language via the settings:

- `/settings lang` displays the available languages ​​(including those loaded via your localization file)
- `/settings lang fr` sets the language using its code (in this case, French)

### Change Default Times
You can also change the default time settings. These are the times used when no specific time is specified, as well as when you type `morning`, `afternoon`, or `evening` (including in the language set for the room).

- `/settings time` displays the current values.
- `/settings time --default 11:00 --evening 22:00` updates the values ​​(in this case, for the default time and evening).

### Change the Time Zone
Use the `/settings tz` command to display the room's time zone - by default this is the time zone from the bot settings. To change it, add [time zone code](https://en.wikipedia.org/wiki/List_of_tz_database_time_zones) to the command, for example: `/tz Europe/Paris`.

## Behaviour Details
### Summary
After the container with bot or bot itself restarts, all reminders are restored from the local database. Reminders that weren't sent are sent to the user or room as a *summary* of missed reminders. Soon, it will be possible to request the summary manually.
### Deletion
After reminders are sent, they are not deleted from the database but marked as sent. To delete all sent reminders, the server administrator must use the `cleanup` command (in development).

> [!IMPORTANT]
> Reminders are stored unencrypted.

### Statuses
Currently, only two statuses are implemented for reminders: pending and sent. Interaction with them is not provided.

## Rooms and Users Settings
### Settings Distribution
Room settings (configured individually for each room where the bot is present, or using values ​​from the config):
- language
- time zone

User settings (apply to the same user across all rooms):
- all default time values

### Localization
In languages that have cases and declension, the keywords `morning`, `tomorrow`, etc., are written in the localization file in the declension corresponding to the phrase "remind me at such-and-such a time, in the morning, etc." 

If the translation does not work as expected, adjust it in [your localization file](https://github.com/outarde/reminder-bot/blob/main/docs/configuration.md#using-a-custom-translation-file), separating possible variants with a space. It would also be very helpful if you created an issue for localization misses.

### Time Zone
The time zone is saved as a Matrix custom events (type *state event*) for the entire room (without a *state_key* with the user id), but information about the user is also saved in the settings database. In the future, this will provide customized settings for different users in the same room.

Matrix power levels are not taken into account when setting the time zone.

## CLI Mode
CLI (Command Lined Interface) or Pro mode allows you to create reminders using syntax similar to that used in the terminal. This mode avoids natural language ambiguities and implements advanced features that, if added to the *natural mode*, would make it too schematic and force users to guess the correct word order.

You don't need to enable this mode specifically: the bot tries to recognize any command in this mode and if it fails, it switches to the *natural mode*.

### Arguments and parameters (flags) for `remind` command

| Value | Description | Required |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `text` | Reminder text. | Yes. |
| `--date` | Reminder date as numbers, without spaces. Supported characters as separators: `.`, `/`, `-`. A day without a month or year can be specified. | No. Overridden by the values below. If no date is specified, today's day is used. |
| `-d`, `--day` | Day as a number. | No. If only a day is specified without a month, the current month is used. If the specified day has already passed in the current month, the next month is used. |
| `-m`, `--month` | Month as a number. | No. |
| `-y`, `--year` | Year as a number. | No. |
| `-t, --time` | Time as a number, without spaces. The colon and dot characters (`:`, `.`) are supported as separators. You can specify am/pm, but only without dots between the letters. | No. Overridden by the values below. If time is not specified, default time from the settings is used. If the date was also specified automatically and the reminder time is in the past, the reminder will be moved forward one day. |
| `--hour` | Hour as a number. | No. |
| `--min` | Minutes as a number. If only minutes are specified, the current or next hour will be used. | No. |
| `--room` | The room to delegate the reminder to, in the `!unique_room_code:homeserver_url` format. You can get it from the *share* in the Element X client. | No. |
| `--user` | The user to delegate the reminder to, in the `@user:homeserver_url` format. You can even mention yourself so that a mention notification appears in the group chat. This option does not search for a room containing the specified user! | No. |
| `-i`, `--interval` | If the flag is specified, the entered date and time will be used as an interval. | No, defaults to `false`. |
>[!IMPORTANT]
>Use the short form of parameters only where they are specified in the short form in the table. Time parameters do not have a short form because their first letter would either match the date parameters or the system help command `-h`.

**Example commands**:
- `remind --date=30.11.2026 make hot chocolate`
- `remind -t 8pm take a bath`
- `remind -d 1 -m 11 --time 00:00 Halloween` - creates a reminder on November 1st at midnight.
- `remind --hour 1 -i Check the pie in the oven` - creates a reminder one hour from the current time.
- `remind --time 00:30 -i take a hot bath` or `remind --min 30 -i take a hot bath` - remind in half an hour.
- `remind --user=@reminder-bot:matrix.org call my name`

### Time and Delegation
If you delegate a reminder to another room, the reminder will use that room's time zone. Default time settings cannot be used in this case, as it is unknown which specific user they would apply to. Even if you specify a user via the `--user` parameter, the Remindrix-bot will use your `default_time` rather than the one saved for that user.

### User Delegation
This feature is designed to mention the user to whom the reminder is delegated when the notification appears, rather than to search for that user's personal chat with the bot. The idea of ​​an "address book" is being considered, but for now, it seems too cumbersome for a messenger interface.
