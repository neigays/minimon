-- nodes.id 改为 AUTOINCREMENT：删除节点后 id 不再被复用。
CREATE TABLE nodes_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    sort INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0,
    info_json TEXT,
    created_ms INTEGER NOT NULL,
    grp TEXT NOT NULL DEFAULT '',
    region TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    last_ip TEXT,
    last_seen_ms INTEGER
);

INSERT INTO nodes_new (id, name, token_hash, sort, hidden, info_json, created_ms, grp, region, note, last_ip, last_seen_ms)
SELECT id, name, token_hash, sort, hidden, info_json, created_ms, grp, region, note, last_ip, last_seen_ms FROM nodes;

DROP TABLE nodes;
ALTER TABLE nodes_new RENAME TO nodes;
