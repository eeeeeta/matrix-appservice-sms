CREATE TABLE messages (
	id SERIAL PRIMARY KEY,
	recipient_id INT REFERENCES recipients ON DELETE CASCADE,
	pdu bytea NOT NULL,
	processing BOOL NOT NULL DEFAULT false,
	failures INT NOT NULL DEFAULT 0
);
