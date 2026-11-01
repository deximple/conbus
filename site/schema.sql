-- conbus waitlist. Apply once:
--   wrangler d1 execute conbus-waitlist --remote --file ./schema.sql
-- Only the UTC day is ever stored — no timestamp, IP address or user agent.

CREATE TABLE IF NOT EXISTS clicks (
  day     TEXT NOT NULL,
  variant TEXT NOT NULL,
  count   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (day, variant)
);

CREATE TABLE IF NOT EXISTS signups (
  id         INTEGER PRIMARY KEY,
  day        TEXT NOT NULL,
  email      TEXT NOT NULL UNIQUE,
  team_size  TEXT,
  -- vendors is a JSON array of the checked names
  vendors    TEXT,
  unattended TEXT,
  incident   TEXT,
  variant    TEXT,
  source     TEXT,
  consent    INTEGER NOT NULL
);
