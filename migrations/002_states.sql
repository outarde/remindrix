CREATE TABLE IF NOT EXISTS message_states (
    event_id   TEXT NOT NULL,
    room_id    TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (event_id, room_id, key)
);
