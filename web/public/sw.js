// 最小 service worker：只为满足「可安装为 App」的条件。
// 不缓存任何内容，所有请求照常走网络，避免更新后显示旧页面。
self.addEventListener('install', () => self.skipWaiting())
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()))
self.addEventListener('fetch', () => {})
