# Security policy

## Supported versions

Only the latest release receives security fixes.

## Reporting a vulnerability

Please **do not open a public issue**. Report it privately through GitHub:
**Security > Report a vulnerability** on this repository
([direct link](https://github.com/aspherex/sonix/security/advisories/new)).

Include the affected version, your OS, what an attacker can do, and steps to reproduce. You should get a first
answer within a week. Fixes are released as soon as they are ready, and you are credited in the advisory unless
you prefer otherwise.

## Scope

In scope:

- The **MCP bridge** (local named pipe / Unix socket, its handshake and token, the playback-control opt-in).
- **Credential handling** (OS keychain use, anything that could leak the server password or tokens into files,
  logs or crash reports).
- **Network handling** towards the Subsonic server (TLS, URL handling, response parsing).
- The **updater** (feed parsing, signature and version verification), when a release channel is configured.

Out of scope: vulnerabilities in the music server itself (report those to Navidrome or the respective project)
and issues that require an attacker who already controls your user account.
