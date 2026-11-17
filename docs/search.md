# Search

`GET /search/?query=...&index=...&numResults=...` returns the digests of
the waves matching a query, newest first unless the query says otherwise.
The same query language is used by the web client, `/notification/`, the
robot API and the agents.

## Token queries

A query is a list of tokens and words. Everything in one group must hold.

| Token | Meaning |
| --- | --- |
| `word`, `"two words"`, `pre*` | Text in the wave (title or blips) |
| `title:word` | Text in the title |
| `in:inbox`, `in:archive`, `in:mute` | The wave's place for you |
| `with:alice@example.com` | `alice` is on the wave (`with:alice` for your domain) |
| `creator:alice@example.com` | `alice` started the wave |
| `id:example.com/w+abc` | That wave |
| `label:7` | You gave the wave label 7 (an ensemble) |
| `after:2026-11-01`, `before:2026-12-01` | Last changed on or after / before that day (UTC) |
| `orderby:datedesc` and the others of Apache Wave | Sort order |

A `-` before a token or word excludes: `relay -in:archive -title:draft`.
Exclusions only look at the parts of a wave you can see, so they never
tell you anything about a private reply you are not on.

`OR` (upper case, unquoted) separates groups; a wave matching any group is
found: `label:7 OR with:bob@example.com relay`. Tokens do not carry over
from one group to the next, so `in:inbox label:7 OR in:inbox relay` needs
`in:inbox` twice.

## SQL mode

A query that starts with `SELECT ` (any case) is run as SQLite SQL. It is
available to signed-in users and to access tokens made with the `sql`
scope (`"scopes": ["read", "sql"]`), not to robots or agents. A server
can turn it off with `search_sql_enabled = false`
(`RESONATOR_SEARCH_SQL_ENABLED`).

The query runs on a private copy of your own data, made for you and
nothing else: there is no table of other users, accounts or deltas, and
no row from a wavelet you are not on. The tables:

| Table | Columns |
| --- | --- |
| `waves` | `wave_id` (`domain/id`), `title`, `creator`, `created`, `last_modified` (ms since the epoch), `box` (`inbox`, `archive`, `mute`) |
| `participants` | `wave_id`, `address` |
| `labels` | `wave_id`, `label` (your labels) |
| `texts` | `wave_id`, `title`, `content` (FTS5: `texts MATCH 'relay'`) |

The first column of the result must be the wave id; the order of the rows
is the order of the result:

```sql
SELECT wave_id FROM waves
WHERE box = 'inbox' AND wave_id IN
  (SELECT wave_id FROM participants WHERE address LIKE '%@acme.com')
ORDER BY last_modified DESC
```

Limits: one `SELECT` statement (no `PRAGMA`, `ATTACH` or writes), 250 ms,
5000 rows, 64 KB of SQL. A query that fails returns no waves, and the
reason in the `X-Resonator-Search-Error` header (percent-encoded UTF-8),
such as `no such column: x`.

## Ensembles

The web client keeps your ensembles (name, colour, query) in a private
wavelet `user+<you>` of the wave `settings+<you>`, so they follow you
between devices. A wave is in an ensemble if you gave it the ensemble's
label (by dragging it there) or if it matches the ensemble's query.
