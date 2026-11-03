// adash service worker: caches the app shell only (API is always network), shows Web Push notifications.
const SHELL = "adash-shell-v1";
const SHELL_FILES = ["/", "/app.js", "/style.css", "/manifest.webmanifest", "/vendor/markdown-it.umd.min.js", "/vendor/katex.min.js", "/vendor/katex.min.css", "/vendor/texmath.js", "/icons/icon-192.png"];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(SHELL).then((c) => c.addAll(SHELL_FILES)).then(() => self.skipWaiting()));
});
self.addEventListener("activate", (e) => {
  e.waitUntil(caches.keys().then((ks) => Promise.all(ks.filter((k) => k !== SHELL).map((k) => caches.delete(k)))).then(() => self.clients.claim()));
});
// Shell: network first (fresh UI after a rebuild), cache as offline fallback. /api/*: never cached.
self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.origin !== self.location.origin || url.pathname.startsWith("/api/")) return;
  e.respondWith(fetch(e.request).then((r) => {
    if (r.ok && SHELL_FILES.includes(url.pathname)) { const copy = r.clone(); caches.open(SHELL).then((c) => c.put(e.request, copy)); }
    return r;
  }).catch(() => caches.match(e.request).then((r) => r || caches.match("/"))));
});

// Push payload: {title, body, agent?, tag?}. Clicking opens that agent's chat.
self.addEventListener("push", (e) => {
  let d = {};
  try { d = e.data ? e.data.json() : {}; } catch { d = { title: "adash", body: e.data ? e.data.text() : "" }; }
  e.waitUntil(self.registration.showNotification(d.title || "adash", {
    body: d.body || "", tag: d.tag || d.agent || undefined, icon: "/icons/icon-192.png", badge: "/icons/maskable-192.png",
    data: { url: d.agent ? `/?agent=${encodeURIComponent(d.agent)}` : "/" },
  }));
});
self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  const url = e.notification.data?.url || "/";
  e.waitUntil(self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((cs) => {
    for (const c of cs) if (new URL(c.url).origin === self.location.origin) { c.postMessage({ open: url }); return c.focus(); }
    return self.clients.openWindow(url);
  }));
});
