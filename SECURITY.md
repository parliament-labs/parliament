# Security

Parliament holds database credentials and runs queries on your databases, so security reports are very welcome.

## Reporting a vulnerability

Please report privately, not in a public issue or discussion: on this repository's **Security** tab, choose **Report a vulnerability** ([direct link](https://github.com/parliament-labs/parliament/security/advisories/new)). Say what you found, how to reproduce it, and what it lets an attacker do.

Parliament is maintained in spare time. Expect an acknowledgement within a week, and a fix in the next release for anything confirmed. You'll be credited in the advisory unless you'd rather not be.

## Supported versions

Only the latest release gets fixes. Parliament is at 0.1.x; to upgrade, run the README's install again.

## What Parliament protects against

Parliament is a local tool for one person, so the threats it guards against are the other accounts on the machine, the web pages open in your browser, and the network — not you:

- noctua listens on 127.0.0.1:3000 only.
- Its WebSocket accepts only Parliament's own pages (`localhost` or `127.0.0.1`, ports 3000 and 8080), or a client that sends no `Origin` header, which browsers always send — so that admits only programs on this machine. Its HTTP endpoints have the same allowlist, through CORS.
- Saved connections, passwords included, are kept in `~/.sqlclient/connections.json`, written atomically and readable only by you (0600); a copy others can read is made private when noctua starts. Postgres connections can take their password from `~/.pgpass` instead.
- Stored passwords are never sent to the browser. psql's `\password` hashes the new password (SCRAM-SHA-256) in the browser, so the password itself never crosses the WebSocket.
- The shell history kept in the browser leaves out every line that may hold a secret (any `\set`, anything mentioning a password or secret, a URI with a user part), and noctua's log never carries statement text.
- `\e` opens an editor chosen from noctua's own settings and environment; no command crosses the WebSocket.

**Worth reporting:** anything that lets another local user, a web page, or a remote host read stored credentials, read or change data through your sessions, or run a command — and anything that leaks a secret to disk, to a log, or to the browser's storage.

**Not in scope:** someone already able to act as your user account (they can read your files anyway), running noctua on an address other than loopback (not supported), and vulnerabilities in PostgreSQL or MySQL themselves.
