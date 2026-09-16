/*
 * SPDX-FileCopyrightText: Copyright (c) 2025 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 */
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use serde_with::DefaultOnNull;

use super::{boot::Boot, oem::SystemExtensions, OData, ODataId, ODataLinks, RedfishSettings};

const MELLANOX_VENDOR_ID_HEX: &str = "0X15B3";
const MELLANOX_VENDOR_ID_DEC: &str = "5555";
const MELLANOX_DPU_DEVICE_IDS_HEX: [&str; 5] = [
    "0XA2DF", // BF4 Family integrated network controller [BlueField-4 integrated network controller]
    "0XA2D9", // MT43162 BlueField-3 Lx integrated ConnectX-7 network controller
    "0XA2DC", // MT43244 BlueField-3 integrated ConnectX-7 network controller
    "0XA2D2", // MT416842 BlueField integrated ConnectX-5 network controller
    "0XA2D6", // MT42822 BlueField-2 integrated ConnectX-6 Dx network controller
];
const MELLANOX_DPU_DEVICE_IDS_DEC: [&str; 5] = ["41695", "41689", "41692", "41682", "41686"];

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
pub enum SystemPowerControl {
    /// Power on a machine
    On,
    /// Graceful host shutdown
    GracefulShutdown,
    /// Forcefully powers a machine off
    ForceOff,
    /// Graceful restart. Asks the OS to restart via ACPI
    /// - Might restart DPUs if no OS is running
    /// - Will not apply pending BIOS/UEFI setting changes
    GracefulRestart,
    /// Force restart. This is equivalent to pressing the reset button on the front panel.
    /// - Will not restart DPUs
    /// - Will apply pending BIOS/UEFI setting changes
    ForceRestart,
    /// Do an AC powercycle.  Not supported on all platforms.
    ACPowercycle,
    /// Implemented by Dells and DPUs. Not supported on Vikings.
    PowerCycle,
    // Dell also has: PushPowerButton and Nmi
    // Lenovo also has: ForceOn and Nmi
}

impl fmt::Display for SystemPowerControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    Off,
    #[default]
    On,
    PoweringOff,
    PoweringOn,
    Paused,
    Reset,
    Unknown,
}

impl fmt::Display for PowerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct StatusState {
    pub state: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Systems {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub description: Option<String>,
    pub members: Vec<ODataId>,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SystemStatus {
    pub health: Option<String>,
    pub health_rollup: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ComponentStatus {
    pub health: Option<String>,
    pub health_rollup: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SystemProcessors {
    #[serde(default)]
    pub count: Option<i64>,
    pub logical_processor_count: Option<i64>,
    pub model: Option<String>,
    pub status: Option<ComponentStatus>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct TrustedModule {
    pub firmware_version: Option<String>,
    pub interface_type: Option<String>,
    // we have seen the above two fields returned as null (on dells), but not status.
    pub status: StatusState,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub enum BootProgressTypes {
    None,
    PrimaryProcessorInitializationStarted,
    BusInitializationStarted,
    MemoryInitializationStarted,
    SecondaryProcessorInitializationStarted,
    PCIResourceConfigStarted,
    SystemHardwareInitializationComplete,
    SetupEntered,
    OSBootStarted,
    OSRunning,
    OEM,
}

impl fmt::Display for BootProgressTypes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BootProgress {
    pub last_state: Option<BootProgressTypes>,
    pub last_state_time: Option<String>,
    pub oem_last_state: Option<String>,
}

#[derive(Debug, Serialize, Default, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ComputerSystem {
    #[serde(flatten)]
    pub odata: OData,
    #[serde(rename = "@Redfish.Settings")]
    pub redfish_settings: Option<RedfishSettings>,
    pub asset_tag: Option<String>,
    #[serde(default)] // Some viking ComputerSystem has no Boot property; so use the default
    pub boot: Boot,
    pub bios: Option<ODataId>,
    pub bios_version: Option<String>,
    pub ethernet_interfaces: Option<ODataId>,
    pub id: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub oem: Option<SystemExtensions>,
    // Dell: String. Lenovo: null
    pub part_number: Option<String>,
    #[serde(default)]
    pub power_state: PowerState,
    pub processor_summary: Option<SystemProcessors>,
    #[serde(rename = "SKU")]
    pub sku: Option<String>,
    pub serial_number: Option<String>,
    pub status: Option<SystemStatus>,
    #[serde(default)]
    pub trusted_modules: Vec<TrustedModule>,
    #[serde(default, rename = "PCIeDevices")]
    pub pcie_devices: Vec<ODataId>, // not in Supermicro
    pub serial_console: Option<SerialConsole>, // Newer Redfish impls, inc Supermicro
    pub links: Option<ComputerSystemLinks>,
    pub boot_progress: Option<BootProgress>,
    #[serde(rename = "IPMIHostInterface")]
    pub ipmi_host_interface: Option<IpmiHostInterface>, // MGX C2 Supermicro (SSIF, not KCS); BMC firmware 01.05.01+
}

/// SSIF in-band host-to-BMC interface on MGX C2 Supermicro systems.
/// Present at `Systems/{id}` when the BMC firmware is 01.05.01+.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct IpmiHostInterface {
    pub service_enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ComputerSystemLinks {
    pub chassis: Option<Vec<ODataId>>,
    pub managed_by: Option<Vec<ODataId>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SerialConsole {
    pub max_concurrent_sessions: Option<usize>,
    #[serde(rename = "SSH")]
    pub ssh: SerialConsoleConnectionType,
    #[serde(rename = "IPMI")]
    pub ipmi: SerialConsoleConnectionType,
}

#[serde_with::skip_serializing_none]
#[derive(Default, Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SerialConsoleConnectionType {
    pub service_enabled: bool,
    pub port: Option<usize>,
    pub hot_key_sequence_display: Option<String>,
    #[serde(rename = "SharedWithManagerCLI")]
    pub shared_with_manager_cli: Option<bool>, // SSH only
    pub console_entry_command: Option<String>, // SSH only
}

#[serde_as]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BootOptions {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub description: Option<String>,
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub members: Vec<ODataId>,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct BootOption {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub alias: Option<String>,
    pub description: Option<String>,
    pub boot_option_enabled: Option<bool>,
    pub boot_option_reference: String,
    pub display_name: String,
    pub id: String,
    pub name: String,
    pub uefi_device_path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PCIeDevice {
    #[serde(flatten)]
    pub odata: OData,
    pub description: Option<String>,
    pub firmware_version: Option<String>,
    pub id: Option<String>,
    pub manufacturer: Option<String>,
    #[serde(rename = "GPUVendor")]
    pub gpu_vendor: Option<String>,
    pub name: Option<String>,
    pub part_number: Option<String>,
    pub serial_number: Option<String>,
    pub status: Option<SystemStatus>,
    pub slot: Option<Slot>,
    #[serde(default, rename = "PCIeFunctions")]
    pub pcie_functions: Option<ODataId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Slot {
    pub location: Option<Location>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Location {
    pub info: Option<String>,
    pub info_format: Option<String>,
    pub part_location: Option<PartLocation>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PartLocation {
    pub location_type: Option<String>,
    pub location_ordinal_value: Option<usize>,
    pub service_label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PCIeFunction {
    #[serde(flatten)]
    pub odata: OData,
    pub class_code: Option<String>,
    pub device_class: Option<String>,
    pub device_id: Option<String>,
    pub function_id: Option<i32>,
    pub function_type: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub status: Option<SystemStatus>,
    pub subsystem_id: Option<String>,
    pub subsystem_vendor_id: Option<String>,
    pub vendor_id: Option<String>,
}

impl PCIeFunction {
    // Is this a Mellanox Bluefield DPU?
    pub fn is_dpu(&self) -> bool {
        let is_mellanox = self.vendor_id.as_ref().is_some_and(|v_id| {
            v_id.to_uppercase() == MELLANOX_VENDOR_ID_HEX || v_id == MELLANOX_VENDOR_ID_DEC
        });
        let is_bluefield = self.device_id.as_ref().is_some_and(|candidate_id| {
            let u_candidate_id = candidate_id.to_uppercase();
            let as_hex = MELLANOX_DPU_DEVICE_IDS_HEX
                .iter()
                .any(|good_id| u_candidate_id == *good_id);
            let as_dec = MELLANOX_DPU_DEVICE_IDS_DEC
                .iter()
                .any(|good_id| u_candidate_id == *good_id);
            as_hex || as_dec
        });
        is_mellanox && is_bluefield
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PCIeDevices {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub description: Option<String>,
    pub members: Vec<ODataId>,
    pub name: String,
}

#[cfg(test)]
mod test {
    use crate::model::boot::{
        BootSourceOverrideEnabled, BootSourceOverrideMode, BootSourceOverrideTarget,
    };

    #[test]
    fn test_systems_parser() {
        let data = include_str!("testdata/systems.json");
        let result: super::Systems = serde_json::from_str(data).unwrap();
        assert_eq!(result.members.len(), 1);
        assert_eq!(result.odata.odata_id, "/redfish/v1/Systems");
    }

    #[test]
    fn test_system_dell() {
        let data = include_str!("testdata/system_dell.json");
        let result: super::ComputerSystem = serde_json::from_str(data).unwrap();
        assert_eq!(result.power_state, crate::PowerState::On);
        assert_eq!(result.processor_summary.unwrap().count, Some(2));
        assert_eq!(result.part_number.as_deref(), Some("0H28RRA06"));
    }

    #[test]
    fn test_system_dell_null_temp_rollup() {
        let data = include_str!("testdata/system_dell_null_temp_rollup.json");
        let result: super::ComputerSystem = serde_json::from_str(data).unwrap();
        let dell = result
            .oem
            .expect("should contain OEM data")
            .dell
            .expect("should contain dell-specific OEM data");
        assert!(dell.dell_system.temp_rollup_status.is_empty());
    }

    #[test]
    fn test_system_bluefield_boot_valid() {
        // Old firmware versions of Bluefield deliver empty values for Boot fields
        // that are not valid enumeration values
        let data = include_str!("testdata/system_bluefield_boot_valid.json");
        let result: super::ComputerSystem = serde_json::from_str(data).unwrap();

        assert_eq!(
            result.boot.boot_source_override_enabled,
            Some(BootSourceOverrideEnabled::Disabled)
        );
        assert_eq!(
            result.boot.boot_source_override_mode,
            Some(BootSourceOverrideMode::UEFI)
        );
        assert_eq!(
            result.boot.boot_source_override_target,
            Some(BootSourceOverrideTarget::None)
        );
    }

    #[test]
    fn test_system_bluefield_boot_bugs() {
        // Old firmware versions of Bluefield deliver empty values for Boot fields
        // that are not valid enumeration values
        let data = include_str!("testdata/system_bluefield_boot_bugs.json");
        let result: super::ComputerSystem = serde_json::from_str(data).unwrap();

        assert_eq!(
            result.boot.boot_source_override_enabled,
            Some(BootSourceOverrideEnabled::InvalidValue)
        );
        assert_eq!(
            result.boot.boot_source_override_mode,
            Some(BootSourceOverrideMode::InvalidValue)
        );
        assert_eq!(
            result.boot.boot_source_override_target,
            Some(BootSourceOverrideTarget::InvalidValue)
        );
    }

    #[test]
    fn test_system_lenovo() {
        let data = include_str!("testdata/system_lenovo.json");
        let result: super::ComputerSystem = serde_json::from_str(data).unwrap();
        assert_eq!(
            result.oem.unwrap().lenovo.unwrap().total_power_on_hours,
            Some(3816)
        );
        assert_eq!(result.processor_summary.unwrap().count, Some(2));
        assert_eq!(result.part_number, None);
    }

    #[test]
    fn test_boot_options() {
        let data = include_str!("testdata/boot_options.json");
        let result: super::BootOptions = serde_json::from_str(data).unwrap();
        assert_eq!(result.members.len(), 5);
    }

    #[test]
    fn test_boot_option() {
        let data = include_str!("testdata/boot_option.json");
        let result: super::BootOption = serde_json::from_str(data).unwrap();
        assert_eq!(result.name, "Network");
    }
}
