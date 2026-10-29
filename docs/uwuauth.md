# Pairing with UwUAuth

[UwUAuth](https://github.com/MinifyX/UwUAuth-Server) is the login service of the UwU suite. Paired
with it, UwUMail gets two things at once:

- **Login:** the login page gets a button *Log in with UwUAuth* (OpenID Connect, see
  [login-oidc-ldap.md](login-oidc-ldap.md)). Nobody has to copy a client ID or secret.
- **Accounts (SCIM 2.0):** UwUAuth creates a mailbox when a person is given access to UwUMail,
  locks it when the person is locked there, makes them an admin or not, and moves the mailbox to
  the trash when access is taken away.

## Pairing

1. In UwUAuth, add UwUMail as an app. UwUAuth shows a one-time pairing code and a QR code.
2. In UwUMail, open *Server → Settings → UwUAuth*, paste the code, and type UwUAuth's address. The
   text of the QR code (`https://auth.example.com/#pair=CODE`) holds both: pasted into the code
   field, the address is not needed.
3. *Pair*. UwUMail asks `GET /uwu/v1/server` whether a UwUAuth that can pair answers there, reads
   its OpenID Connect discovery document, and redeems the code with `POST /uwu/v1/pair`. It sends
   its redirect URI (`https://<host>/api/auth/oidc/callback`), its SCIM address
   (`https://<host>/scim/v2`) and the roles it knows (`admin`, `user`).

UwUAuth answers with the client and a SCIM token. UwUMail then sets:

| Setting | Value |
| --- | --- |
| `auth.oidc.enabled` | `true` |
| `auth.oidc.issuer`, `client_id`, `client_secret` | From UwUAuth |
| `auth.oidc.button_label` | `UwUAuth` |
| `auth.oidc.auto_create` | `false`: mailboxes come from SCIM, not from the first login |
| `auth.oidc.scopes` | What UwUAuth granted (`openid email profile groups roles` asked for) |
| `auth.oidc.admin_group_claim` / `admin_group_value` | `roles` / `admin` |
| `auth.oidc.sync_admin` | `true`: each login makes the person an admin or not, by the claim |
| `auth.oidc.private_network` | As chosen when pairing |

The SCIM token itself is not stored, only its SHA-256 hash; the pairing (address, issuer, app id,
who paired when) is kept in the setting `uwuauth.pairing`. Settings that come from the config file
stay as they are, and the page says so.

What can go wrong: an unknown, used or expired code (*create a new one in UwUAuth*), too many tries
(UwUAuth's own limit), an address where no UwUAuth 0.4 or newer answers, or an answer whose issuer
differs from the discovery document. Nothing is changed in those cases.

### UwUAuth in the local network

Normally the login service must have a public address: the server refuses private, loopback and
link-local addresses for every outgoing request, so a link cannot make it reach into the local
network. *The login service is in the local network* (`auth.oidc.private_network`) lifts this for
**the issuer's host only**, and only for the login (discovery, keys, tokens) and pairing. Everything
else stays guarded.

HTTPS stays mandatory, and the certificate must be one the server trusts: a public CA (for example
Let's Encrypt with a DNS challenge for an internal name). A self-signed certificate does not work.

## SCIM

UwUAuth calls `https://<host>/scim/v2` with `Authorization: Bearer <token>`. The token is compared
in constant time against the stored hash; wrong tokens count towards the same per-address limit as
wrong passwords, and a blocked address gets `429`.

| Endpoint | |
| --- | --- |
| `GET /ServiceProviderConfig`, `/ResourceTypes`, `/Schemas` | What is supported: PATCH yes, bulk, sort, ETags and password changes no; filters only `attribute eq "value"` |
| `GET/POST /Users`, `GET/PUT/PATCH/DELETE /Users/{id}` | People, as mailboxes |
| `GET/POST /Groups`, `GET/PUT/PATCH/DELETE /Groups/{id}` | Groups, kept but not used |

Answers are `application/scim+json`; errors use the SCIM error schema with `scimType`
(`uniqueness`, `invalidValue`, `invalidFilter`, `mutability`).

### People

- **Create:** `userName` is the mail address and must be on a domain of this server (else `400
  invalidValue`). The mailbox gets `auth_source = oidc` and no password: the person logs in with
  UwUAuth. `externalId` is UwUAuth's id of the person, which is also the OpenID Connect `sub`; the
  login is linked to the mailbox at once. An address that already has a mailbox, or an `externalId`
  that belongs to another one, is `409 uniqueness`. UwUAuth then finds the mailbox with a filter
  (`userName`, `externalId` or `id`) and links it.
- **`active: false`** locks the mailbox (no login, no IMAP/SMTP/JMAP). `active: true` unlocks it
  only if SCIM locked it: a mailbox an admin locked here stays locked.
- **Roles:** `admin` makes the person an admin, anything else a user. The last admin cannot be
  locked or demoted (`409 mutability`).
- **`userName` cannot change** (`400 mutability`): a mailbox does not move to another address.
- **Delete** moves the mailbox to the trash, where an admin can restore it until the trash is
  emptied. From then on the SCIM id answers `404`, as SCIM expects; the address stays taken, so a
  new user with the same address is `409` until the mailbox is restored or deleted for good.
- Mailboxes made here, not by UwUAuth, are listed too and get a SCIM id the first time UwUAuth
  looks, so UwUAuth can take them over.

### Groups

UwUMail keeps groups with their members so UwUAuth finds them again, and answers every request
validly, but groups change nothing about mail (mail groups are a different thing, see
[groups.md](groups.md)).

## Unpairing

*Unpair* switches the UwUAuth login off and clears the client (only if the login settings still
point at the paired UwUAuth), forgets the SCIM token, the groups and the external ids. Mailboxes,
their mail and the links of earlier logins stay; the people then need a password from an admin, or a
new pairing. Remove the app in UwUAuth as well.

## Tests

`crates/uwumail-web/tests/integration/uwuauth.rs` runs the pairing and SCIM against a small fake
UwUAuth in the same process: the test transport hands requests to its router instead of the
network, because the real transport only trusts public certificates.
