//! The wire contract between a client worker and the server's `/api/work/*`.
//!
//! Both ends live in this crate — the server's handlers and the client's
//! [`crate::remote::RemoteJobSource`] — so the shapes are defined once and a
//! field renamed on one side cannot silently break the other. Everything is
//! JSON except the two byte streams: a review frame going up and an original
//! file coming down.
//!
//! A worker authenticates every call with its machine token
//! ([`MACHINE_TOKEN_HEADER`]), never with a person's session: the laptop
//! keeps contributing after its owner signs out. Calls about one job also
//! carry that job's lease token ([`LEASE_TOKEN_HEADER`]), the same fencing
//! token the database gate checks, so a result from a lapsed lease is
//! refused exactly as it would be locally.

use serde::{Deserialize, Serialize};
use skwad_database::models::{Job, JobState, Media};
use skwad_database::repo::machines::{Capabilities, Machine};

use crate::analysis::AnalysisOutput;
use crate::models::ModelInfo;
use crate::settings::LibrarySettings;

/// The API contract version every `/api` request names in `X-Skwad-Api`.
pub const API_VERSION: u32 = 1;

pub const MACHINE_TOKEN_HEADER: &str = "x-skwad-machine-token";
pub const LEASE_TOKEN_HEADER: &str = "x-skwad-lease";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRequest {
    pub capabilities: Capabilities,
}

/// A job plus everything a worker without the database needs to run it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimedJob {
    pub job: Job,
    pub media: Media,
    /// The file as reachable through the shoot's share mapping, when the
    /// shoot has one. A worker tries this before asking for a download.
    pub client_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimResponse {
    pub job: Option<ClaimedJob>,
    /// Changes whenever the library-wide settings do; the worker refetches
    /// them when it sees a new value.
    pub library_version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRequest {
    pub token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeartbeatStatus {
    Alive,
    Cancelled,
    LeaseLost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatResponse {
    pub status: HeartbeatStatus,
    pub library_version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsResponse {
    pub library_version: u64,
    pub settings: LibrarySettings,
    /// Content hashes of the model pair the server analyses with. A worker
    /// must run the same pair or its vectors would not be comparable.
    pub detector_hash: Option<String>,
    pub embedder_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultRequest {
    pub token: String,
    pub output: AnalysisOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleResponse {
    /// False when the lease had lapsed: nothing was recorded.
    pub settled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailRequest {
    pub token: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailResponse {
    /// `None` when the lease had lapsed.
    pub state: Option<JobState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRequest {
    pub token: String,
    /// Why this worker could not run the job, when that is the reason it is
    /// being handed back. Shown against the shoot, named after the machine.
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsResponse {
    /// The detector and embedder the server runs, in that order when both
    /// resolve. Each is fetchable at `/api/models/{hash}`.
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrolRequest {
    pub name: String,
    pub machine_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrolResponse {
    pub machine: Machine,
    /// Shown once. The server keeps only its hash.
    pub token: String,
}
