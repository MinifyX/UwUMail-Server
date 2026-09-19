# Virus scanner

UwUMail can hand every message to [ClamAV](https://www.clamav.net/) before it
is taken. If ClamAV finds something, the message is never accepted: the sending
server gets a `554` and tells its own sender, and nothing of it reaches a
mailbox. Mail that our own people send is checked the same way, so an infected
attachment does not leave the house either.

The scanner is not part of the UwUMail image: it runs in its own container
beside the server. clamd keeps its signatures in memory — a good gigabyte of
it — and needs a writable place for them, neither of which fits a read-only
image that is meant to run on a Raspberry Pi.

## Starting it

`install.sh` brings the scanner along and switches it on, unless you said
`--no-antivirus` or the machine has less than 2.5 GB of memory.
`update.sh` offers it to an installation that does not have it yet.

By hand it is a service in `compose.yaml` behind a profile, so it only starts
when somebody asks for it:

```bash
docker compose --profile antivirus up -d
docker compose exec uwumail uwumail-server settings set spam.antivirus.enabled true
```

Its first start takes a few minutes: the image ships without signatures and
fetches them once, into the `uwumail-clamav` volume. `docker compose logs
clamav` shows how far it got; `docker compose ps` shows `healthy` once clamd
answers.

The setting can also be switched under *Spam filter → Viruses* in the portal.
The server reaches the scanner as `clamav:3310` inside the compose network, and
nothing outside the machine can: the port is not published.

Leave the profile out of a later `docker compose up -d` and the scanner is
gone, while the setting stays on — the portal then says the scanner cannot be
reached, and mail keeps flowing unchecked. Switch it off in the portal too.

## What it does with a message

| What the scanner says | What happens |
| --- | --- |
| nothing found | the message goes its usual way and carries `X-Virus-Scanned: yes (ClamAV)` |
| something found | `554`, nothing is delivered, the find is written into the spam history with its name |
| no answer, too slow, or the message is bigger than `spam.antivirus.max_size` | the message goes on and carries `X-Virus-Scanned: no (…)` |

The last row is the deliberate part: a scanner that is away must not stop the
post. The header says that nobody looked, the server log says why, and the
health overview on the server page turns red while the scanner is unreachable.
A virus verdict that a message brought along is removed first, so the only one
left is this server's.

The check happens before the spam filter scores anything, so an infected
message is never learned from and never lands in anyone's Junk folder. It is
not a matter of points either: an allowed sender does not get a virus through.

## Settings

Under *Spam filter → Viruses*, or in the configuration file:

```toml
[spam.antivirus]
enabled = false
address = "clamav:3310"
timeout_secs = 30
max_size = 26214400  # 25 MiB
```

`max_size` follows clamd's own `StreamMaxLength` (25 MiB by default). Larger
messages are not sent to it at all, because it would refuse them anyway.

## Checking that it works

The page has a button that sends the scanner the
[EICAR test file](https://www.eicar.org/download-anti-malware-testfile/) — a
harmless string every scanner recognises. If it comes back named, the two
really do talk to each other.

The page also shows the version and the number and date of the signature
database. ClamAV publishes several times a day; a database older than three
days means its updater (`freshclam`, which runs inside the same container) is
not getting through, and the health overview says so.

Tried end to end on the test instance on 18 September 2026 with ClamAV 1.5.4: a
message with the EICAR file as an attachment came back as `554 5.7.0 This
message contains Eicar-Test-Signature` and reached no mailbox, a clean one
arrived carrying `X-Virus-Scanned: yes (ClamAV)`, a scan verdict the sender
had written itself was dropped, and with the scanner stopped both messages went
through with `X-Virus-Scanned: no (the virus scanner did not answer)`.

## Updating, and installations that are already running

`update.sh` brings `compose.yaml` up to date, so a server installed before
0.3.0 — whose file has no scanner in it at all — gets one there. It then offers
to add the scanner, and `--no-antivirus` says no. By hand, the current file is:

```bash
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/compose.yaml
```

A plain `docker compose pull && docker compose up -d` only fetches a new image
and leaves `compose.yaml` alone.

To have the scanner come along with every `docker compose up -d` — including
the ones an update does for you — the profile goes into `.env`, which is what
the two scripts write there:

```
COMPOSE_PROFILES=antivirus
```

Without that line it still survives updates: `docker compose up -d` and even
`docker compose down` leave a running scanner alone, because it belongs to a
profile that was not asked for. Only `docker compose --profile antivirus down`
takes it away — and after a reboot it comes back by itself, like every other
container here.

## Memory

clamd keeps the whole signature database in memory. On the test instance
(ClamAV 1.5.4, 3.6 million signatures) the container settles at about 970 MB
while UwUMail itself uses under 10 MB; plan for 2 GB so an update of the
signatures has room. On a machine with 2 GB in total the two will not fit
comfortably — leave the scanner off there, or give the machine more memory.

The built-in lists from abuse.ch already catch known malware links and file
hashes without a scanner ([spam filter](spam-filter.md)); they are not a
replacement, but they cost nothing.
