ALTER TABLE nodes ADD COLUMN grp TEXT NOT NULL DEFAULT '';
ALTER TABLE nodes ADD COLUMN region TEXT NOT NULL DEFAULT '';
ALTER TABLE nodes ADD COLUMN note TEXT NOT NULL DEFAULT '';
ALTER TABLE nodes ADD COLUMN last_ip TEXT;
ALTER TABLE nodes ADD COLUMN last_seen_ms INTEGER;

CREATE TABLE metrics_1m (
    node_id INTEGER NOT NULL,
    ts_ms INTEGER NOT NULL,
    samples INTEGER NOT NULL,
    cpu REAL,
    mem_used REAL,
    swap_used REAL,
    disk_used REAL,
    disk_total REAL,
    net_rx_rate REAL,
    net_tx_rate REAL,
    load1 REAL,
    load5 REAL,
    load15 REAL,
    process_count REAL,
    tcp_count REAL,
    udp_count REAL,
    cpu_max REAL,
    mem_used_max INTEGER,
    net_rx_rate_max INTEGER,
    net_tx_rate_max INTEGER,
    net_rx_delta INTEGER,
    net_tx_delta INTEGER,
    net_rx_bytes INTEGER,
    net_tx_bytes INTEGER,
    uptime_secs INTEGER,
    PRIMARY KEY (node_id, ts_ms)
) WITHOUT ROWID;

CREATE TABLE metrics_15m (
    node_id INTEGER NOT NULL,
    ts_ms INTEGER NOT NULL,
    samples INTEGER NOT NULL,
    cpu REAL,
    mem_used REAL,
    swap_used REAL,
    disk_used REAL,
    disk_total REAL,
    net_rx_rate REAL,
    net_tx_rate REAL,
    load1 REAL,
    load5 REAL,
    load15 REAL,
    process_count REAL,
    tcp_count REAL,
    udp_count REAL,
    cpu_max REAL,
    mem_used_max INTEGER,
    net_rx_rate_max INTEGER,
    net_tx_rate_max INTEGER,
    net_rx_delta INTEGER,
    net_tx_delta INTEGER,
    net_rx_bytes INTEGER,
    net_tx_bytes INTEGER,
    uptime_secs INTEGER,
    PRIMARY KEY (node_id, ts_ms)
) WITHOUT ROWID;

CREATE TABLE admin (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    username TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    updated_ms INTEGER NOT NULL
);

CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    created_ms INTEGER NOT NULL,
    expires_ms INTEGER NOT NULL
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
