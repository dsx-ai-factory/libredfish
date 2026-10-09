use std::fmt;

use serde::{Deserialize, Serialize};

/// This OEM specific extension is mainly applicable for querying chassis information for the ERoT subsystem
/// odata_type is always present regardless of the subsystem we are querying for (Bluefield_BMC, Bluefield_ERoT, or Card1)
/// the remaining attributes are only present when querying the Bluefield_ERoT
/// Due to the indistinguishable names, this is used for DPUs, GB200, and potentially others; comments describe
/// what platforms it may be expected on.
// `Default` + per-field `skip_serializing_if` let this read model double as a
// minimal partial PATCH body: construct it with `..Default::default()` and only
// the fields that are set serialize, leaving the rest of the resource untouched.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ChassisExtensions {
    #[serde(
        rename = "@odata.type",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub odata_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_background_copy_enabled: Option<bool>, // DPU
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_copy_status: Option<BackgroundCopyStatus>, // DPU
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inband_update_policy_enabled: Option<bool>, // DPU
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chassis_physical_slot_number: Option<i32>, // GB200
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_tray_index: Option<i32>, // GB200
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology_id: Option<i32>, // GB200
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<i32>, // GB200
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_management_network_access: Option<bool>, // VR72 host BMC (SMM)
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone, Eq, PartialEq)]
pub enum BackgroundCopyStatus {
    InProgress,
    Completed,
    Pending,
}

impl fmt::Display for BackgroundCopyStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
