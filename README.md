# careeraxis-mcp

MCP server + agent-first CLI for **NTU CareerAxis** (careeraxis.ntu.edu.sg, a CareerHub instance).
One Rust binary: with no arguments it speaks MCP over stdio; with a command it's a CLI.
Built in the style of [ntulearn-mcp](../ntulearn-mcp).

## Setup

```bash
cargo build --release                       # target/release/careeraxis-mcp (~4 MB)
target/release/careeraxis-mcp setup         # opens a browser window: sign in to NTU once
target/release/careeraxis-mcp status        # logged_in: true
```

Register with Claude Code (or any MCP host):

```bash
claude mcp add careeraxis -- /path/to/careeraxis-mcp
```

## How login works

CareerAxis signs in through NTU's ADFS (`loginfs.ntu.edu.sg`) over SAML. `setup` opens Chrome on
a dedicated profile (`~/.config/careeraxis-mcp/profile`), you sign in, and the binary reads the
cookies through the DevTools protocol. It never sees your password.

Every cookie involved is a session cookie, so the binary saves them itself in
`~/.config/careeraxis-mcp/jar.json` (mode 0600). When the CareerAxis session (`.CHAUTH`) expires,
the client replays the SAML exchange over plain HTTP using the saved ADFS session (`MSISAuth`). No
browser is involved, and it takes about 3 s. Only when ADFS itself expires do you see
`NEEDS_LOGIN` (CLI exit code 3), and then you run `setup` again or call the `careeraxis_login` tool.

Keep `~/.config/careeraxis-mcp/` out of git, backups and cloud sync: anyone with `jar.json` can use
your CareerAxis session.

## CLI

```
status                      session check
jobs <text>                 search jobs  [--type internship --industry ... --work-model Remote --order ClosingSoon --page N --size N --expired]
job <id>                    full job listing
filters                     valid values for jobs filters
recommended                 jobs recommended from your preferences  [--take N]
events <text>               search events  [--from YYYY-MM-DD --until YYYY-MM-DD --event-type-id N --page N]
event <id>                  full event details + booking link
my-events <which>           your booked events: upcoming | past | attended
orgs <text>                 find employers by name
org <id>                    employer profile + current openings
bookmarks                   your saved items
appointments <search>       career-advice appointment topics
slots <topic_id>            open days for a topic  [--date YYYY-MM-DD | --from --until]
notifications               your notifications
```

Output is a compact TSV or `key: value` layout. `--json` prints raw JSON and `--full` turns off
cell truncation. Filter names match by substring (`--type intern`, `--industry fintech`), and
`--remote` is shorthand for `--work-model Remote`.

## MCP tools

`careeraxis_status`, `careeraxis_login`, `search_jobs`, `get_job`, `job_filters`, `recommended_jobs`,
`search_events`, `get_event`, `my_events`, `search_organisations`, `get_organisation`, `bookmarks`,
`appointment_topics`, `appointment_availability`, `notifications`.

All tools are read-only: nothing books, applies, bookmarks or withdraws.

## Where the data comes from

- `/services/students/*`: the JSON API the CareerAxis web app itself calls. It is an NSwag-generated
  client, so parameter names come straight from the frontend bundle.
- Event detail and organisation pages have no JSON endpoint, so the binary parses their
  server-rendered HTML.
- Job applications (`/jobs/applications`) are switched off at NTU ("Talent Integration component
  is not enabled"). You apply on the employer's site, so the binary doesn't track applications.
- The public `/api/public/v1/jobs` and `/events` endpoints work without login but only return a
  small subset of listings, so the binary doesn't use them.
