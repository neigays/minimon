// 最小 service worker：不缓存、不拦截任何请求（没有 fetch 监听），所有请求照常走网络。
// 以后若要移除：改为发布一个会自行 unregister 的 sw.js，不要直接删除本文件
// （删除后 /sw.js 会回退成 index.html，浏览器更新检查失败，旧 worker 会一直留着）。
self.addEventListener('install', () => self.skipWaiting())
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()))
