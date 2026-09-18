# Backups

The server backs up to an SFTP server, for example a NAS, once a night and
whenever I press *Jetzt sichern* under *Server → Backups*.

## What is in a backup

Everything a new server needs to take over: the database (people, settings,
spam filter, calendars and contacts), every mail and the other files in the
data directory, such as certificates. The database is copied while mail keeps
arriving, so nothing has to stop.

The first backup uploads everything. Later ones only upload what is new: every
mail is stored once by its content, and the database copy is cut into pieces
that only change where the database changed. A day of mail usually means a few
megabytes.

Every snapshot stays complete on its own. The retention rules keep the newest
snapshot of each of the last 7 days, 4 weeks and 6 months (adjustable), and
remove what no remaining snapshot needs.

## Encryption

Backups are encrypted by default (ChaCha20-Poly1305, with keyed names, so the
backup server sees neither content nor which mails exist). When I set up the
backups, the portal shows the **recovery key** once. Without it the backups
cannot be read by anyone, so it belongs in a password manager, apart from the
server. It can be shown again under *Server → Backups* after confirming the
password.

Unencrypted backups are possible for a backup server that is encrypted itself.
The choice is fixed once there are backups; for a change, use a new folder.

## The backup server

- **SSH key** (recommended): the server makes its own key. Put the line the
  portal shows into `~/.ssh/authorized_keys` of the backup user.
- **Password**: for systems like Synology DSM that offer only that for SFTP.

The first *Test connection* shows the backup server's host key and remembers
it. If the key changes later, backups stop until I confirm the new one, as
`ssh` would warn.

A dedicated user with access to just the backup folder is a good idea. RSA
keys are not supported; the backup server needs an Ed25519 or ECDSA host key,
which current systems have.

## From the command line

```sh
docker compose exec uwumail uwumail-server backup run     # back up now
docker compose exec uwumail uwumail-server backup list    # snapshots
docker compose exec uwumail uwumail-server backup check   # is the newest one complete?
```

## Restoring the whole server

There are three ways in, and they differ only in where you are standing.

### From the portal, on a server that is running

*Server → Backups → Snapshots*, then *Put back* beside the snapshot. The server
fetches it, stops itself, and the start after that puts the files in place —
while it runs, the database it would replace is the one it is running on. Docker
brings the container back by itself; it takes a few minutes.

Three things belong to this machine and not to the one the snapshot came from,
and are put right afterwards:

- **Backups are switched off.** The snapshot carries the old server's backup
  target and recovery key, and the first thing this machine would otherwise do
  is write its own history over that server's. Turn them on again once the
  target is the right one.
- **The gateway pairing of this machine is kept**, unless you say otherwise. The
  pairing in the snapshot belongs to the machine that made it.
- The database from before is kept as `uwumail.db.replaced.<time>` in the data
  directory. Delete it once you are sure.

### From the setup assistant, on a fresh machine

A machine that is standing in for one that died has no admin yet, so open
`/setup`, enter the one-time code, and choose *Put a backup back* instead of
creating the first admin. It asks for the backup server, lists what is there and
puts a snapshot back.

That machine has no SSH key the backup server knows, and no way to add one — the
machine that had it is gone. So the assistant takes the old private key pasted
in, or a password. Afterwards you log in with an account from the backup, not a
new one.

### From the command line

Restoring needs nothing from the old server: on a new machine, restore into an
empty data directory before starting UwUMail there.

The container runs as user 10001, so give it a copy of the key it may read:

```sh
sudo install -o 10001 -m 0400 ~/.ssh/backup_key /tmp/backup_key
docker run --rm -it -v uwumail-data:/data \
  -v /tmp/backup_key:/key:ro \
  ghcr.io/minifyx/uwumail-server:latest \
  backup restore --sftp backup@nas.example.com:uwumail --ssh-key /key --into /data
```

The command asks for the recovery key (or reads `UWUMAIL_BACKUP_KEY`). With a
password instead of a key, set `UWUMAIL_BACKUP_SFTP_PASSWORD`. `--snapshot`
picks an older snapshot from `backup list`; `--host-key` checks the backup
server's fingerprint.

If the connection breaks halfway, run the same command again: mail that is
already in place stays, and only the rest is fetched.

Restore with the version the snapshot came from, or a newer one. The database
migrations only ever run forwards, so an older server cannot open a newer
snapshot and says so instead of trying. `backup list` shows each snapshot's
version.

Delete `/tmp/backup_key` afterwards.
