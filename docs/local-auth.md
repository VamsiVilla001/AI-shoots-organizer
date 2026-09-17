# Local JSON authentication

SKWAD does not require Supabase for login. The desktop reads a versioned JSON
credential file and verifies Argon2id password hashes locally.

By default the file lives in the library folder, so a team sharing one library
over the network shares one set of accounts. On a machine with no shared
library configured that is:

```text
%APPDATA%\com.skwad.mediaorganiser\auth\credentials.json
```

Set `SKWAD_AUTH_FILE` before launching the app to use a different local or LAN
path. The file must be readable for login and writable when an administrator
changes an account. See [the shared library](shared-library.md) for pointing a
whole team at one folder.

## Testing build: shared password, no forced change

While the team tests this build:

- The first launch on a machine with no credential file writes the team roster
  and gives every account the password `Tess@123`.
- A credential file from an older build that has no administrator adopts the
  same roster once, adding the missing accounts without touching the passwords
  of accounts that are already there. After that the admin panel owns the file:
  an account an administrator removes stays removed.
- Nobody is asked to change their password on first sign-in.

Both behaviours are the constants at the top of
`apps/desktop/src-tauri/src/catalogue.rs`:

```rust
const ENFORCE_PASSWORD_CHANGE: bool = false;
const SEED_PASSWORD: &str = "Tess@123";
const SEED_USERS: &[(&str, &str, UserRole)] = &[ … ];
```

Setting `ENFORCE_PASSWORD_CHANGE` back to `true` restores the original flow:
new and reset accounts get `mustChangePassword: true`, and after a successful
sign-in SKWAD requires a different password of at least 6 characters before
letting the account in. Nothing else has to change — the sign-in screen still
carries the change-password step.

## Roles and the admin panel

Every account is an `admin` or a `member`. Administrators get a **Users** panel
(Settings → Users in the project workspace, or the Users item in the classic
sidebar) and can:

- add an account, with a password of their choice or the shared testing one,
- rename an account, change its role, enable or disable it,
- reset any account's password,
- remove an account.

The backend refuses to let an administrator remove their own access, delete the
account they are signed in with, or leave the workspace with no enabled
administrator. Members never see the panel, and the commands behind it reject
non-administrators, so hiding the tab is not the only thing protecting it.

## Add or reset a user from the command line

The provisioning binary still works and is the way in if nobody can sign in. It
prompts for the password without echoing it and never writes plaintext:

```powershell
cargo run -p skwad-desktop --bin skwad-credentials -- `
  --file "$env:APPDATA\com.skwad.mediaorganiser\auth\credentials.json" `
  --email "person@example.com" `
  --display-name "Person Name" `
  --role admin
```

`--role` is `member` unless given. Running it for an existing email resets that
account to the entered password.

## File shape

```json
{
  "version": 1,
  "users": [
    {
      "id": "stable-uuid",
      "email": "person@example.com",
      "displayName": "Person Name",
      "passwordHash": "$argon2id$...",
      "enabled": true,
      "mustChangePassword": false,
      "role": "member"
    }
  ]
}
```

`role` is optional; a record without one is a member. Do not add plaintext
`password` fields — the file is rejected if you do. Protect the file with
operating-system permissions and do not commit it.

Profiles are not stored in this credential file. Each installation stores
editable profile metadata in SQLite under the authenticated account ID, which
is shared with the rest of the team when the library is.
