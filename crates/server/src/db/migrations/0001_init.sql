CREATE TABLE nodes (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    sort INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0,
    info_json TEXT,
    created_ms INTEGER NOT NULL
);

CREATE TABLE metrics_raw (
    node_id INTEGER NOT NULL,
    ts_ms INTEGER NOT NULL,
    cpu REAL,
    mem_used INTEGER,
    swap_used INTEGER,
    disk_used INTEGER,
    disk_total INTEGER,
    net_rx_bytes INTEGER,
    net_tx_bytes INTEGER,
    net_rx_rate INTEGER,
    net_tx_rate INTEGER,
    load1 REAL,
    load5 REAL,
    load15 REAL,
    uptime_secs INTEGER,
    process_count INTEGER,
    tcp_count INTEGER,
    udp_count INTEGER,
    PRIMARY KEY (node_id, ts_ms)
) WITHOUT ROWID;
