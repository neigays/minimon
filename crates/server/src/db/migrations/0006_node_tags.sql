-- 节点标签：规范化后的原始串，如 `1Gbps<yellow>;CN2 GIA<red>`
ALTER TABLE nodes ADD COLUMN tags TEXT NOT NULL DEFAULT '';
