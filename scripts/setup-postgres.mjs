#!/usr/bin/env node
// Provisions the local PostgreSQL server SKWAD needs: the `skwad` role, the
// `skwad` and `skwad_test` databases, and the password file the app and the
// command-line tools both read.
//
// Idempotent — safe to re-run. It never touches an existing database's
// contents; it only creates what is missing.
//
//   node scripts/setup-postgres.mjs                 # provision
//   node scripts/setup-postgres.mjs --check         # report, change nothing
//
// The superuser password is read from PGPASSWORD, or --superpassword, so it
// never has to appear in a shell history.

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const APP_USER = "skwad";
const APP_DBS = ["skwad", "skwad_test"];
const HOST = process.env.SKWAD_PGHOST ?? "localhost";
const PORT = process.env.SKWAD_PGPORT ?? "5432";

const args = process.argv.slice(2);
const checkOnly = args.includes("--check");
const superUser = valueOf("--superuser") ?? process.env.PGSUPERUSER ?? "postgres";
const superPassword = valueOf("--superpassword") ?? process.env.PGPASSWORD;
const DEV_PASSWORD = "skwad_dev";
const appPassword = valueOf("--password") ?? process.env.SKWAD_DATABASE_PASSWORD ?? DEV_PASSWORD;
const usingDevPassword = appPassword === DEV_PASSWORD;

function valueOf(flag) {
  const index = args.indexOf(flag);
  return index >= 0 ? args[index + 1] : undefined;
}

/** Finds `psql`, preferring PATH and falling back to a default install. */
function findPsql() {
  const candidates = [
    process.env.PSQL,
    "psql",
    ...(process.platform === "win32"
      ? ["17", "16", "15"].map((v) => `C:\\Program Files\\PostgreSQL\\${v}\\bin\\psql.exe`)
      : ["/usr/bin/psql", "/usr/local/bin/psql", "/opt/homebrew/bin/psql"]),
  ].filter(Boolean);

  for (const candidate of candidates) {
    try {
      execFileSync(candidate, ["--version"], { stdio: "ignore" });
      return candidate;
    } catch {
      // Try the next one.
    }
  }
  fail(
    "could not find `psql`.\n" +
      "Install PostgreSQL 15 or newer, or set PSQL to its path.\n" +
      "  Windows:  winget install PostgreSQL.PostgreSQL.17\n" +
      "  macOS:    brew install postgresql@17 && brew services start postgresql@17\n" +
      "  Linux:    sudo apt install postgresql",
  );
}

function fail(message) {
  console.error(`\n${message}\n`);
  process.exit(1);
}

const psql = findPsql();

/** Runs one statement as the superuser against `database`. */
function sql(database, statement, { quiet = false } = {}) {
  try {
    return execFileSync(psql, ["-U", superUser, "-h", HOST, "-p", PORT, "-d", database, "-tAc", statement], {
      encoding: "utf8",
      env: { ...process.env, ...(superPassword ? { PGPASSWORD: superPassword } : {}) },
      stdio: quiet ? ["ignore", "pipe", "ignore"] : ["ignore", "pipe", "pipe"],
    }).trim();
  } catch (error) {
    const detail = error.stderr?.toString().trim() || error.message;
    fail(
      `psql failed against ${HOST}:${PORT}:\n  ${detail}\n\n` +
        "Is the server running, and is the superuser password right?\n" +
        "Pass it with --superpassword, or set PGPASSWORD.",
    );
  }
}

console.log(`PostgreSQL at ${HOST}:${PORT} via ${psql}`);
console.log(sql("postgres", "SELECT version()").split(",")[0]);

// ICU is what the `nocase` collation is built on — without it the schema will
// not apply, so say so here rather than at the first migration.
const icu = sql("postgres", "SELECT count(*) FROM pg_collation WHERE collprovider = 'i'");
if (Number(icu) === 0) {
  fail("this server has no ICU collations, which the `nocase` collation needs.\nInstall a PostgreSQL build with ICU support.");
}
console.log("ICU collations: available");

if (checkOnly) {
  const role = sql("postgres", `SELECT count(*) FROM pg_roles WHERE rolname = '${APP_USER}'`);
  console.log(`role ${APP_USER}: ${Number(role) ? "present" : "MISSING"}`);
  for (const db of APP_DBS) {
    const exists = sql("postgres", `SELECT count(*) FROM pg_database WHERE datname = '${db}'`);
    console.log(`database ${db}: ${Number(exists) ? "present" : "MISSING"}`);
  }
  process.exit(0);
}

// --- role ------------------------------------------------------------------
// NOSUPERUSER/NOCREATEDB deliberately: the app only reads and writes inside its
// own database.
const roleExists = Number(sql("postgres", `SELECT count(*) FROM pg_roles WHERE rolname = '${APP_USER}'`));
if (roleExists) {
  console.log(`role ${APP_USER}: already exists (password left alone)`);
} else {
  sql(
    "postgres",
    `CREATE ROLE ${APP_USER} LOGIN PASSWORD '${appPassword.replace(/'/g, "''")}' NOSUPERUSER NOCREATEDB NOCREATEROLE`,
  );
  console.log(`role ${APP_USER}: created`);
}

// --- databases -------------------------------------------------------------
for (const db of APP_DBS) {
  const exists = Number(sql("postgres", `SELECT count(*) FROM pg_database WHERE datname = '${db}'`));
  if (exists) {
    console.log(`database ${db}: already exists (left alone)`);
  } else {
    sql("postgres", `CREATE DATABASE ${db} OWNER ${APP_USER}`);
    console.log(`database ${db}: created`);
  }
}

// --- password file ---------------------------------------------------------
// libpq's standard location, which `psql`, the migration tool and the app all
// read. Keeping the credential here is what lets `database.json` stay free of
// one, so a shared library folder never carries a password.
const pgpassPath =
  process.env.PGPASSFILE ??
  (process.platform === "win32"
    ? join(process.env.APPDATA ?? "", "postgresql", "pgpass.conf")
    : join(process.env.HOME ?? "", ".pgpass"));

mkdirSync(dirname(pgpassPath), { recursive: true });
const wanted = APP_DBS.map((db) => `${HOST}:${PORT}:${db}:${APP_USER}:${appPassword}`);
const existing = existsSync(pgpassPath) ? readFileSync(pgpassPath, "utf8").split(/\r?\n/) : [];
const kept = existing.filter(
  (line) => line.trim() !== "" && !APP_DBS.some((db) => line.startsWith(`${HOST}:${PORT}:${db}:${APP_USER}:`)),
);
writeFileSync(pgpassPath, [...kept, ...wanted].join("\n") + "\n", { mode: 0o600 });
console.log(`password file: ${pgpassPath}`);

if (process.platform !== "win32") {
  // libpq refuses to read a group- or world-readable password file.
  execFileSync("chmod", ["600", pgpassPath]);
}

if (usingDevPassword) {
  console.warn(`
WARNING: the '${APP_USER}' role was given the built-in development password.
That is fine for a laptop talking to localhost. On anything another machine can
reach, set a real one instead:

  npm run db:setup -- --password '<a real password>'
  # or: SKWAD_DATABASE_PASSWORD='<a real password>' npm run db:setup

For an existing role, change it in psql:
  ALTER ROLE ${APP_USER} PASSWORD '<a real password>';
…then re-run this script so the password file matches.`);
}

console.log(`
Done. Next:
  npm run db:migrate -- --sqlite <path to media.db>   # bring an old library across
  npm run dev                                          # the app now opens the Postgres library

The app reads connection details from database.json in the library folder and
the password from the file above; SKWAD_DATABASE_URL overrides both.`);
