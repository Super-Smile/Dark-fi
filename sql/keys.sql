CREATE TABLE IF NOT EXISTS keys(
	key_id INTEGER PRIMARY KEY NOT NULL,
	public BLOB NOT NULL,
	secret BLOB NOT NULL
);