-- M3：流量额度/到期日列与延迟探测表。
ALTER TABLE nodes ADD COLUMN traffic_quota INTEGER;
ALTER TABLE nodes ADD COLUMN traffic_reset_day INTEGER NOT NULL DEFAULT 1;
ALTER TABLE nodes ADD COLUMN traffic_mode TEXT NOT NULL DEFAULT 'sum';
ALTER TABLE nodes ADD COLUMN expire_date TEXT;

CREATE TABLE probe_tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    interval_secs INTEGER NOT NULL DEFAULT 60,
    samples INTEGER NOT NULL,
    timeout_ms INTEGER NOT NULL DEFAULT 5000,
    all_nodes INTEGER NOT NULL DEFAULT 1,
    show_on_card INTEGER NOT NULL DEFAULT 1,
    sort INTEGER NOT NULL DEFAULT 0,
    created_ms INTEGER NOT NULL
);

CREATE TABLE probe_task_nodes (
    task_id INTEGER NOT NULL,
    node_id INTEGER NOT NULL,
    PRIMARY KEY (task_id, node_id)
);

CREATE TABLE probe_raw (
    task_id INTEGER NOT NULL,
    node_id INTEGER NOT NULL,
    ts_ms INTEGER NOT NULL,
    sent INTEGER NOT NULL,
    lost INTEGER NOT NULL,
    avg_ms REAL,
    min_ms REAL,
    max_ms REAL,
    error TEXT,
    PRIMARY KEY (task_id, node_id, ts_ms)
) WITHOUT ROWID;

CREATE TABLE probe_5m (
    task_id INTEGER NOT NULL,
    node_id INTEGER NOT NULL,
    ts_ms INTEGER NOT NULL,
    sent INTEGER NOT NULL,
    lost INTEGER NOT NULL,
    avg_ms REAL,
    min_ms REAL,
    max_ms REAL,
    samples INTEGER NOT NULL,
    PRIMARY KEY (task_id, node_id, ts_ms)
) WITHOUT ROWID;
