use std::fmt;

use serde::{Deserialize, Serialize};

/// This OEM specific extension is mainly applicable for querying chassis information for the ERoT subsystem
/// odata_type is always present regardless of the subsystem we are querying for (Bluefield_BMC, Bluefield_ERoT, or Card1)
/// the remaining attributes are only present when querying the Bluefield_ERoT
/// Due to the indistinguishable names, this is used for DPUs, GB200, and potentially others; comments describe
/// what platforms it may be expected on.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ChassisExtensions {
    #[serde(rename = "@odata.type")]
    pub odata_type: String,
    pub automatic_background_copy_enabled: Option<bool>, // DPU
    pub background_copy_status: Option<BackgroundCopyStatus>, // DPU
    pub inband_update_policy_enabled: Option<bool>,      // DPU
    pub chassis_physical_slot_number: Option<i32>,       // GB200
    pub compute_tray_index: Option<i32>,                 // GB200
    pub topology_id: Option<i32>,                        // GB200
    pub revision_id: Option<i32>,                        // GB200
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
