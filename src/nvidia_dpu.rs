use std::str::FromStr;
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
use std::{collections::HashMap, path::Path, time::Duration};

use reqwest::StatusCode;
use serde::Deserialize;
use tokio::fs::File;

use crate::model::oem::nvidia_dpu::NicMode;
use crate::model::service_root::RedfishVendor;
use crate::model::task::Task;
use crate::model::update_service::ComponentType;
use crate::Boot::UefiHttp;
use crate::HostPrivilegeLevel::Restricted;
use crate::InternalCPUModel::Embedded;
use crate::{
    model::{
        boot::{
            BootOverride, BootSourceOverrideEnabled, BootSourceOverrideMode,
            BootSourceOverrideTarget,
        },
        oem::nvidia_dpu::{HostPrivilegeLevel, InternalCPUModel},
        sel::{LogEntry, LogEntryCollection},
        BootOption,
    },
    standard::RedfishStandard,
    BiosProfileType, Chassis, NetworkDeviceFunction, Redfish, RedfishError, SystemPowerControl,
};
use crate::{EnabledDisabled, MachineSetupDiff, MachineSetupStatus};

pub struct Bmc {
    s: RedfishStandard,
}

pub enum BootOptionName {
    Http,
    Pxe,
    Disk,
}
impl BootOptionName {
    fn to_string(&self) -> &str {
        match self {
            BootOptionName::Http => "UEFI HTTPv4",
            BootOptionName::Pxe => "UEFI PXEv4",
            BootOptionName::Disk => "UEFI Non-Block Boot Device",
        }
    }
}

impl Bmc {
    pub fn new(s: RedfishStandard) -> Result<Bmc, RedfishError> {
        Ok(Bmc { s })
    }

    async fn try_force_dpu_reset(&self) -> Result<bool, RedfishError> {
        let chassis_ids = self.s.get_members("Chassis").await?;
        let Some(chassis_id) = matching_chassis_id(self.s.system_id(), &chassis_ids) else {
            return Ok(false);
        };

        let url = format!("Chassis/{chassis_id}");
        let (_, chassis): (_, Chassis) = self.s.client.get(&url).await?;
        let Some(target) = nvidia_force_dpu_reset_target(&chassis) else {
            return Ok(false);
        };
        let target = target
            .strip_prefix(&format!("/{}/", crate::REDFISH_ENDPOINT))
            .unwrap_or(target);
        let data = HashMap::from([("ResetType", "ForceDpuReset")]);
        match self.s.client.post(target, data).await {
            Ok(_) => Ok(true),
            Err(error) if error.not_found() => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn matching_chassis_id<'a>(system_id: &str, chassis_ids: &'a [String]) -> Option<&'a str> {
    chassis_ids.iter().find_map(|chassis_id| {
        (chassis_id.rsplit('/').next() == Some(system_id)).then_some(chassis_id.as_str())
    })
}

fn nvidia_force_dpu_reset_target(chassis: &Chassis) -> Option<&str> {
    chassis
        .actions
        .as_ref()?
        .oem
        .as_ref()?
        .reset
        .as_ref()?
        .target
        .as_deref()
}

impl Redfish for Bmc {
    fn std_redfish(&self) -> &RedfishStandard {
        &self.s
    }
    fn get_power_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Power, RedfishError>> {
        Box::pin(async move {
            let (_status_code, body) = self.s.client.get("Chassis/Card1/Power/").await?;
            Ok(body)
        })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        false
    }

    fn power<'a>(
        &'a self,
        action: SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if action == SystemPowerControl::ForceRestart && self.try_force_dpu_reset().await? {
                return Ok(());
            }

            self.s.power(action).await
        })
    }

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Thermal, RedfishError>> {
        Box::pin(async move {
            let (_status_code, body) = self.s.client.get("Chassis/Card1/Thermal/").await?;
            Ok(body)
        })
    }

    fn get_system_event_log<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move { self.get_system_event_log().await })
    }

    fn get_bmc_event_log<'a>(
        &'a self,
        from: Option<chrono::DateTime<chrono::Utc>>,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Systems/{}/LogServices/EventLog/Entries",
                self.s.system_id()
            );
            self.s.fetch_bmc_event_log(url, from).await
        })
    }

    fn machine_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
        _bios_profiles: &'a HashMap<
            RedfishVendor,
            HashMap<String, HashMap<BiosProfileType, HashMap<String, serde_json::Value>>>,
        >,
        _selected_profile: BiosProfileType,
        _oem_manager_profiles: &'a HashMap<
            RedfishVendor,
            HashMap<String, HashMap<BiosProfileType, HashMap<String, serde_json::Value>>>,
        >,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            self.set_host_privilege_level(Restricted).await?;
            // we have found that only newer BMC fws support this action.
            // Until we re-enable DPU BMC firmware updates in preingestion,
            // ignore an error from trying to disable host rshim against
            // BF3s that have a BMC that is too old.
            self.set_host_rshim(EnabledDisabled::Disabled).await?;
            self.set_internal_cpu_model(Embedded).await?;
            self.boot_once(UefiHttp).await?;
            Ok(None)
        })
    }

    fn machine_setup_status<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            let mut diffs = vec![];

            let bios = self.s.bios_attributes().await?;
            let key = "HostPrivilegeLevel";
            let key_with_spaces = "Host Privilege Level";
            let Some(hpl) = bios.get(key).or_else(|| bios.get(key_with_spaces)) else {
                return Err(RedfishError::MissingKey {
                    key: key.to_string(),
                    url: "Systems/{}/Bios".to_string(),
                });
            };

            let actual = HostPrivilegeLevel::deserialize(hpl).map_err(|e| {
                RedfishError::JsonDeserializeError {
                    url: "Systems/{}/Bios".to_string(),
                    body: hpl.to_string(),
                    source: e,
                }
            })?;
            let expected = HostPrivilegeLevel::Restricted;
            if actual != expected {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    actual: actual.to_string(),
                    expected: expected.to_string(),
                });
            }

            let key = "InternalCPUModel";
            let key_with_spaces = "Internal CPU Model";
            let Some(icm) = bios.get(key).or_else(|| bios.get(key_with_spaces)) else {
                return Err(RedfishError::MissingKey {
                    key: key.to_string(),
                    url: "Systems/{}/Bios".to_string(),
                });
            };

            let actual = InternalCPUModel::deserialize(icm).map_err(|e| {
                RedfishError::JsonDeserializeError {
                    url: "Systems/{}/Bios".to_string(),
                    body: hpl.to_string(),
                    source: e,
                }
            })?;
            let expected = InternalCPUModel::Embedded;
            if actual != expected {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    actual: actual.to_string(),
                    expected: expected.to_string(),
                });
            }

            Ok(MachineSetupStatus {
                is_done: diffs.is_empty(),
                diffs,
            })
        })
    }

    fn set_machine_password_policy<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            /*
            We used to try to PATCH AccountLockoutThreshold and AccountLockoutDuration
            But, I tried this against multiple DPUs, both BF2 and BF3. When I issued the same
            request, the DPU's BMC returns an error indicating that these properties are read only.
            */
            Ok(())
        })
    }

    fn boot_once<'a>(
        &'a self,
        target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let override_target = match target {
                crate::Boot::Pxe => BootSourceOverrideTarget::Pxe,
                crate::Boot::HardDisk => BootSourceOverrideTarget::Hdd,
                crate::Boot::UefiHttp => BootSourceOverrideTarget::UefiHttp,
            };
            Redfish::set_boot_override(
                self,
                BootOverride {
                    target: override_target,
                    enabled: BootSourceOverrideEnabled::Once,
                    mode: None,
                    http_boot_uri: None,
                },
            )
            .await?;
            Ok(())
        })
    }

    fn boot_first<'a>(
        &'a self,
        target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            match target {
                crate::Boot::Pxe => self.set_boot_order(&BootOptionName::Pxe).await,
                crate::Boot::HardDisk => self.set_boot_order(&BootOptionName::Disk).await,
                crate::Boot::UefiHttp => self.set_boot_order(&BootOptionName::Http).await,
            }
        })
    }

    fn set_boot_override<'a>(
        &'a self,
        settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mut boot_data: HashMap<String, serde_json::Value> = HashMap::new();
            boot_data.insert(
                "BootSourceOverrideTarget".to_string(),
                settings.target.to_string().into(),
            );
            boot_data.insert(
                "BootSourceOverrideEnabled".to_string(),
                settings.enabled.to_string().into(),
            );
            // BlueField DPU BMCs default to UEFI mode when the caller doesn't specify one.
            let mode = settings.mode.unwrap_or(BootSourceOverrideMode::UEFI);
            boot_data.insert(
                "BootSourceOverrideMode".to_string(),
                mode.to_string().into(),
            );
            if let Some(uri) = settings.http_boot_uri {
                boot_data.insert("HttpBootUri".to_string(), uri.into());
            }
            let url = format!("Systems/{}/Settings", self.s.system_id());
            self.s
                .client
                .patch(&url, HashMap::from([("Boot", boot_data)]))
                .await?;
            Ok(None)
        })
    }

    fn update_firmware_multipart<'a>(
        &'a self,
        filename: &'a Path,
        _reboot: bool,
        timeout: Duration,
        _component_type: ComponentType,
    ) -> crate::RedfishFuture<'a, Result<String, RedfishError>> {
        Box::pin(async move {
            let firmware = File::open(&filename)
                .await
                .map_err(|e| RedfishError::FileError(format!("Could not open file: {}", e)))?;

            let update_service = self.s.get_update_service().await?;

            if update_service.multipart_http_push_uri.is_empty() {
                return Err(RedfishError::NotSupported(
                    "Host BMC does not support HTTP multipart push".to_string(),
                ));
            }

            let parameters = "{}".to_string();

            let (_status_code, _loc, body) = self
                .s
                .client
                .req_update_firmware_multipart(
                    filename,
                    firmware,
                    parameters,
                    &update_service.multipart_http_push_uri,
                    true,
                    timeout,
                )
                .await
                .map_err(|e| match e {
                    RedfishError::HTTPErrorCode { status_code, .. }
                        if status_code == StatusCode::NOT_FOUND =>
                    {
                        RedfishError::NotSupported(
                            "Host BMC does not support HTTP multipart push".to_string(),
                        )
                    }
                    e => e,
                })?;

            let task: Task =
                serde_json::from_str(&body).map_err(|e| RedfishError::JsonDeserializeError {
                    url: update_service.multipart_http_push_uri,
                    body,
                    source: e,
                })?;

            Ok(task.id)
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/Settings", self.s.system_id());
            let mut attributes = HashMap::new();
            let mut data = HashMap::new();
            data.insert("ResetEfiVars", true);
            attributes.insert("Attributes", data);
            self.s
                .client
                .patch(&url, attributes)
                .await
                .map(|_resp| Ok(()))?
        })
    }

    fn get_ports<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            // http://redfish.dmtf.org/schemas/v1/NetworkPortCollection.json
            let url = format!(
                "Chassis/{}/NetworkAdapters/{}/Ports",
                chassis_id, network_adapter
            );
            self.s.get_members(&url).await
        })
    }

    fn get_port<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::NetworkPort, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Chassis/{}/NetworkAdapters/{}/Ports/{}",
                chassis_id, network_adapter, id
            );
            let (_status_code, body) = self.s.client.get(&url).await?;
            Ok(body)
        })
    }

    fn get_network_device_function<'a>(
        &'a self,
        chassis_id: &'a str,
        id: &'a str,
        _port: Option<&'a str>,
    ) -> crate::RedfishFuture<'a, Result<NetworkDeviceFunction, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Chassis/{}/NetworkAdapters/NvidiaNetworkAdapter/NetworkDeviceFunctions/{}",
                chassis_id, id
            );
            let (_status_code, body) = self.s.client.get(&url).await?;
            Ok(body)
        })
    }

    /// http://redfish.dmtf.org/schemas/v1/NetworkDeviceFunctionCollection.json
    fn get_network_device_functions<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Chassis/{}/NetworkAdapters/NvidiaNetworkAdapter/NetworkDeviceFunctions",
                chassis_id
            );
            self.s.get_members(&url).await
        })
    }

    fn change_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
        new_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mut attributes = HashMap::new();
            let mut data = HashMap::new();
            data.insert("CurrentUefiPassword", current_uefi_password.to_string());
            data.insert("UefiPassword", new_uefi_password.to_string());
            attributes.insert("Attributes", data);
            let url = format!("Systems/{}/Bios/Settings", self.s.system_id());
            let _status_code = self.s.client.patch(&url, attributes).await?;
            Ok(None)
        })
    }

    fn change_boot_order<'a>(
        &'a self,
        boot_array: Vec<String>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let body = HashMap::from([("Boot", HashMap::from([("BootOrder", boot_array)]))]);
            let url = format!("Systems/{}/Settings", self.s.system_id());
            self.s.client.patch(&url, body).await?;
            Ok(())
        })
    }

    fn bmc_reset_to_defaults<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Managers/{}/Actions/Manager.ResetToDefaults",
                self.s.manager_id()
            );
            let mut arg = HashMap::new();
            arg.insert("ResetToDefaultsType", "ResetAll".to_string());
            self.s.client.post(&url, arg).await.map(|_resp| Ok(()))?
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "set_dpu_first_boot_order".to_string(),
            ))
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn get_base_mac_address<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Oem/Nvidia", self.s.system_id());
            let (_sc, body): (reqwest::StatusCode, HashMap<String, serde_json::Value>) =
                self.s.client.get(url.as_str()).await?;
            Ok(body.get("BaseMAC").map(|v| v.to_string()))
        })
    }

    fn enable_rshim_bmc<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let data = HashMap::from([("BmcRShim", HashMap::from([("BmcRShimEnabled", true)]))]);

            self.s
                .client
                .patch("Managers/Bluefield_BMC/Oem/Nvidia", data)
                .await
                .map(|_status_code| Ok(()))?
        })
    }

    fn get_nic_mode<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<NicMode>, RedfishError>> {
        Box::pin(async move { self.get_nic_mode().await })
    }

    fn set_nic_mode<'a>(
        &'a self,
        mode: NicMode,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.set_nic_mode(mode).await })
    }

    fn set_host_rshim<'a>(
        &'a self,
        enabled: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if self.is_bf2().await? {
                return Ok(());
            }

            let mut data: HashMap<&str, String> = HashMap::new();
            data.insert("HostRshim", enabled.to_string());
            let url = format!(
                "Systems/{}/Oem/Nvidia/Actions/HostRshim.Set",
                self.s.system_id()
            );

            self.s.client.post(&url, data).await.map(|_resp| Ok(()))?
        })
    }

    fn get_host_rshim<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<EnabledDisabled>, RedfishError>> {
        Box::pin(async move {
            if self.is_bf2().await? {
                return Ok(None);
            }

            let url = format!("Systems/{}/Oem/Nvidia", self.s.system_id());
            let (_sc, body): (reqwest::StatusCode, HashMap<String, serde_json::Value>) =
                self.s.client.get(url.as_str()).await?;
            let val = body.get("HostRshim").map(|v| v.to_string());
            let is_host_rshim_enabled = match val {
                Some(is_host_rshim_enabled) => {
                    EnabledDisabled::from_str(is_host_rshim_enabled.trim_matches('"')).ok()
                }
                None => None,
            };
            Ok(is_host_rshim_enabled)
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let status = self.machine_setup_status(boot_interface).await?;
            Ok(status.is_done)
        })
    }

    fn set_host_privilege_level<'a>(
        &'a self,
        level: HostPrivilegeLevel,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // There is a change in the Attribute naming in DPU BMC 24.10, it no longer has spaces
            // Because of this we need to try both cases of the named key
            let key = "HostPrivilegeLevel";
            let data = HashMap::from([("Attributes", HashMap::from([(key, level.to_string())]))]);

            match self.patch_bios_setting(data).await {
                Ok(_) => return Ok(()),
                Err(RedfishError::HTTPErrorCode { response_body, .. })
                    if response_body.contains(key) =>
                {
                    Ok(())
                }
                Err(e) => Err(e),
            }?;

            let key = "Host Privilege Level";
            let data = HashMap::from([("Attributes", HashMap::from([(key, level.to_string())]))]);

            self.patch_bios_setting(data)
                .await
                .map(|_status_code| Ok(()))?
        })
    }
}

impl Bmc {
    async fn patch_bios_setting(
        &self,
        data: HashMap<&str, HashMap<&str, String>>,
    ) -> Result<(), RedfishError> {
        let url = format!("Systems/{}/Bios/Settings", self.s.system_id());
        self.s
            .client
            .patch(&url, data)
            .await
            .map(|_status_code| Ok(()))?
    }

    async fn is_bf2(&self) -> Result<bool, RedfishError> {
        let chassis = self.get_chassis("Card1").await?;
        Ok(chassis
            .model
            .is_none_or(|m| m.as_str().to_lowercase().as_str().contains("bluefield 2")))
    }

    async fn set_internal_cpu_model(&self, model: InternalCPUModel) -> Result<(), RedfishError> {
        // There is a change in the Attribute naming in DPU BMC 24.10, it no longer has spaces
        // Because of this we need to try both cases of the named key
        let key = "InternalCPUModel";
        let data = HashMap::from([("Attributes", HashMap::from([(key, model.to_string())]))]);

        match self.patch_bios_setting(data).await {
            Ok(_) => return Ok(()),
            Err(RedfishError::HTTPErrorCode { response_body, .. })
                if response_body.contains(key) =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }?;

        let key = "Internal CPU Model";
        let data = HashMap::from([("Attributes", HashMap::from([(key, model.to_string())]))]);

        self.patch_bios_setting(data)
            .await
            .map(|_status_code| Ok(()))?
    }

    // name: The name of the device you want to make the first boot choice.
    async fn set_boot_order(&self, name: &BootOptionName) -> Result<(), RedfishError> {
        let boot_array = match self.get_boot_options_ids_with_first(name).await? {
            None => {
                return Err(RedfishError::MissingBootOption(name.to_string().to_owned()));
            }
            Some(b) => b,
        };
        self.change_boot_order(boot_array).await
    }

    // A Vec of string boot option names, with the one you want first.
    //
    // Example: get_boot_options_ids_with_first(lenovo::BootOptionName::Network) might return
    // ["Boot0003", "Boot0002", "Boot0001", "Boot0004"] where Boot0003 is Network. It has been
    // moved to the front ready for sending as an update.
    // The order of the other boot options does not change.
    //
    // If the boot option you want is not found returns Ok(None)
    async fn get_boot_options_ids_with_first(
        &self,
        with_name: &BootOptionName,
    ) -> Result<Option<Vec<String>>, RedfishError> {
        let with_name_str = with_name.to_string();
        let mut ordered = Vec::new(); // the final boot options
        let boot_options = self.s.get_system().await?.boot.boot_order;
        for member in boot_options {
            let b: BootOption = self.s.get_boot_option(member.as_str()).await?;
            if b.display_name.starts_with(with_name_str) {
                ordered.insert(0, b.id);
            } else {
                ordered.push(b.id);
            }
        }
        Ok(Some(ordered))
    }

    // dpu stores the sel as part of the system? there's a LogServices for the bmc too, but no sel
    async fn get_system_event_log(&self) -> Result<Vec<LogEntry>, RedfishError> {
        let url = format!("Systems/{}/LogServices/SEL/Entries", self.s.system_id());
        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.s.client.get(&url).await?;
        let log_entries = log_entry_collection.members;
        Ok(log_entries)
    }

    // get bmc firmware version for the DPU
    async fn get_bmc_firmware_version(&self) -> Result<String, RedfishError> {
        let inventory_list = self.get_software_inventories().await?;
        if let Some(bmc_firmware) = inventory_list.iter().find(|i| i.contains("BMC_Firmware")) {
            if let Some(bmc_firmware_version) =
                self.get_firmware(bmc_firmware.as_str()).await?.version
            {
                Ok(bmc_firmware_version)
            } else {
                Err(RedfishError::MissingKey {
                    key: "BMC_Firmware".to_owned(),
                    url: format!("UpdateService/FirmwareInventory/{bmc_firmware}"),
                })
            }
        } else {
            Err(RedfishError::MissingKey {
                key: "BMC_Firmware".to_owned(),
                url: "UpdateService/FirmwareInventory".to_owned(),
            })
        }
    }

    fn parse_nic_mode_from_bios(
        &self,
        bios: HashMap<String, serde_json::Value>,
    ) -> Result<NicMode, RedfishError> {
        match bios.get("Attributes") {
            Some(bios_attributes) => {
                if let Some(nic_mode) = bios_attributes
                    .get("NicMode")
                    .and_then(|v| v.as_str().and_then(|v| NicMode::from_str(v).ok()))
                {
                    Ok(nic_mode)
                } else {
                    Err(RedfishError::MissingKey {
                        key: "NicMode".to_owned(),
                        url: format!("Systems/{}/Bios", self.s.system_id()),
                    })
                }
            }
            None => Err(RedfishError::MissingKey {
                key: "Attributes".to_owned(),
                url: format!("Systems/{}/Bios", self.s.system_id()),
            }),
        }
    }

    async fn get_nic_mode_from_bios(
        &self,
        current_bmc_firmware_version: &str,
    ) -> Result<NicMode, RedfishError> {
        let nic_mode = match self.s.bios().await {
            Ok(bios) => self.parse_nic_mode_from_bios(bios),
            Err(e) => {
                // If the BMC firmware version is less than 24.07, querying the bios attributes on a DPU in NIC mode will return an internal 500 error.
                let min_bmc_fw_version_to_query_nic_mode_without_error = "BF-24.07-14";

                if version_compare::compare(
                    current_bmc_firmware_version,
                    min_bmc_fw_version_to_query_nic_mode_without_error,
                )
                .is_ok_and(|c| c == version_compare::Cmp::Lt)
                    && self.check_bios_error_is_dpu_in_nic_mode(&e)
                {
                    return Ok(NicMode::Nic);
                }

                return Err(e);
            }
        }?;

        Ok(nic_mode)
    }

    fn check_bios_error_is_dpu_in_nic_mode(&self, e: &RedfishError) -> bool {
        match e {
            RedfishError::HTTPErrorCode {
                url: _,
                status_code,
                response_body,
            } if *status_code == StatusCode::INTERNAL_SERVER_ERROR => {
                let bios: HashMap<String, serde_json::Value> =
                    serde_json::from_str(response_body).unwrap_or_default();
                if let Ok(NicMode::Nic) = self.parse_nic_mode_from_bios(bios) {
                    return true;
                }
            }
            _ => {}
        }

        false
    }

    /*
    There is a known bug with querying a BF3's mode when it is in NIC mode on certain BMC firmwares: the OEM extension times out
    and querying the BIOS attributes returns an Internal Server Error with the NicMode value populated properly within the BIOS attributes.
    */
    async fn check_bios_is_bf3_in_nic_mode(&self) -> bool {
        if let Err(e) = self.s.bios().await {
            return self.check_bios_error_is_dpu_in_nic_mode(&e);
        }

        false
    }

    async fn get_nic_mode_bf3_oem_extension(&self) -> Result<Option<NicMode>, RedfishError> {
        let url = format!("Systems/{}/Oem/Nvidia", self.s.system_id());
        let (_sc, body): (reqwest::StatusCode, HashMap<String, serde_json::Value>) =
            self.s.client.get(url.as_str()).await?;
        let val = body.get("Mode").map(|v| v.to_string());
        let nic_mode = match val {
            Some(mode) => NicMode::from_str(&mode).ok(),
            None => None,
        };
        Ok(nic_mode)
    }

    async fn get_nic_mode_bf3(
        &self,
        current_bmc_firmware_version: &str,
    ) -> Result<Option<NicMode>, RedfishError> {
        if self.will_oem_extension_timeout_in_nic_mode(current_bmc_firmware_version)
            && self.check_bios_is_bf3_in_nic_mode().await
        {
            return Ok(Some(NicMode::Nic));
        }

        self.get_nic_mode_bf3_oem_extension().await
    }

    fn nic_mode_unsupported(
        &self,
        current_bmc_firmware_version: &str,
    ) -> Result<bool, RedfishError> {
        let min_bmc_fw_version_to_query_nic_mode = "BF-23.10-5";
        Ok(version_compare::compare(
            current_bmc_firmware_version,
            min_bmc_fw_version_to_query_nic_mode,
        )
        .is_ok_and(|c| c == version_compare::Cmp::Lt))
    }

    // BMC FW BF-24.04-5 times out when accessing "redfish/v1/Systems/Bluefield/Oem/Nvidia" on DPUs in NIC mode
    fn will_oem_extension_timeout_in_nic_mode(&self, current_bmc_firmware_version: &str) -> bool {
        // right now, we know that BF-24.04-5 on BF3 times out when accessing redfish/v1/Systems/Bluefield/Oem/Nvidia
        let bmc_versions_without_oem_extension_support = vec!["BF-24.04-5"];
        for version in bmc_versions_without_oem_extension_support {
            if version_compare::compare(current_bmc_firmware_version, version)
                .is_ok_and(|c| c == version_compare::Cmp::Eq)
            {
                return true;
            }
        }

        false
    }

    async fn get_nic_mode(&self) -> Result<Option<NicMode>, RedfishError> {
        let current_bmc_firmware_version = self.get_bmc_firmware_version().await?;
        if self.nic_mode_unsupported(&current_bmc_firmware_version)? {
            tracing::warn!(
                "cannot query nic mode on this DPU (bmc fw: {current_bmc_firmware_version})"
            );
            return Ok(None);
        }

        if self.is_bf2().await? {
            let nic_mode = self
                .get_nic_mode_from_bios(&current_bmc_firmware_version)
                .await?;
            return Ok(Some(nic_mode));
        }

        let nic_mode = match self.get_nic_mode_bf3(&current_bmc_firmware_version).await? {
            Some(mode) => mode,
            None => {
                tracing::warn!("could not retrieve a nic mode from the system oem extension on a BF3--trying to parse nic mode from the DPU's BIOS attributes");
                self.get_nic_mode_from_bios(&current_bmc_firmware_version)
                    .await?
            }
        };

        Ok(Some(nic_mode))
    }

    async fn set_nic_mode(&self, nic_mode: NicMode) -> Result<(), RedfishError> {
        let current_bmc_firmware_version = self.get_bmc_firmware_version().await?;
        if self.nic_mode_unsupported(&current_bmc_firmware_version)? {
            return Err(RedfishError::NotSupported(format!(
                "cannot set nic mode on this DPU (bmc fw: {current_bmc_firmware_version})"
            )));
        }

        let mut data = HashMap::new();
        let val = match nic_mode {
            NicMode::Dpu => "DpuMode",
            NicMode::Nic => "NicMode",
        };

        if self.is_bf2().await? {
            let mut attributes = HashMap::new();
            data.insert("NicMode", val);
            attributes.insert("Attributes", data);
            let url = format!("Systems/{}/Bios/Settings", self.s.system_id());
            return self
                .s
                .client
                .patch(&url, attributes)
                .await
                .map(|_resp| Ok(()))?;
        }

        data.insert("Mode", val);
        tracing::warn!("data: {data:#?}");
        let url = format!("Systems/{}/Oem/Nvidia/Actions/Mode.Set", self.s.system_id());

        self.s.client.post(&url, data).await.map(|_resp| Ok(()))?
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn matching_chassis_requires_the_system_id() {
        let cases = [
            (
                "matching BlueField-4 chassis",
                "BlueField_0",
                vec!["BMC_0".to_string(), "BlueField_0".to_string()],
                Some("BlueField_0"),
            ),
            (
                "BlueField-3 chassis uses a different id",
                "Bluefield",
                vec!["Bluefield_BMC".to_string(), "Card1".to_string()],
                None,
            ),
        ];

        for (name, system_id, chassis_ids, expected) in cases {
            assert_eq!(
                matching_chassis_id(system_id, &chassis_ids),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn nvidia_force_dpu_reset_requires_an_advertised_target() {
        let cases = [
            (
                "advertised action",
                json!({
                    "Actions": {
                        "Oem": {
                            "#NvidiaChassis.Reset": {
                                "target": "/redfish/v1/Chassis/BlueField_0/Actions/Oem/NvidiaChassis.Reset"
                            }
                        }
                    }
                }),
                Some("/redfish/v1/Chassis/BlueField_0/Actions/Oem/NvidiaChassis.Reset"),
            ),
            ("missing OEM action", json!({ "Actions": {} }), None),
        ];

        for (name, payload, expected) in cases {
            let chassis: Chassis = serde_json::from_value(payload).expect(name);
            assert_eq!(nvidia_force_dpu_reset_target(&chassis), expected, "{name}");
        }
    }
}
