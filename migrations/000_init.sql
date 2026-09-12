CREATE TABLE IF NOT EXISTS reminders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    room_id TEXT NOT NULL,
    text TEXT NOT NULL,
    target_time TEXT NOT NULL,
    utc_time TEXT NOT NULL,
    tz TEXT NOT NULL,
    created_at TEXT DEFAULT (datetime('now')),
    created_by TEXT NOT NULL,
    status INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS settings (
    room_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (room_id, user_id, key)
);

CREATE INDEX IF NOT EXISTS idx_reminders_status ON reminders(status);
