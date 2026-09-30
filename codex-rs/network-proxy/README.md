# Managed networking compatibility

This crate retains the configuration, policy, wire types, and inherited-environment
sanitation used by existing Codex consumers. It does not ship a managed network
proxy, a TLS interceptor, or a standalone proxy executable.

Requests for managed proxy enforcement, attachment-owned network policies, remote
proxy launches, or credential brokerage fail closed with an unsupported-capability
error. A requirement that only disables the proxy remains accepted.

Ordinary operating-system sandbox network permissions and application HTTP clients
(including their existing system-proxy support) are unchanged.
