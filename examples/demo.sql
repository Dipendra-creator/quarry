-- Fictional SQLite data for a small, repeatable product demonstration.
-- Run in a new/disposable database; it intentionally refuses existing tables.
PRAGMA foreign_keys = ON;
BEGIN;
CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1))
);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    total REAL NOT NULL,
    created_at TEXT NOT NULL
);
INSERT INTO customers (id, name, email) VALUES
    (1, 'Avery', 'avery@example.com'),
    (2, 'Robin', 'robin@example.com'),
    (3, 'Sam', 'sam@example.com'),
    (4, 'Jordan', 'jordan@example.com');
INSERT INTO orders (id, customer_id, total, created_at) VALUES
    (1, 1, 125.00, date('now', '-3 days')),
    (2, 1, 79.00, date('now', '-30 days')),
    (3, 2, 45.00, date('now', '-95 days')),
    (4, 3, 210.00, date('now', '-12 days'));
COMMIT;

-- Suggested first prompt: Which customers haven't ordered in the last 60 days?
-- Expected customers: Robin and Jordan (who has never ordered).
