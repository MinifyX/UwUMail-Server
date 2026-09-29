# Backups

The server backs up once a night, and whenever I press *Back up now* under
*Server → Overview → Backups*, to one of three places:

- an **SFTP server**, for example a NAS;
- an **S3 bucket**, at Amazon or any service that speaks the S3 API (MinIO,
  Backblaze B2, Hetzner Object Storage, Wasabi, Garage, …);
- a **folder** on the machine, for example a second disk or a NAS share mounted
  into the container.

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
server. It can be shown again under *Server → Overview → Backups* after confirming the
password.

Unencrypted backups are possible for a backup server that is encrypted itself.
The choice is fixed once there are backups; for a change, use a new folder.

Whether a backup is encrypted is decided by the server, by whether it has a
recovery key, and never by the backup server. A folder that claims to hold an
unencrypted backup while the server has a key is refused, and so is anything in
an encrypted backup that is not encrypted, that sits under another object's
name, or a snapshot stored under another snapshot's name. An unencrypted backup
has no such protection: its content is checked against its names, which
catches damage, but whoever controls the backup server can change both.

## Where backups go

### An SFTP server

- **SSH key** (recommended): the server makes its own key. Put the line the
  portal shows into `~/.ssh/authorized_keys` of the backup user.
- **Password**: for systems like Synology DSM that offer only that for SFTP.
  Left empty when saving, the stored password is kept, but only for the same
  host, port and user: for any other it has to be typed again, so it is never
  handed to a server it was not meant for.

The first *Test connection* shows the backup server's host key and remembers
it. If the key changes later, backups stop until I confirm the new one, as
`ssh` would warn.

A dedicated user with access to just the backup folder is a good idea. RSA
keys are not supported; the backup server needs an Ed25519 or ECDSA host key,
which current systems have.

### An S3 bucket

The server needs the S3 address without the bucket (such as
`https://s3.eu-central-1.amazonaws.com` or `https://fsn1.your-objectstorage.com`),
the bucket, the region the provider signs requests for, an access key and a
secret key. A folder in the bucket keeps several servers apart. *Bucket in the
path* sends `https://server/bucket/…` instead of `https://bucket.server/…`, which
MinIO and most servers in the own network want.

Requests are signed with AWS Signature Version 4; the secret key itself never
travels. Each object is written in one piece, so an interrupted upload leaves
nothing half-written behind. A bucket that answers *SlowDown* or is briefly
unavailable is asked again a few times.

The server does not take the bucket's word for everything: a listing page may
be 8 MB, one listing at most 2 million names (128 MB of them) over 10 000
pages, and an answer has as long as it would take at 32 KB/s for the most it
may be. A bucket that goes past that fails the backup with "damaged" or "did
not answer in time" instead of filling the memory or holding the backup for
ever.

Plain `http://` works only for addresses in the own network (a MinIO next to
the server): the backups and the signed requests would otherwise cross the
internet unencrypted. For anything on the internet, use `https://`.

A key with the rights to list, read, write and delete objects in that bucket
(or that folder) is all it needs. *Test connection* writes a small file, reads
it back and removes it.

### A folder

A full path such as `/backup`. It has to exist already: a disk or share that is
not mounted must not quietly turn into a folder on the disk the server runs
from. It also has to be outside the data directory, or the backup would be
lost together with the server. In Docker, mount it into the container, for
example with `- /mnt/nas/uwumail:/backup` under `volumes:`, and make it writable
for user 10001.

Inside the folder the server never follows a symbolic link. Whoever else can
write to a share could otherwise plant one that leads a backup, a clean-up of
old snapshots or *Test connection* to the server's own mail or database. Links
inside the folder are skipped when it is listed, and a backup that would have
to write through one stops with an error that names it. The folder itself may
be a link, since that is your own setting.

## From the command line

```sh
docker compose exec uwumail uwumail-server backup run     # back up now
docker compose exec uwumail uwumail-server backup list    # snapshots
docker compose exec uwumail uwumail-server backup check   # is the newest one complete?
```

## Restoring one mailbox

When someone deleted a folder they still needed, the whole server does not
have to go back in time. *Server → Overview → Backups → Restore one mailbox*
takes one person's mail out of a snapshot while the server keeps running:

1. **Open** a snapshot. Only its database is fetched, into a scratch folder in
   the data directory, and read without ever being opened as the server's own
   (it would be migrated otherwise). The page then lists everybody in it who had
   a mailbox. The database is read as something that may be anything: read-only,
   nothing in it may run code, its tables have to be tables, no single value
   may be longer than 64 KB, one person may have at most 100,000 folders and
   5 million messages, and reading it stops after ten minutes.
2. Pick the **person**, and **all folders or some**. *Into the mailbox of* puts
   the mail into another account, for someone whose address changed since.
3. **Restore.** Each message is taken from this server's own mail store when it
   still has it and fetched from the backup otherwise, checked against its hash,
   and stored again the ordinary way: into a new folder `Restored <date of the
   snapshot>`, with the old folders below it, read and flagged as it was.
   Messages the mailbox still has (the same Message-ID, or the same content) are
   left out, so restoring twice brings nothing twice. Nothing that is there now
   is changed.

A full mailbox stops it; what came back until then stays. While a snapshot is
being opened or a mailbox restored, the nightly backup waits, and the other way
round: a backup deletes what no snapshot needs, and must not do that under a
restore that is reading it. *Close snapshot* removes the fetched database; a
restart does too.

The same from the command line, with the backup settings of the server:

```sh
docker compose exec uwumail uwumail-server backup restore-mailbox \
  --account mini@example.org --folder Inbox --folder Projects/2025
```

`--snapshot` picks another snapshot than the newest, `--into` another mailbox;
without `--folder` every folder comes back. A folder brings the folders inside
it along.

## Restoring the whole server

There are three ways in, and they differ only in where you are standing.

### From the portal, on a server that is running

*Server → Overview → Backups → Snapshots*, then *Put back* beside the snapshot. The server
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

Backups in an S3 bucket or a folder are named the same way:

```sh
# S3: the keys come from the environment, never from the command line.
UWUMAIL_BACKUP_S3_ACCESS_KEY=… UWUMAIL_BACKUP_S3_SECRET_KEY=… \
  uwumail-server backup restore --s3 s3://my-bucket/uwumail \
  --endpoint https://s3.eu-central-1.amazonaws.com --region eu-central-1 --into /data
# A folder, mounted into the container:
uwumail-server backup restore --folder /backup --into /data
```

`--path-style` is for MinIO and similar servers.

The command asks for the recovery key (or reads `UWUMAIL_BACKUP_KEY`), also when
the backup server says the backup is not encrypted: if yours is, enter the key
anyway, and a backup server that lies about it is caught. With a password
instead of a key, set `UWUMAIL_BACKUP_SFTP_PASSWORD`. `--snapshot`
picks an older snapshot from `backup list`; `--host-key` checks the backup
server's fingerprint.

If the connection breaks halfway, run the same command again: mail that is
already in place stays, and only the rest is fetched.

Restore with the version the snapshot came from, or a newer one. The database
migrations only ever run forwards, so an older server cannot open a newer
snapshot and says so instead of trying. `backup list` shows each snapshot's
version.

Delete `/tmp/backup_key` afterwards.
