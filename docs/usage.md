# Bot's Usage
## Actions Overview
### Create a Reminder
Example commands:
- `/remind 19.08.2026 at 10:00 buy milk`
- `/remind 19/08 pet a cactus` - create a reminder for August 19th of this year at 9am.
- `/remind 19 August 21:30 plant a tree`
- `/remind tomorrow evening be kind with people` - create a reminder with predefined `evening` time.
- `19 feb afternoon to have a fantasy` - create a reminder if the creation of reminders only on command (`on_command` in `config.yml`) is `false`.

> [!WARNING]
> Currently, the American format of writing the month and then the day are not supported, as is the 12-hour system.

### Change the Time Zone
Use the `/tz` command to display the room's time zone - by default this is the time zone from the bot settings. To change it, add [time zone code](https://en.wikipedia.org/wiki/List_of_tz_database_time_zones) to the command, for example: `/tz Europe/Paris`.

## Behaviour Details
### Summary
After the container with bot or bot itself restarts, all reminders are restored from the local database. Reminders that weren't sent are sent to the user or room as a *summary* of missed reminders. Soon, it will be possible to request the summary manually.
### Deletion
After reminders are sent, they are not deleted from the database but marked as sent. To delete all sent reminders, the server administrator must use the `cleanup` command (in development).

> [!IMPORTANT]
> Reminders are stored unencrypted.

### Statuses
Currently, only two statuses are implemented for reminders: pending and sent. Interaction with them is not provided.

### Localization
In languages that have cases and declension, the keywords `morning`, `tomorrow`, etc., are written in the localization file in the declension corresponding to the phrase "remind me at such-and-such a time, in the morning, etc." 

If the translation does not work as expected, adjust it in [your localization file](https://github.com/outarde/reminder-bot/blob/main/docs/configuration.md#using-a-custom-translation-file), separating possible variants with a space. It would also be very helpful if you created an issue for localization misses.

### Time Zone
The time zone is saved as a Matrix custom events (type *state event*) for the entire room (without a *state_key* with the user id), but information about the user is also saved in the settings database. In the future, this will provide customized settings for different users in the same room.

Matrix power levels are not taken into account when setting the time zone.
