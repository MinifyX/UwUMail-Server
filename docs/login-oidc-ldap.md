# Logging in with OpenID Connect or LDAP

A server that belongs to a club, a family or a small company often has a place
where everyone already has a login: Authentik, Keycloak, Authelia, Kanidm, or an
LDAP directory. The portal can use it:

- **OpenID Connect:** the login page gets a button *Log in with …*, which sends
  the person to that provider and back.
- **LDAP:** the person types their directory password into the normal login
  form, and the server checks it at the directory. Mail apps can use the same
  password.

Both are set under *Server → Settings → Login*, take effect at once without a
restart, and can be tried with *Test connection* before they are saved. Logins
at the server itself keep working next to them.

For the other direction, mail apps signing in *to this server* with OAuth, see
[oauth.md](oauth.md).

## OpenID Connect

### At the provider

Create an application (a "client") for the portal:

- **Redirect URI:** `https://mail.example.com/api/auth/oidc/callback`, with the
  server's `hostname`. The settings page shows it with a copy button.
- **Client type:** confidential, with a client secret, is the usual choice. A
  public client without a secret works too; PKCE protects the code either way.
- **Scopes:** `openid email profile`. The server asks for exactly these.
- The provider has to vouch for the address: `email_verified` has to be `true`
  in the ID token or at the userinfo endpoint.

### Settings

| Setting | |
| --- | --- |
| `auth.oidc.enabled` | Shows the button and allows the login |
| `auth.oidc.issuer` | The provider's issuer, e.g. `https://auth.example.com/application/o/uwumail/`. The discovery document is read from `<issuer>/.well-known/openid-configuration`; a slash at the end may be left out or added |
| `auth.oidc.client_id` | The client ID from the provider |
| `auth.oidc.client_secret` | The client secret, if the client has one. Stored sealed |
| `auth.oidc.button_label` | What the button says after *Log in with*, e.g. `Authentik` |
| `auth.oidc.auto_create` | Makes an account at the first login of a verified address that has none |
| `auth.oidc.allowed_domains` | The domains such accounts may be made in (domains of this server). Empty: none |
| `auth.oidc.admin_group_claim` | A claim such as `groups` … |
| `auth.oidc.admin_group_value` | … whose value (or one of its values) makes a **new** account an admin, e.g. `uwumail-admins` |

### How a login goes

1. *Log in with …* goes to `/api/auth/oidc/start`, which sends the browser to
   the provider with the authorization code flow, PKCE (`S256`), a `state` and a
   `nonce`. The `state`, the PKCE verifier and the nonce travel in a cookie that
   this server signs and that lives ten minutes; the answer has to come back to
   the same browser.
2. The provider sends the browser back to `/api/auth/oidc/callback`. The server
   trades the code in at the token endpoint (with the client secret as HTTP
   Basic, or in the form when the provider only takes it there) and checks the
   ID token:
   - the signature, with a key from the provider's JWKS (`RS256`, `RS384`,
     `RS512`, `PS256`–`PS512`, `ES256`, `ES384` or `EdDSA`; never `none` or
     HMAC). Unknown key IDs make it fetch the keys again; otherwise discovery and
     keys are kept for an hour.
   - `iss` exactly as the provider names itself, `aud` the client ID (with
     several audiences, `azp` has to be the client ID), `exp` not passed and
     `iat` not in the future (two minutes of leeway), and the `nonce` of this
     login.
   - Address, name and groups come from the ID token, or from the userinfo
     endpoint when the ID token leaves them out (Authelia does).
3. The login is matched to an account:
   - by the provider's lasting subject (`iss` + `sub`), once it was seen before;
   - the first time, by the **verified** address: an account whose login is that
     address. Only one login at a provider can belong to an account; another one
     that later claims the same address gets nothing;
   - with `auto_create`, a new account in an allowed domain. It has no password
     here. Its owner uses [OAuth](oauth.md) or app passwords for mail apps, and
     confirms sensitive changes by logging in again.
4. **A second factor set up here is still asked for**: after the provider, the
   login page asks for the code or passkey as usual. The activity list shows the
   login as one at the provider.

When something does not work, the login page says why (`oidcError` in its
address): the provider could not be reached, the login took too long or came
back in another browser, the person cancelled, the answer did not check out,
the address was not verified, its domain is not allowed, there is no account,
or the account already belongs to another login there. The server's log has
the details.

Every request to the provider leaves like the server's other requests to the
internet (the egress route for updates, see [configuration.md](configuration.md)),
**over https and only to public addresses**. A provider that is only reachable
inside the local network cannot be used.

## LDAP

### Settings

| Setting | |
| --- | --- |
| `auth.ldap.enabled` | Checks passwords at the directory |
| `auth.ldap.url` | `ldaps://ldap.example.com` (port 636), or `ldap://ldap.example.com` (389) with STARTTLS |
| `auth.ldap.starttls` | STARTTLS on an `ldap://` address; on unless switched off |
| `auth.ldap.insecure_localhost` | Allows `ldap://` without TLS, but only to a directory on this very machine (`localhost`, `127.0.0.1`, `::1`) |
| `auth.ldap.user_dn_template` | A person's DN made from what they typed, e.g. `uid={user},ou=people,dc=example,dc=com`. With it, nothing is searched |
| `auth.ldap.bind_dn`, `auth.ldap.bind_password` | Without a template: the service account that searches for people (anonymous when empty). The password is stored sealed |
| `auth.ldap.base_dn` | Where to search, e.g. `ou=people,dc=example,dc=com` |
| `auth.ldap.user_filter` | How a person is found; default `(&(objectClass=person)(mail={email}))` |
| `auth.ldap.mail_attribute` | The attribute with the person's addresses; default `mail` |
| `auth.ldap.name_attribute` | The display name for new accounts; default `cn` |
| `auth.ldap.admin_group_dn` | A group whose members (by `memberOf`) become admins when their account is made |
| `auth.ldap.auto_create` | Makes an account at the first login of someone the directory knows |
| `auth.ldap.allowed_domains` | The domains such accounts may be made in. Empty: none |

`{email}` is the whole address that was typed (in lower case), `{user}` the
part before the `@`. In a filter they are escaped as RFC 4515 says, in a DN as
RFC 4514 says, so `*`, `(`, `)`, `\`, `,` or `=` in a login stay characters and
cannot change the search or the name.

The server trusts the usual web certificate authorities and those of the
system it runs on. A directory with a certificate from the organisation's own
authority works once that authority is in the system's store (in Docker: mount
it into the container's certificate folder).

### How a password is checked

1. With a DN template, the DN is made from the login. Otherwise the service
   account binds and searches below `base_dn` with the filter; exactly one
   entry has to come back, nobody or several are no login.
2. The server binds as that DN with the password that was typed. **An empty
   password is never sent**: most directories take a bind without one as an
   anonymous ("unauthenticated") bind and say yes.
3. It reads the entry's addresses, name and groups (as the person, or from the
   service account's search when the person may not read their own entry).
   **When the directory has addresses for the person, the login has to be one
   of them**, so a filter or template that only looks at `{user}` cannot let
   `leni@` of one domain into `leni@` of another.

A right password is remembered for five minutes, only in memory and only as a
salted hash, so a mail app that logs in often does not ask the directory each
time. Changing the settings forgets all of it.

### Which accounts use it

- **New people:** with `auto_create`, someone the directory knows gets an
  account at their first portal login, in the allowed domains, as an admin when
  they are in the admin group.
- **Existing accounts:** on the account's page (*Server → Accounts*), *Password
  is checked* switches between *here* and *at the LDAP directory*. Switching to
  the directory removes the password stored here, so the old one cannot be used
  next to the directory's. Switching back means setting a new password or
  sending a link. App passwords stay either way.

For an account at the directory, the password is changed at the directory: the
portal does not offer it, and an admin cannot set one here either.

### Mail apps

While main passwords are allowed for mail apps (the person has no second factor
and did not ask for app passwords), IMAP, SMTP, ManageSieve, JMAP, CalDAV and
CardDAV check the directory password too. Otherwise they need an app password
or [OAuth](oauth.md), as for every other account. Wrong passwords count against
the network like any others. A directory that cannot be reached is a temporary
failure for mail apps, and a refused login in the portal.

## In the config file

The same settings can be set in the config file or as environment variables
(`UWUMAIL_AUTH__OIDC__CLIENT_SECRET=…`); then they are locked in the portal.

```toml
[auth.oidc]
enabled = true
issuer = "https://auth.example.com/application/o/uwumail/"
client_id = "uwumail"
client_secret = "…"
button_label = "Authentik"
auto_create = true
allowed_domains = ["example.com"]

[auth.ldap]
enabled = true
url = "ldaps://ldap.example.com"
bind_dn = "cn=uwumail,ou=services,dc=example,dc=com"
bind_password = "…"
base_dn = "ou=people,dc=example,dc=com"
```

Secrets set in the portal are kept sealed in the database, like the passwords
of fetched mailboxes.
