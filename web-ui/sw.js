/* rsntr console: install-only service worker. No fetch handler on
   purpose - no caching, no offline behavior; the page talks to the
   server directly, exactly as without the worker. */
self.addEventListener("install", function () { self.skipWaiting(); });
self.addEventListener("activate", function (e) { e.waitUntil(self.clients.claim()); });
