# OAuth 2.0 and OpenID Connect for mail apps

Instead of an app password, a mail app can sign in the way it signs in to big
providers: it opens the portal in a browser, the person logs in there (with
their second factor or passkey, if they have one) and agrees, and the app gets
tokens it uses from then on. The password never reaches the app, and the
person sees each app under *My account → Security* and can sign it out there.

The server is an OAuth 2.0 authorization server (RFC 6749) and an OpenID
Connect provider for this. Every app is a **public client**: it runs on the
person's own device and could not keep a secret, so every sign-in uses PKCE
(RFC 7636) with `S256`.

For logging in to the portal *at* another provider (Authentik, Keycloak, …)
see [login-oidc-ldap.md](login-oidc-ldap.md); that is the other direction.

## Discovery

Both addresses answer with the same document:

- `https://mail.example.com/.well-known/oauth-authorization-server` (RFC 8414)
- `https://mail.example.com/.well-known/openid-configuration` (OpenID Connect Discovery)

| Field | Value |
| --- | --- |
| `issuer` | `https://mail.example.com` (the server's `hostname`) |
| `authorization_endpoint` | `/oauth/authorize` |
| `token_endpoint` | `/oauth/token` |
| `registration_endpoint` | `/oauth/register` |
| `revocation_endpoint` | `/oauth/revoke` |
| `jwks_uri` | `/oauth/jwks` |
| `userinfo_endpoint` | `/oauth/userinfo` |
| `code_challenge_methods_supported` | `S256` |
| `token_endpoint_auth_methods_supported` | `none` |
| `id_token_signing_alg_values_supported` | `ES256` |
| `authorization_response_iss_parameter_supported` | `true` (RFC 9207) |

These endpoints (and the token endpoints below) answer any web page
(`Access-Control-Allow-Origin: *`): they hold nothing a cookie would unlock.

## Registering an app: `POST /oauth/register`

Apps register themselves (RFC 7591), usually once per installation:

```http
POST /oauth/register HTTP/1.1
Host: mail.example.com
Content-Type: application/json

{ "client_name": "Mail on the laptop", "redirect_uris": ["http://127.0.0.1/oauth"] }
```

```json
{
  "client_id": "uwu-3q2kX0…",
  "client_id_issued_at": 1790000000,
  "client_name": "Mail on the laptop",
  "redirect_uris": ["http://127.0.0.1/oauth"],
  "token_endpoint_auth_method": "none",
  "grant_types": ["authorization_code", "refresh_token"],
  "response_types": ["code"]
}
```

- Whatever `token_endpoint_auth_method` an app asks for, it gets `none` and no
  secret (RFC 7591 lets the server replace what it does not offer).
- `grant_types` may only name `authorization_code` and `refresh_token`,
  `response_types` only `code`; anything else is `invalid_client_metadata`.
- `redirect_uris`: 1 to 10 addresses of at most 500 characters, each one of
  - `https://…` without a login in it,
  - `http://127.0.0.1…`, `http://[::1]…` or `http://localhost…`, the device
    itself (RFC 8252 section 7.3). The app may use any port there later, since
    it picks a free one each time; host and path have to match.
  - a private-use scheme in reverse-DNS form, such as
    `com.example.mail:/oauth2redirect` (RFC 8252 section 7.1).

  Never a fragment (`#`), never `javascript:`, `data:` or plain `http://` to
  another host. A wrong address is `invalid_redirect_uri`.
- `client_name` is what the consent page and the list under *Security* show,
  cut to 80 characters. The consent page says clearly that the name is the
  app's own claim, and shows where the answer goes.
- 30 registrations per network (IPv4 address or IPv6 /64) and hour. An app
  nobody ever allowed in is forgotten after a day, one nobody uses any more
  after 7 days. The server keeps at most 10,000 apps; when that is full, a new
  registration pushes out the oldest app nobody ever allowed in, so
  registrations alone cannot keep a real app out. Apps in use are never
  pushed out.

## Signing in: `/oauth/authorize`

The app opens the browser at

```
https://mail.example.com/oauth/authorize?response_type=code
  &client_id=uwu-3q2kX0…
  &redirect_uri=http://127.0.0.1:53682/oauth
  &scope=openid email mail smtp
  &state=…&nonce=…
  &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
  &code_challenge_method=S256
```

This is a page of the portal. Without a portal session it shows the login
first, including the second factor or a passkey, and comes back to the
question afterwards. Then it shows the app's name, the account, where the
answer goes and what the app asks for, with *Allow* and *Deny*.

| Parameter | |
| --- | --- |
| `response_type` | `code`; nothing else is offered |
| `client_id` | from the registration |
| `redirect_uri` | one of the registered addresses (for loopback, any port); may be left out when exactly one is registered |
| `scope` | space-separated, see below; `mail smtp` when left out |
| `state` | handed back unchanged; up to 1024 characters |
| `nonce` | put into the ID token; up to 256 characters |
| `code_challenge`, `code_challenge_method` | required, `S256` |
| `prompt` | `consent` asks again even when allowed before; `none` never asks and answers `consent_required` if the app was not allowed before |

The answer goes to the redirect address with `code`, `state` and `iss`, or with
`error` (`invalid_request` when PKCE is missing, `unsupported_response_type`,
`invalid_scope`, `access_denied` when the person says no,
`consent_required`). An unknown `client_id` or a redirect address the app did
not register is shown on the page and never sent anywhere.

Anyone can register an app with any https address, so an error goes back by
itself only to an app the person allowed in before, or to one on the device
(a loopback address or an app scheme). For a web address nobody allowed yet,
the page shows the error instead of sending the browser there (RFC 9700
section 4.11.2); `prompt=none` without consent is shown the same way. Saying
no always goes back to the app: that takes a click.

Allowing an app for the first time, or for more than before, needs the
password again when the portal login is older than ten minutes, as a new app
password does. Once a person allowed an app, it is not asked again for the
same scopes: the page hands out the code by itself. Signing the app out under *Security*
forgets that.

### Scopes

| Scope | What it opens |
| --- | --- |
| `mail` | IMAP, JMAP and ManageSieve (mail and mail rules); over JMAP this includes sending |
| `smtp` | Sending through submission (ports 587 and 465) |
| `dav` | Calendars and contacts (CalDAV, CardDAV, and over JMAP) |
| `maskedemail` | Masked addresses only, over JMAP; nothing else of the mailbox ([jmap-masked-email.md](jmap-masked-email.md#apps-allowed-masked-addresses-only-the-maskedemail-scope)) |
| `app-password` | One app password for the app, made once at [`POST /oauth/app-password`](#an-app-password-for-the-app-post-oauthapp-password); the token opens no protocol itself |
| `openid` | An ID token, and `/oauth/userinfo` |
| `email` | The address in the ID token and userinfo |
| `profile` | The name in the ID token and userinfo |
| `offline_access` | Accepted; every sign-in gets a refresh token anyway |

`imap`, `jmap`, `sieve` and `managesieve` count as `mail`, `submission` as
`smtp`, `caldav` and `carddav` as `dav`. Unknown scopes are left out.
Protocols the admin switched off for the account (*Protocols* on the
account's page) are left out as well (`maskedemail` goes with JMAP), and a
request that ends up with none of `mail`, `smtp`, `dav`, `maskedemail`,
`app-password` or `openid` is `invalid_scope`. `mail` includes everything
`maskedemail` opens.

`app-password` is never remembered as allowed: each request with it shows the
question (and needs the password again when the login is older than ten
minutes), and `prompt=none` answers `consent_required`. The page then says
plainly that the app wants to set itself up with an app password.

## Tokens: `POST /oauth/token`

Form-encoded (`application/x-www-form-urlencoded`). The app names itself with
`client_id` in the form, or as the user name of HTTP Basic, which some
libraries send for public clients too; a secret sent along is not looked at.

**`grant_type=authorization_code`** with `code`, `redirect_uri` (exactly the one
the code was made for) and `code_verifier`:

```json
{
  "access_token": "uwu_at_…",
  "token_type": "Bearer",
  "expires_in": 3600,
  "refresh_token": "uwu_rt_…",
  "scope": "openid email mail smtp",
  "id_token": "eyJhbGciOiJFUzI1NiIs…"
}
```

- A code works for two minutes, once, and only for the app and redirect address
  it was made for. A wrong verifier uses it up as well.
- The **access token** works for an hour.
- The **refresh token** works for 90 days and only once: **`grant_type=refresh_token`**
  with `refresh_token` hands out a new access token and a new refresh token. If
  a refresh token that was already traded in comes back, it was copied: the
  whole sign-in ends for every token of it, and the person gets a mail about it
  (RFC 9700 section 4.14).
- The **ID token** (only with `openid`) is signed with ES256. It carries `iss`,
  `sub` (the account's number here), `aud` and `azp` (the `client_id`), `iat`,
  `exp`, `auth_time`, `nonce`, and with the scopes `email`/`email_verified` and
  `name`/`preferred_username`. The key is at `/oauth/jwks`; it is made the first
  time it is needed and kept sealed in the database.

Errors follow RFC 6749 section 5.2: `invalid_client` (401) for an unknown app,
`invalid_grant` for a wrong, used or expired code or token,
`unsupported_grant_type`, `invalid_request`. After 30 refused requests in 15
minutes a network gets `429 temporarily_unavailable` from the token, revocation
and userinfo endpoints until the window has passed. This is counted apart from
logins: an app that keeps trying a token its person revoked does not lock the
household out of the portal.

**`POST /oauth/revoke`** (RFC 7009) with `token` (access or refresh) and
`client_id` signs the app out: every token of that sign-in stops working. It
answers `200` for unknown tokens too.

**`GET /oauth/userinfo`** with `Authorization: Bearer <access token>` of a
sign-in with `openid` returns `sub`, and with the scopes `email` and `profile`
the address and name.

Tokens and codes are long random strings. The database keeps only their
SHA-256 hashes, like app passwords.

## An app password for the app: `POST /oauth/app-password`

A mail app that only wants a normal login, such as the UwUMail app setting an
account up, asks for `scope=app-password` alone, trades the code in as usual
and then, right away:

```
POST /oauth/app-password
Authorization: Bearer uwu_at_…
Content-Type: application/json

{ "name": "Lorins MacBook", "scopes": ["mail", "smtp", "dav"] }
```

| Field | |
| --- | --- |
| `name` | required; trimmed, 1 to 80 characters, no control or bidi characters. Shown in the list of app passwords, e.g. the device's name |
| `scopes` | optional; out of `mail`, `smtp`, `dav`. Left out: every one the account may use. Uses the account may not have (switched-off protocols) are left out |

`201 Created`:

```json
{
  "id": 42,
  "name": "Lorins MacBook",
  "username": "nyu@example.com",
  "password": "abcd-efgh-jkmn-pqrs",
  "scopes": ["dav", "mail", "smtp"]
}
```

`username` and `password` are an ordinary login for HTTP Basic (JMAP, CalDAV,
CardDAV), IMAP `LOGIN`/`AUTHENTICATE PLAIN`, SMTP `AUTH PLAIN`/`LOGIN` and
ManageSieve, within `scopes`. The password is shown only in this answer. It
works when the person requires app passwords for mail apps, like any other
app password.

**Once only.** In the same database transaction the whole sign-in behind the
token ends: the access token, its refresh token and the grant. The app keeps
nothing but the app password, which the person sees under *Security → App
passwords* and revokes there. When a check fails (name, scopes, the limit of 50
app passwords), nothing is made and the token stays good for another try.

Errors are problem details (`application/problem+json`, RFC 9457) that also
carry OAuth's `error` and `error_description`:

| Status | `error` | When |
| --- | --- | --- |
| 400 | `invalid_request` | not JSON, unknown scopes, a name that does not fit, no use the account may have |
| 401 | `invalid_token` | unknown, expired or already traded-in token, or the account may not log in (disabled, made a service); `WWW-Authenticate: Bearer error="invalid_token"` |
| 401 | `invalid_request` | no `Authorization: Bearer` header |
| 403 | `insufficient_scope` | a good token without `app-password`; `WWW-Authenticate: Bearer error="insufficient_scope", scope="app-password"`. The token stays good |
| 409 | `tooManyAppPasswords` | the account has 50 app passwords already |
| 429 | `temporarily_unavailable` | the network had 30 refused tokens in 15 minutes (counted with the token endpoint) |

The person gets the *new app password* mail and activity entry, as for one made
in the portal; the sign-in itself sends none. Like the other `/oauth/`
endpoints it answers any web page (`Access-Control-Allow-Origin: *`): the token
comes in the header only, never from a cookie.

## Using the access token

| Protocol | How |
| --- | --- |
| IMAP (993) | `AUTHENTICATE OAUTHBEARER` or `AUTHENTICATE XOAUTH2`, with SASL-IR or after the `+` |
| SMTP submission (587 after STARTTLS, 465) | `AUTH OAUTHBEARER` or `AUTH XOAUTH2` |
| ManageSieve (4190, after STARTTLS) | `AUTHENTICATE "OAUTHBEARER"` or `"XOAUTH2"` |
| JMAP, CalDAV, CardDAV | `Authorization: Bearer <access token>` |

A token with `maskedemail` but without `mail` works only on JMAP's session,
API, event stream and WebSocket, and there only for masked addresses; see
[jmap-masked-email.md](jmap-masked-email.md#apps-allowed-masked-addresses-only-the-maskedemail-scope).

The mechanisms are listed in `CAPABILITY`, `EHLO` and the ManageSieve
capabilities. OAUTHBEARER (RFC 7628) looks like
`n,a=nyu@example.com,^Aauth=Bearer uwu_at_…^A^A`, XOAUTH2 like
`user=nyu@example.com^Aauth=Bearer uwu_at_…^A^A`, both base64-encoded. The login
in them has to be the token's account (in any case); OAUTHBEARER may leave it
out.

A refused token first gets the error challenge RFC 7628 section 3.2.2
describes, base64-encoded:

```json
{ "status": "invalid_token", "scope": "mail",
  "openid-configuration": "https://mail.example.com/.well-known/openid-configuration" }
```

(XOAUTH2: `"status": "401"` and `"schemes": "bearer"`.) The app answers with a
single `^A` (`AQ==`), and then gets `NO` or `535`. Refused tokens count against
the network like wrong passwords.

Each token only works within its scopes, while the account may log in and use
the protocol. An OAuth sign-in went through the portal's whole login, second
factor included, so it keeps working when a person requires app passwords for
mail apps, the same way app passwords do.

## Seeing and ending sign-ins

- *My account → Security → Apps signed in with OAuth* lists each sign-in with
  its app, scopes, when it was made and last used (protocol and address).
  *Sign out* ends it at once.
- The first sign-in of an app sends the person a mail (not one for nothing but
  `app-password`: the app password made with it sends its own), and so does a refresh
  token that came back after it was traded in. The security activity lists
  sign-ins of apps and signing them out.
- An admin sees the same list on the person's page and can sign an app out,
  for a lost phone for example; that is in the change log.
- Disabling or deleting the account ends every token.
