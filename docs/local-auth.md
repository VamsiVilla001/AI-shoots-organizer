# Local JSON authentication

SKWAD does not require Supabase for login. The desktop reads a versioned JSON
credential file and verifies Argon2id password hashes locally.

The default Windows path is:

```text
%APPDATA%\com.skwad.mediaorganiser\auth\credentials.json
```

Set `SKWAD_AUTH_FILE` before launching the app to use a different local or LAN
path. The file must be readable for login and writable when a user changes a
temporary password.

## Add or reset a user

Use the included provisioning command. It prompts for the temporary password
without echoing it and never writes plaintext passwords:

```powershell
cargo run -p skwad-desktop --bin skwad-credentials -- `
  --file "$env:APPDATA\com.skwad.mediaorganiser\auth\credentials.json" `
  --email "person@example.com" `
  --display-name "Person Name"
```

Running it for an existing email resets that account to the entered temporary
password. New and reset accounts receive `mustChangePassword: true`. After a
successful login, SKWAD requires a different password of at least 10 characters
and changes the JSON record to `mustChangePassword: false`.

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
      "mustChangePassword": true
    }
  ]
}
```

Do not add plaintext `password` fields. Protect the file with operating-system
permissions and do not commit it. Using one temporary password for several new
users is acceptable only when every record requires a change on first login;
unique temporary passwords are safer.

Profiles are not stored in this credential file. Each installation stores its
own editable profile metadata in SQLite under the authenticated account ID.
