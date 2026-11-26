-- M4a：告警规则、状态、去重标记与事件。
ALTER TABLE nodes ADD COLUMN alert_offline INTEGER NOT NULL DEFAULT 1;

CREATE TABLE alert_rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    metric TEXT NOT NULL,
    op TEXT NOT NULL,
    threshold REAL NOT NULL,
    duration_secs INTEGER NOT NULL DEFAULT 300,
    cooldown_secs INTEGER NOT NULL DEFAULT 1800,
    all_nodes INTEGER NOT NULL DEFAULT 1,
    probe_task_id INTEGER
);

CREATE TABLE alert_rule_nodes (
    rule_id INTEGER NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
    node_id INTEGER NOT NULL,
    PRIMARY KEY (rule_id, node_id)
);

CREATE TABLE alert_state (
    kind TEXT NOT NULL,
    rule_id INTEGER NOT NULL,
    node_id INTEGER NOT NULL,
    task_id INTEGER NOT NULL,
    status TEXT NOT NULL,
    since_ms INTEGER NOT NULL,
    fired_ms INTEGER,
    notified INTEGER NOT NULL DEFAULT 0,
    value REAL,
    last_notified_ms INTEGER,
    PRIMARY KEY (kind, rule_id, node_id, task_id)
);

CREATE TABLE alert_marks (
    node_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    mark TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (node_id, kind, mark)
);

CREATE TABLE alert_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    node_id INTEGER,
    node_name TEXT,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    sent TEXT NOT NULL
);
CREATE INDEX idx_alert_events_ts ON alert_events(ts_ms);
