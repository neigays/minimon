-- agent 自查的公网地址与国家；token 原文（安装命令可随时查看，认证仍用 token_hash）
ALTER TABLE nodes ADD COLUMN ipv4 TEXT;
ALTER TABLE nodes ADD COLUMN ipv6 TEXT;
ALTER TABLE nodes ADD COLUMN country TEXT;
ALTER TABLE nodes ADD COLUMN token TEXT;
