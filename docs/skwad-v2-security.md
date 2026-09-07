# SKWAD V2 encrypted catalogue prototype

The `.skwad` container is metadata-only. Its encrypted ZIP payload contains
exactly `manifest.json` and a dedicated portable `catalog.sqlite`. The portable
database is rebuilt from an allow-listed schema; it is not a copy of the local
working database. Absolute paths, originals, proxies, thumbnails, face crops,
embeddings, credentials, jobs, exports, settings and application logs are not
included.

## Cryptography

- A fresh random 256-bit data key encrypts every revision with
  XChaCha20-Poly1305.
- Device and backend wraps use HPKE with X25519, HKDF-SHA-256 and
  ChaCha20-Poly1305.
- The offline passphrase wrap uses Argon2id (64 MiB minimum) and
  ChaCha20-Poly1305. Production defaults use three iterations; benchmark this
  on supported release hardware and tune toward 500 ms.
- The backend signs the authenticated header and ciphertext with Ed25519.
- Immutable payload fields are AEAD associated data. Recipient wraps are
  signature-authenticated so the backend can add an authorised device wrap and
  re-sign without re-encrypting a large payload.

Backend signing and wrapping private keys are accepted only by the separate
`skwad-backend` process through server-side environment/secret configuration.
Generate local-development keys with `cargo run -p skwad-backend -- keygen`.
Do not commit the output.

## Local development

1. Install the Supabase CLI and Docker, then run `supabase start`.
2. Use the public values in the root `.env.example` for the desktop process.
3. Generate backend keys and place them in a separate backend-only environment,
   following `services/backend/.env.example`. Never load that environment into
   the desktop process.
4. Run `cargo run -p skwad-backend`.
5. Start the desktop application with `npm run dev`.

Run `supabase test db` while the local stack is available. The policy suite in
`supabase/tests/catalogue_rls.sql` exercises anonymous, owner, editor, viewer
and revoked-member access, owner-only publishing, and published-revision
immutability.

The SQL migration enables RLS for every shared table. Anonymous access has no
policies. Owners publish and administer members, editors modify draft metadata,
and viewers read published data. Published revision rows are immutable.

## Moving authentication to the permanent local/LAN server

The desktop is not tied to a hosted Supabase project. In development,
`SKWAD_SUPABASE_URL=http://127.0.0.1:54321` points it at the local stack. For a
permanent self-hosted server, apply every file under `supabase/migrations`, then
change `SKWAD_SUPABASE_URL`, `SKWAD_SUPABASE_ANON_KEY`, and
`SKWAD_BACKEND_URL` in the desktop launch environment. No rebuild is required.

Do not expose PostgreSQL directly to the desktop. The client talks to Supabase
Auth and its RLS-protected REST API. Use HTTPS for every non-loopback address.
To preserve development accounts, migrate both the Supabase `auth` schema and
the application `public` schema using the supported backup/restore process;
copying only `public.profiles` does not copy password hashes. If the destination
uses a different JWT secret, existing cached sessions must be discarded and
users must sign in again. Device private keys should stay in each computer's OS
credential store and must not be imported into PostgreSQL.

## Security boundary

The operating-system credential store holds the device private key and cached
authenticated session. Passphrases are never stored. Decrypted portable SQLite
bytes live only in process memory and are wiped on drop. A catalogue media
reference is always `libraryId + normalizedRelativePath`; opening it requires a
device-local approved NAS root and a final canonical containment check.
Backend signing keys are learned only during an authenticated sign-in or
publish flow. Opening a package never trusts an unfamiliar signing key; the
user must sign in again to accept an authorised key rotation.

Possession of an exported package and its passphrase grants access to that
revision. Revocation prevents future wraps and future revisions, but cannot
erase packages or plaintext already retained by a recipient. An independent
cryptographic and authorization review is required before production use.
