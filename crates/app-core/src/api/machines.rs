//! Enrolling, listing and revoking the machines that work for this library.
//!
//! Enrolment is an administrator's act — the server's front door gates
//! `enrol_machine` and `revoke_machine` behind the admin role and audits
//! them — and hands out the one thing the worker keeps: its token. Only the
//! token's hash is stored, so the roster cannot leak a credential.

pub use skwad_database::repo::machines::MachineRosterEntry;
use skwad_database::repo::machines::{self, Capabilities};

pub use crate::work_api::EnrolResponse;
use crate::api::{ApiError, Ctx, Result};

/// Every enrolled machine and what it is doing.
pub fn list_machines(ctx: &Ctx) -> Result<Vec<MachineRosterEntry>> {
    let mut conn = ctx.state.db.conn()?;
    Ok(machines::roster(&mut conn)?)
}

/// Enrols (or re-enrols, with a fresh token) the machine `machine_id`. The
/// token in the answer is shown once.
pub fn enrol_machine(ctx: &Ctx, name: String, machine_id: String) -> Result<EnrolResponse> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("give the machine a name people will recognise"));
    }
    let machine_id = machine_id.trim();
    if machine_id.is_empty()
        || machine_id.len() > 64
        || !machine_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(ApiError::bad_request("the machine id must be the installation's own id"));
    }
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let enrolled_by = ctx.session.user.as_ref().map(|user| user.account_id.clone());
    let mut conn = ctx.state.db.conn()?;
    let machine = machines::enrol(
        &mut conn,
        machine_id,
        name,
        &token,
        enrolled_by.as_deref(),
        &Capabilities::default(),
    )?;
    tracing::info!(machine = %machine.id, name = %machine.name, "machine enrolled as a worker");
    Ok(EnrolResponse { machine, token })
}

/// Stops a machine's token working at its next claim. Jobs it holds finish
/// or lapse as usual.
pub fn revoke_machine(ctx: &Ctx, machine_id: String) -> Result<bool> {
    let mut conn = ctx.state.db.conn()?;
    let revoked = machines::revoke(&mut conn, machine_id.trim())?;
    if revoked {
        tracing::info!(machine = %machine_id, "machine revoked");
    }
    Ok(revoked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Ctx;
    use crate::state::AppState;
    use skwad_database::Db;
    use std::sync::Arc;

    fn test_ctx() -> Ctx {
        let temp = std::env::temp_dir().join(format!("skwad-machines-{}", std::process::id()));
        let paths = crate::paths::AppPaths::create(&temp).unwrap();
        Ctx::headless(Arc::new(AppState::new(
            skwad_database::Database::open_test().unwrap(),
            paths,
            crate::settings::AppSettings::default(),
            "skwadmedia://".into(),
            "server-test",
            temp.join(format!("machine-{}.json", uuid::Uuid::new_v4())),
        )))
    }

    #[test]
    fn enrolment_returns_a_token_that_is_only_ever_stored_hashed() {
        let ctx = test_ctx();
        let enrolled = enrol_machine(&ctx, " Editing laptop ".into(), "abc-123".into()).unwrap();
        assert_eq!(enrolled.machine.name, "Editing laptop");
        assert_eq!(enrolled.token.len(), 64);

        let mut conn = ctx.state.db.conn().unwrap();
        let by_token = machines::authenticate(&mut conn, &enrolled.token).unwrap().unwrap();
        assert_eq!(by_token.id, "abc-123");
        let stored: String = conn
            .row_one(
                "SELECT token_hash FROM machines WHERE id = 'abc-123'",
                skwad_database::params![],
            )
            .unwrap()
            .get(0);
        assert_ne!(stored, enrolled.token);

        assert_eq!(list_machines(&ctx).unwrap().len(), 1);
        assert!(revoke_machine(&ctx, "abc-123".into()).unwrap());
        assert!(machines::authenticate(&mut conn, &enrolled.token).unwrap().is_none());

        assert!(enrol_machine(&ctx, "".into(), "abc-123".into()).is_err());
        assert!(enrol_machine(&ctx, "x".into(), "has spaces".into()).is_err());
    }
}
