use std::{env, fs, path::PathBuf};

use argon2::{
    password_hash::{PasswordHasher, SaltString},
    Argon2,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialFile {
    version: u32,
    users: Vec<Credential>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Credential {
    id: String,
    email: String,
    display_name: String,
    password_hash: String,
    enabled: bool,
    must_change_password: bool,
}

fn main() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let mut file = None;
    let mut email = None;
    let mut display_name = None;
    while let Some(argument) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value after {argument}"))?;
        match argument.as_str() {
            "--file" => file = Some(PathBuf::from(value)),
            "--email" => email = Some(value),
            "--display-name" => display_name = Some(value),
            _ => return Err(format!("unknown option: {argument}")),
        }
    }
    let file = file.ok_or("--file is required")?;
    let email = email.ok_or("--email is required")?.trim().to_lowercase();
    let display_name = display_name.ok_or("--display-name is required")?.trim().to_owned();
    if email.is_empty() || !email.contains('@') || display_name.is_empty() {
        return Err("provide a valid email and display name".into());
    }
    let password = Zeroizing::new(rpassword::prompt_password("Temporary password: ").map_err(|e| e.to_string())?);
    if password.chars().count() < 6 {
        return Err("temporary password must contain at least 6 characters".into());
    }
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|e| e.to_string())?;
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| e.to_string())?
        .to_string();

    let mut document = if file.is_file() {
        serde_json::from_slice::<CredentialFile>(&fs::read(&file).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid existing credential file: {e}"))?
    } else {
        CredentialFile {
            version: 1,
            users: Vec::new(),
        }
    };
    if document.version != 1 {
        return Err(format!("unsupported credential file version {}", document.version));
    }
    if let Some(existing) = document
        .users
        .iter_mut()
        .find(|user| user.email.eq_ignore_ascii_case(&email))
    {
        existing.display_name = display_name;
        existing.password_hash = password_hash;
        existing.enabled = true;
        existing.must_change_password = true;
    } else {
        document.users.push(Credential {
            id: Uuid::new_v4().to_string(),
            email,
            display_name,
            password_hash,
            enabled: true,
            must_change_password: true,
        });
    }
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = file.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::copy(&temporary, &file).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(temporary);
    println!(
        "Updated {} local account(s) in {}",
        document.users.len(),
        file.display()
    );
    Ok(())
}
