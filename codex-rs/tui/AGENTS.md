# Thin frontend

The TUI observes and presents server-owned conversations. Attaching, detaching,
reconnecting, or reloading a renderer must not change agent capabilities or
whether an idle, unloaded, or stopped conversation can receive work.

Thread coordination and durable delivery belong to the app server. A new task
must never wait for frontend registration. A missing frontend may leave a
legitimate approval pending; it must never imply approval.
