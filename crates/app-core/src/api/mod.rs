//! The command layer, written once for both front doors.
//!
//! Every command the React frontend can ask for is a plain function here:
//! `fn name(ctx: &Ctx, args…) -> Result<T>`. The Tauri crate wraps each one as
//! an IPC command; the HTTP server dispatches to the same functions by name.
//! Neither front door contains any logic of its own — a command added here
//! and forgotten by a front door is a compile error, because both are
//! generated from the one [`command_registry!`] table rather than maintained
//! twice.
//!
//! [`Ctx`] is everything a command may need from the outside: the shared
//! state, the event sink, who is calling, and where that caller's device
//! identity is kept. Nothing here knows whether the call came over IPC or
//! HTTP.

pub mod catalogue;
pub mod commands;
pub mod machines;
pub mod roster;
pub mod storage;

use std::sync::Arc;

use serde::Serialize;

use crate::progress::ProgressSink;
use crate::state::AppState;

pub use catalogue::{StoredIdentity, UserRole};

/// Why a command was refused, for the front door to turn into a status code.
/// The Tauri bridge does not care; the HTTP server does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The request cannot be satisfied as written — the caller can fix it.
    BadRequest,
    /// Nobody is signed in.
    Unauthorized,
    /// Signed in, but not allowed to do this.
    Forbidden,
    NotFound,
    /// Ours to fix.
    Internal,
}

/// Errors cross the front door as a plain message; the UI shows it verbatim,
/// so the text has to be something a person can act on.
///
/// Deliberately does not implement `Display`: the blanket `From<E: Display>`
/// below is what lets every `?` in the command bodies convert, and a `Display`
/// impl here would make that overlap with the reflexive `From<T> for T`.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub kind: ErrorKind,
    pub message: String,
}

impl Serialize for ApiError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ApiError", 1)?;
        s.serialize_field("message", &self.message)?;
        s.end()
    }
}

impl ApiError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Forbidden, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }
}

/// Anything that can describe itself becomes an internal error. The command
/// bodies rely on this for every `?` over a database, I/O or pipeline error;
/// refusals the caller can act on are built with the constructors above.
impl<E: std::fmt::Display> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self::internal(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Who is calling. `None` is a caller that has not signed in; commands that
/// need a person call [`Ctx::require_user`].
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub user: Option<SessionUser>,
    /// Keeps one caller's in-memory state — the catalogues they opened —
    /// apart from everyone else's on a shared server. `None` on the desktop,
    /// where one process is one person.
    pub scope: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionUser {
    pub account_id: String,
    pub email: String,
    pub display_name: String,
}

/// Where a caller's device identity — its catalogue keypair and the signing
/// keys it trusts — is kept. The desktop keeps it in the operating-system
/// credential store; a server keeps one per account under its own control.
/// Each front door constructs a store already bound to the calling principal.
pub trait IdentityStore: Send + Sync {
    /// Errors when nobody is signed in on this store.
    fn load(&self) -> Result<StoredIdentity>;
    fn save(&self, identity: &StoredIdentity) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

/// An identity store with nothing in it, for callers that cannot sign in.
pub struct NoIdentity;

impl IdentityStore for NoIdentity {
    fn load(&self) -> Result<StoredIdentity> {
        Err(ApiError::unauthorized("sign in first"))
    }

    fn save(&self, _identity: &StoredIdentity) -> Result<()> {
        Err(ApiError::unauthorized("this caller cannot hold a device identity"))
    }

    fn clear(&self) -> Result<()> {
        Ok(())
    }
}

/// Everything a command may reach for.
#[derive(Clone)]
pub struct Ctx {
    pub state: Arc<AppState>,
    pub sink: Arc<dyn ProgressSink>,
    pub session: Session,
    pub identity: Arc<dyn IdentityStore>,
}

impl Ctx {
    /// A context with no caller and no front end listening — for background
    /// work and tests.
    pub fn headless(state: Arc<AppState>) -> Self {
        Self {
            state,
            sink: crate::progress::null_sink(),
            session: Session::default(),
            identity: Arc::new(NoIdentity),
        }
    }

    pub fn require_user(&self) -> Result<&SessionUser> {
        self.session
            .user
            .as_ref()
            .ok_or_else(|| ApiError::unauthorized("sign in first"))
    }
}

/// The one table both front doors are generated from.
///
/// `name(args) -> Return = implementation;` per command. The macro passed in
/// receives the whole table and expands it however that front door needs:
/// the core builds [`dispatch`] from it, the Tauri crate builds one IPC
/// command per row, and the HTTP server hands names to [`dispatch`]. Adding
/// a command means adding one row here.
#[macro_export]
macro_rules! command_registry {
    ($callback:ident) => {
        $callback! {
            // --- application ---------------------------------------------
            app_info() -> AppInfo = $crate::api::commands::app_info;
            get_settings() -> AppSettings = $crate::api::commands::get_settings;
            update_settings(settings: AppSettings) -> AppSettings = $crate::api::commands::update_settings;
            model_status() -> ModelStatus = $crate::api::commands::model_status;
            embedding_cohorts(shoot_id: Option<i64>) -> EmbeddingCohorts = $crate::api::commands::embedding_cohorts;
            reembed_stale_faces(shoot_id: Option<i64>) -> usize = $crate::api::commands::reembed_stale_faces;
            list_machines() -> Vec<MachineRosterEntry> = $crate::api::machines::list_machines;
            enrol_machine(name: String, machine_id: String) -> EnrolResponse = $crate::api::machines::enrol_machine;
            revoke_machine(machine_id: String) -> bool = $crate::api::machines::revoke_machine;
            list_projects() -> Vec<Project> = $crate::api::commands::list_projects;
            save_project(project: Project) -> Project = $crate::api::commands::save_project;
            delete_project(project_id: String) -> () = $crate::api::commands::delete_project;
            replace_project_members(project_id: String, members: Vec<ProjectMember>) -> Project = $crate::api::commands::replace_project_members;
            // --- accounts and catalogues ---------------------------------
            catalogue_session_status() -> SessionStatus = $crate::api::catalogue::catalogue_session_status;
            sign_in_skwad(email: String, password: String) -> SessionStatus = $crate::api::catalogue::sign_in_skwad;
            change_initial_password(email: String, current_password: String, new_password: String) -> SessionStatus = $crate::api::catalogue::change_initial_password;
            list_local_users() -> Vec<LocalUser> = $crate::api::catalogue::list_local_users;
            create_local_user(user: NewLocalUser) -> Vec<LocalUser> = $crate::api::catalogue::create_local_user;
            update_local_user(user: LocalUserUpdate) -> Vec<LocalUser> = $crate::api::catalogue::update_local_user;
            reset_local_user_password(email: String, password: String) -> Vec<LocalUser> = $crate::api::catalogue::reset_local_user_password;
            delete_local_user(email: String) -> Vec<LocalUser> = $crate::api::catalogue::delete_local_user;
            sign_out_skwad() -> () = $crate::api::catalogue::sign_out_skwad;
            clear_authenticated_session() -> () = $crate::api::catalogue::clear_authenticated_session;
            get_user_profile() -> UserProfile = $crate::api::catalogue::get_user_profile;
            update_user_profile(update: ProfileUpdate) -> UserProfile = $crate::api::catalogue::update_user_profile;
            publish_skwad(shoot_id: i64, destination: String, passphrase: String) -> PublishResult = $crate::api::catalogue::publish_skwad;
            load_skwad(path: String, passphrase: Option<String>) -> LoadedCatalogueInfo = $crate::api::catalogue::load_skwad;
            approve_catalogue_library(package_id: String, revision_id: String, root: String) -> LoadedCatalogueInfo = $crate::api::catalogue::approve_catalogue_library;
            list_loaded_catalogues() -> Vec<LoadedCatalogueInfo> = $crate::api::catalogue::list_loaded_catalogues;
            list_catalogue_groups(package_id: String, revision_id: String) -> Vec<CatalogueGroup> = $crate::api::catalogue::list_catalogue_groups;
            list_catalogue_media(package_id: String, revision_id: String, group_id: Option<i64>) -> Vec<CatalogueMedia> = $crate::api::catalogue::list_catalogue_media;
            resolve_catalogue_media(package_id: String, revision_id: String, media_id: i64) -> String = $crate::api::catalogue::resolve_catalogue_media;
            // --- team rosters --------------------------------------------
            preview_roster_file(path: String) -> RosterPreview = $crate::api::roster::preview_roster_file;
            preview_roster_text(source: String, text: String) -> RosterPreview = $crate::api::roster::preview_roster_text;
            import_roster(source: String, entries: Vec<RosterEntry>) -> RosterSummary = $crate::api::roster::import_roster;
            roster_summary() -> RosterSummary = $crate::api::roster::roster_summary;
            list_roster() -> Vec<RosterEntry> = $crate::api::roster::list_roster;
            search_roster(query: String, limit: Option<usize>) -> Vec<RosterEntry> = $crate::api::roster::search_roster;
            resolve_roster_name(name: String) -> Option<RosterEntry> = $crate::api::roster::resolve_roster_name;
            clear_roster(source: Option<String>) -> RosterSummary = $crate::api::roster::clear_roster;
            // --- shoots --------------------------------------------------
            list_shoots() -> Vec<ShootSummary> = $crate::api::commands::list_shoots;
            get_shoot(shoot_id: i64) -> Option<ShootSummary> = $crate::api::commands::get_shoot;
            create_shoot(name: String, source_path: String) -> Shoot = $crate::api::commands::create_shoot;
            rename_shoot(shoot_id: i64, name: String) -> () = $crate::api::commands::rename_shoot;
            delete_shoot_index(shoot_id: i64) -> () = $crate::api::commands::delete_shoot_index;
            clear_selected_scanned_data(shoot_ids: Vec<i64>) -> usize = $crate::api::commands::clear_selected_scanned_data;
            clear_scanned_data() -> usize = $crate::api::commands::clear_scanned_data;
            resume_processing(shoot_id: i64) -> usize = $crate::api::commands::resume_processing;
            pause_processing(shoot_id: i64, paused: bool) -> bool = $crate::api::commands::pause_processing;
            cancel_processing(shoot_id: i64) -> usize = $crate::api::commands::cancel_processing;
            reanalyse_shoot(shoot_id: i64) -> usize = $crate::api::commands::reanalyse_shoot;
            get_progress(shoot_id: i64) -> ProcessingProgress = $crate::api::commands::get_progress;
            get_shoot_telemetry(shoot_id: i64) -> ShootTelemetry = $crate::api::commands::get_shoot_telemetry;
            get_shoot_storage(shoot_id: i64) -> ShootStorage = $crate::api::storage::get_shoot_storage;
            list_failed_jobs(shoot_id: i64) -> Vec<Job> = $crate::api::commands::list_failed_jobs;
            // --- media ---------------------------------------------------
            list_media(query: MediaQuery) -> Vec<Media> = $crate::api::commands::list_media;
            get_media(media_id: i64) -> Option<Media> = $crate::api::commands::get_media;
            media_faces(media_id: i64) -> Vec<Face> = $crate::api::commands::media_faces;
            set_media_editorial(media_ids: Vec<i64>, rating: Option<i64>, pick_state: Option<String>) -> usize = $crate::api::commands::set_media_editorial;
            // --- players -------------------------------------------------
            list_people(shoot_id: Option<i64>) -> Vec<PersonSummary> = $crate::api::commands::list_people;
            list_enrolled_people() -> Vec<PersonSummary> = $crate::api::commands::list_enrolled_people;
            reference_library_shoot_id() -> Option<i64> = $crate::api::commands::reference_library_shoot_id;
            create_person(name: String, team: Option<String>) -> Person = $crate::api::commands::create_person;
            enroll_person(name: String, team: Option<String>, photo_paths: Vec<String>, video_path: Option<String>) -> EnrollPersonResult = $crate::api::commands::enroll_person;
            enroll_people_from_directory(root: String, team: Option<String>) -> EnrollDirectoryResult = $crate::api::commands::enroll_people_from_directory;
            find_person_media(person_id: i64, shoot_id: Option<i64>) -> MatchPersonReport = $crate::api::commands::find_person_media;
            rename_person(person_id: i64, name: String) -> () = $crate::api::commands::rename_person;
            update_person(person_id: i64, team: Option<String>, notes: Option<String>) -> () = $crate::api::commands::update_person;
            merge_people(target_id: i64, source_id: i64) -> i64 = $crate::api::commands::merge_people;
            delete_person(person_id: i64) -> () = $crate::api::commands::delete_person;
            clear_person_recognition(person_id: i64) -> () = $crate::api::commands::clear_person_recognition;
            // --- clusters ------------------------------------------------
            list_clusters(shoot_id: i64, include_named: bool) -> Vec<ClusterSummary> = $crate::api::commands::list_clusters;
            name_cluster(cluster_id: i64, name: String, team: Option<String>) -> Person = $crate::api::commands::name_cluster;
            merge_clusters(target_id: i64, source_id: i64) -> () = $crate::api::commands::merge_clusters;
            split_cluster(cluster_id: i64, face_ids: Vec<i64>, label: Option<String>) -> i64 = $crate::api::commands::split_cluster;
            ignore_cluster(cluster_id: i64) -> () = $crate::api::commands::ignore_cluster;
            // --- albums --------------------------------------------------
            list_albums(shoot_id: i64) -> Vec<Album> = $crate::api::commands::list_albums;
            regenerate_albums(shoot_id: i64) -> usize = $crate::api::commands::regenerate_albums;
            // --- groups (the editor's own sorting) -----------------------
            list_groups(shoot_id: i64) -> Vec<Group> = $crate::api::commands::list_groups;
            group_stats(shoot_id: i64) -> GroupStats = $crate::api::commands::group_stats;
            group_links(shoot_id: i64) -> Vec<MediaGroupLink> = $crate::api::commands::group_links;
            create_group(shoot_id: i64, name: String) -> Group = $crate::api::commands::create_group;
            rename_group(group_id: i64, name: String) -> Group = $crate::api::commands::rename_group;
            update_group(group_id: i64, folder_name: Option<String>, notes: Option<String>) -> Group = $crate::api::commands::update_group;
            delete_group(group_id: i64) -> () = $crate::api::commands::delete_group;
            add_media_to_group(shoot_id: i64, group_id: Option<i64>, group_name: Option<String>, media_ids: Vec<i64>, move_files: bool) -> usize = $crate::api::commands::add_media_to_group;
            remove_media_from_group(group_id: i64, media_ids: Vec<i64>) -> usize = $crate::api::commands::remove_media_from_group;
            clear_group(group_id: i64) -> usize = $crate::api::commands::clear_group;
            groups_from_ai_albums(shoot_id: i64) -> SeedResult = $crate::api::commands::groups_from_ai_albums;
            group_from_album(album_id: i64, name: Option<String>) -> Group = $crate::api::commands::group_from_album;
            // --- review --------------------------------------------------
            list_faces(query: FaceQuery) -> Vec<FaceWithContext> = $crate::api::commands::list_faces;
            confirm_faces(face_ids: Vec<i64>) -> usize = $crate::api::commands::confirm_faces;
            reject_faces(face_ids: Vec<i64>) -> usize = $crate::api::commands::reject_faces;
            assign_faces(face_ids: Vec<i64>, person_id: Option<i64>, person_name: Option<String>) -> usize = $crate::api::commands::assign_faces;
            ignore_faces(face_ids: Vec<i64>) -> usize = $crate::api::commands::ignore_faces;
            add_manual_face(media_id: i64, bbox: BoundingBox, frame_time: Option<f64>) -> ManualFaceResult = $crate::api::commands::add_manual_face;
            name_face(face_id: i64, name: String, team: Option<String>) -> NameFaceResult = $crate::api::commands::name_face;
            // --- video ---------------------------------------------------
            video_timelines(media_id: i64) -> Vec<VideoTimeline> = $crate::api::commands::video_timelines;
            video_sample_frames(media_id: i64) -> Vec<f64> = $crate::api::commands::video_sample_frames;
            // --- export --------------------------------------------------
            preview_export(shoot_id: i64, destination: String, options: ExportOptions) -> ExportPreview = $crate::api::commands::preview_export;
            start_export(shoot_id: i64, destination: String, options: ExportOptions) -> i64 = $crate::api::commands::start_export;
            cancel_export(shoot_id: i64) -> () = $crate::api::commands::cancel_export;
            list_exports(shoot_id: i64) -> Vec<ExportRecord> = $crate::api::commands::list_exports;
            // --- premiere ------------------------------------------------
            send_media_to_premiere(media_ids: Vec<i64>, label: Option<String>) -> () = $crate::api::commands::send_media_to_premiere;
            send_collection_to_premiere(collection_id: String) -> () = $crate::api::commands::send_collection_to_premiere;
            // --- logs and privacy ----------------------------------------
            recent_logs(shoot_id: Option<i64>, limit: i64) -> Vec<LogEntry> = $crate::api::commands::recent_logs;
            clear_all_embeddings() -> usize = $crate::api::commands::clear_all_embeddings;
            clear_all_recognition_data() -> () = $crate::api::commands::clear_all_recognition_data;
            clear_thumbnail_cache() -> u64 = $crate::api::commands::clear_thumbnail_cache;
            clear_log() -> () = $crate::api::commands::clear_log;
        }
    };
}

/// Builds [`dispatch`] and [`COMMANDS`] from the registry: a command by name
/// with JSON arguments in, JSON out. This is the whole HTTP command surface.
macro_rules! define_dispatch {
    ($( $name:ident ( $($arg:ident : $ty:ty),* ) -> $ret:ty = $path:path ; )*) => {
        /// Every command name, for a front door to check coverage against.
        pub const COMMANDS: &[&str] = &[ $( stringify!($name) ),* ];

        /// Runs `command` with JSON `args` (the same camelCase object the
        /// Tauri bridge sends) and returns its JSON result.
        pub fn dispatch(ctx: &Ctx, command: &str, args: serde_json::Value) -> Result<serde_json::Value> {
            let args = if args.is_null() {
                serde_json::Value::Object(Default::default())
            } else {
                args
            };
            match command {
                $( stringify!($name) => {
                    #[allow(non_camel_case_types, dead_code)]
                    #[derive(serde::Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Args { $( $arg: $ty ),* }
                    let Args { $( $arg ),* } = serde_json::from_value(args).map_err(|e| {
                        ApiError::bad_request(format!("invalid arguments for `{}`: {e}", stringify!($name)))
                    })?;
                    let out: $ret = $path(ctx $(, $arg)*)?;
                    serde_json::to_value(out).map_err(|e| ApiError::internal(e.to_string()))
                } )*
                _ => Err(ApiError::not_found(format!("unknown command `{command}`"))),
            }
        }
    };
}

mod generated {
    #![allow(unused_imports)]
    use super::catalogue::*;
    use super::commands::*;
    use super::machines::*;
    use super::roster::*;
    use super::storage::*;
    use super::{ApiError, Ctx, Result};
    use crate::models::ModelStatus;
    use crate::settings::AppSettings;
    use skwad_catalogue::{CatalogueGroup, CatalogueMedia};
    use skwad_database::models::*;
    use skwad_database::repo::roster::RosterEntry;
    use skwad_export_engine::ExportOptions;

    crate::command_registry!(define_dispatch);
}

pub use generated::{dispatch, COMMANDS};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_has_no_duplicate_names() {
        let mut names = COMMANDS.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len(), "a command is registered twice");
        assert!(COMMANDS.len() > 100, "the registry looks truncated: {}", COMMANDS.len());
    }

    #[test]
    fn errors_serialise_as_a_bare_message() {
        let error = ApiError::bad_request("give the shoot a name");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({ "message": "give the shoot a name" })
        );
    }

    #[test]
    fn an_unknown_command_is_refused_before_touching_state() {
        // `dispatch` needs a `Ctx`; an unknown name must fail before any of
        // it is used, so a bogus context is fine here.
        let db = skwad_database::Database::open_test().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::create(temp.path()).unwrap();
        let state = Arc::new(AppState::new(
            db,
            paths,
            crate::settings::AppSettings::default(),
            "skwadmedia://".into(),
            "test",
            temp.path().join("machine.json"),
        ));
        let ctx = Ctx::headless(state);
        let error = dispatch(&ctx, "not_a_command", serde_json::Value::Null).unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);

        // And a real one round-trips through JSON.
        let shoots = dispatch(&ctx, "list_shoots", serde_json::Value::Null).unwrap();
        assert_eq!(shoots, serde_json::json!([]));

        let bad = dispatch(&ctx, "get_shoot", serde_json::json!({ "shootId": "seven" })).unwrap_err();
        assert_eq!(bad.kind, ErrorKind::BadRequest);
    }
}
