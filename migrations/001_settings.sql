CREATE TABLE IF NOT EXISTS room_settings (
    room_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (room_id, key)
);

CREATE TABLE IF NOT EXISTS user_settings (
    user_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, key)
);

ALTER TABLE reminders ADD COLUMN target_user_id TEXT;
ALTER TABLE reminders ADD COLUMN delegation_kind TEXT NOT NULL DEFAULT 'personal'; 
-- 'personal' | 'delegated_user' | 'delegated_room'

DROP TABLE settings
