ALTER TABLE reminders ADD COLUMN schedule_type TEXT NOT NULL DEFAULT 'once';
ALTER TABLE reminders ADD COLUMN schedule TEXT;
