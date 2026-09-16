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

use reqwest::{header::HeaderMap, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::fs::File;

use crate::{
    jsonmap,
    model::{
        boot::BootOverride,
        chassis::NetworkAdapter,
        network_device_function::NetworkDeviceFunction,
        oem::dell::{self, ShareParameters, StorageCollection, SystemConfiguration},
        resource::ResourceCollection,
        sel::{LogEntry, LogEntryCollection},
        service_root::RedfishVendor,
        update_service::ComponentType,
        BootOption, InvalidValueError, OnOff,
    },
    standard::RedfishStandard,
    BiosProfileType, Boot, EnabledDisabled, JobState, MachineSetupDiff, MachineSetupStatus,
    PowerState, Redfish, RedfishError, RoleId, Status, StatusInternal, SystemPowerControl,
};

const UEFI_PASSWORD_NAME: &str = "SetupPassword";

const MAX_ACCOUNT_ID: u8 = 16;

/// Match a Dell `NetworkDeviceFunction` against a [`BootInterfaceRef`].
///
/// - [`BootInterfaceRef::Mac`] matches when the NDF's `Ethernet.MACAddress`
///   equals the target (case-insensitive).
/// - [`BootInterfaceRef::InterfaceId`] matches when the NDF's `Id` is a
///   prefix of the target (with a `-` boundary) -- e.g. NDF `NIC.Slot.7-1`
///   matches partition `NIC.Slot.7-1-1`. Equality also matches. This lets us
///   locate the NDF for partitions whose MAC has been stripped (the
///   NicMode-Disabled case).
/// - [`BootInterfaceRef::Pair`] uses the same interface-ID match as
///   [`BootInterfaceRef::InterfaceId`]. Its MAC is not a fallback.
fn nw_dev_func_matches(
    nw_dev_func: &NetworkDeviceFunction,
    boot_interface: crate::BootInterfaceRef<'_>,
) -> bool {
    match boot_interface {
        crate::BootInterfaceRef::Mac(target) => nw_dev_func
            .ethernet
            .as_ref()
            .and_then(|e| e.mac_address.as_ref())
            .is_some_and(|m| m.eq_ignore_ascii_case(&target.to_string())),
        crate::BootInterfaceRef::InterfaceId(target)
        | crate::BootInterfaceRef::Pair {
            interface_id: target,
            ..
        } => nw_dev_func
            .id
            .as_deref()
            .is_some_and(|ndf_id| target == ndf_id || target.starts_with(&format!("{ndf_id}-"))),
    }
}

fn boot_option_name_matches(expected: &str, actual: &str) -> bool {
    actual == expected
        || actual
            .strip_prefix(expected)
            .is_some_and(|suffix| suffix.starts_with(" - "))
}

pub struct Bmc {
    s: RedfishStandard,
}
impl Redfish for Bmc {
    fn std_redfish(&self) -> &RedfishStandard {
        &self.s
    }
    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        role_id: RoleId,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // Find an unused ID
            // 'root' is typically ID 2 on an iDrac, and ID 1 might be special
            let mut account_id = 3;
            let mut is_free = false;
            while !is_free && account_id <= MAX_ACCOUNT_ID {
                let a = match self.s.get_account_by_id(&account_id.to_string()).await {
                    Ok(a) => a,
                    Err(_) => {
                        is_free = true;
                        break;
                    }
                };
                if let Some(false) = a.enabled {
                    is_free = true;
                    break;
                }
                account_id += 1;
            }
            if !is_free {
                return Err(RedfishError::TooManyUsers);
            }

            // Edit that unused account to be ours. That's how iDrac account creation works.
            self.s
                .edit_account(account_id, username, password, role_id, true)
                .await
        })
    }

    fn power<'a>(
        &'a self,
        action: SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if action == SystemPowerControl::ACPowercycle {
                let is_lockdown = self.is_lockdown().await?;
                let bios_attrs = self.s.bios_attributes().await?;
                let uefi_var_access = bios_attrs
                    .get("UefiVariableAccess")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if is_lockdown || uefi_var_access == "Controlled" {
                    return Err(RedfishError::GenericError {
                    error: "Cannot perform AC power cycle while system is locked down. Disable lockdown, reboot, verify BIOS attribute 'UefiVariableAccess' is 'Standard', and then try again.".to_string(),
                });
                }
                self.perform_ac_power_cycle().await
            } else {
                self.s.power(action).await
            }
        })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        true
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
            // Different Dell timestamp formats (UTC-5, DST, etc..) are making filtering and comparing very difficult
            self.s.get_bmc_event_log(from).await
        })
    }

    fn set_bios<'a>(
        &'a self,
        values: HashMap<String, serde_json::Value>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let apply_time = dell::SetSettingsApplyTime {
                apply_time: dell::RedfishSettingsApplyTime::OnReset, // requires reboot to apply
            };

            let set_attrs = dell::GenericSetBiosAttrs {
                redfish_settings_apply_time: apply_time,
                attributes: values,
            };

            let url = format!("Systems/{}/Bios/Settings/", self.s.system_id());
            self.s
                .client
                .patch(&url, set_attrs)
                .await
                .map(|_status_code| ())
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.factory_reset_bios().await })
    }

    fn machine_setup<'a>(
        &'a self,
        boot_interface: Option<crate::BootInterfaceRef<'a>>,
        bios_profiles: &'a HashMap<
            RedfishVendor,
            HashMap<String, HashMap<BiosProfileType, HashMap<String, serde_json::Value>>>,
        >,
        selected_profile: BiosProfileType,
        oem_manager_profiles: &'a HashMap<
            RedfishVendor,
            HashMap<String, HashMap<BiosProfileType, HashMap<String, serde_json::Value>>>,
        >,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            self.delete_job_queue().await?;

            let apply_time = dell::SetSettingsApplyTime {
                apply_time: dell::RedfishSettingsApplyTime::OnReset, // requires reboot to apply
            };

            // A boot interface means a managed DPU NIC to pin as the boot device;
            // `nic_slot_for` uses the interface id directly and only falls back to
            // a by-MAC NetworkDeviceFunction lookup when a MAC is all we have.
            let has_dpu = boot_interface.is_some();
            let nic_slot = self.nic_slot_for(boot_interface).await?;

            // dell idrac requires applying all bios settings at once.
            let machine_settings = self.machine_setup_attrs(&nic_slot).await?;
            let set_machine_attrs = dell::SetBiosAttrs {
                redfish_settings_apply_time: apply_time,
                attributes: machine_settings,
            };
            // Convert to a more generic HashMap to allow merging with the extra BIOS values
            let as_json = serde_json::to_string(&set_machine_attrs).map_err(|e| {
                RedfishError::GenericError {
                    error: { e.to_string() },
                }
            })?;
            let mut set_machine_attrs: HashMap<String, serde_json::Value> =
                serde_json::from_str(as_json.as_str()).map_err(|e| RedfishError::GenericError {
                    error: { e.to_string() },
                })?;
            if let Some(dell) = bios_profiles.get(&RedfishVendor::Dell) {
                let model = crate::model_coerce(
                    self.get_system()
                        .await?
                        .model
                        .unwrap_or("".to_string())
                        .as_str(),
                );
                if let Some(all_extra_values) = dell.get(&model) {
                    if let Some(extra_values) = all_extra_values.get(&selected_profile) {
                        tracing::debug!("Setting extra BIOS values: {extra_values:?}");
                        set_machine_attrs.extend(extra_values.clone());
                    }
                }
            }

            let url = format!("Systems/{}/Bios/Settings/", self.s.system_id());
            let bios_job_id = self
                .patch_settings_for_job_id(&url, set_machine_attrs)
                .await?;

            let oem_attrs = if let Some(dell) = oem_manager_profiles.get(&RedfishVendor::Dell) {
                let model = crate::model_coerce(
                    self.get_system().await?.model.unwrap_or_default().as_str(),
                );
                dell.get(&model)
                    .and_then(|all| all.get(&selected_profile))
                    .cloned()
                    .unwrap_or_default()
            } else {
                HashMap::new()
            };
            self.machine_setup_oem(&oem_attrs).await?;
            self.setup_bmc_remote_access().await?;

            if has_dpu {
                Ok(bios_job_id)
            } else {
                // Tell the caller and let them decide
                Err(RedfishError::NoDpu)
            }
        })
    }

    fn machine_setup_status<'a>(
        &'a self,
        boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            // Check BIOS and BMC attributes. Pass the boot interface through so an
            // interface id resolves the NIC slot directly (no by-MAC
            // NetworkDeviceFunction lookup, which fails when the NDF MAC is empty).
            let mut diffs = self.diff_bios_bmc_attr(boot_interface).await?;

            // Check lockdown
            let lockdown = self.lockdown_status().await?;
            if !lockdown.is_fully_enabled() {
                diffs.push(MachineSetupDiff {
                    key: "lockdown".to_string(),
                    expected: "Enabled".to_string(),
                    actual: lockdown.status.to_string(),
                });
            }

            // Check the first boot option
            if let Some(boot_interface) = boot_interface {
                let (expected, actual) = self
                    .get_expected_and_actual_first_boot_option(boot_interface)
                    .await?;
                if !matches!(
                    (&expected, &actual),
                    (Some(expected), Some(actual))
                        if boot_option_name_matches(expected, actual)
                ) {
                    diffs.push(MachineSetupDiff {
                        key: "boot_first".to_string(),
                        expected: expected.unwrap_or_else(|| "Not found".to_string()),
                        actual: actual.unwrap_or_else(|| "Not found".to_string()),
                    });
                }
            }

            Ok(MachineSetupStatus {
                is_done: diffs.is_empty(),
                diffs,
            })
        })
    }

    /// iDRAC does not suport changing password policy. They support IP blocking instead.
    /// https://github.com/dell/iDRAC-Redfish-Scripting/issues/295
    fn set_machine_password_policy<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // These are all password policy a Dell has, and they are all read only.
            // Redfish will reject attempts to modify them.
            // - AccountLockoutThreshold
            // - AccountLockoutDuration
            // - AccountLockoutCounterResetAfter
            // - AuthFailureLoggingThreshold
            Ok(())
        })
    }

    fn lockdown<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            use EnabledDisabled::*;
            // XE9680's can't PXE boot for some reason
            let system = self.s.get_system().await?;
            let entry = match system.model.as_deref() {
                Some("PowerEdge XE9680") => dell::BootDevices::UefiHttp,
                _ => dell::BootDevices::PXE,
            };
            match target {
                Enabled => {
                    //self.enable_bios_lockdown().await?;
                    self.enable_bmc_lockdown(entry).await
                }
                Disabled => {
                    self.disable_bmc_lockdown(entry).await?;
                    // BIOS lockdown blocks impi, ensure it's disabled even though we never set it
                    self.disable_bios_lockdown().await
                }
            }
        })
    }

    fn lockdown_status<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            let mut message = String::new();
            let enabled = EnabledDisabled::Enabled.to_string();
            let disabled = EnabledDisabled::Disabled.to_string();

            // BMC lockdown
            let (attrs, url) = self.manager_attributes().await?;
            let system_lockdown = jsonmap::get_str(&attrs, "Lockdown.1.SystemLockdown", &url)?;
            let racadm = jsonmap::get_str(&attrs, "Racadm.1.Enable", &url)?;

            message.push_str(&format!(
                "BMC: system_lockdown={system_lockdown}, racadm={racadm}."
            ));

            let is_bmc_locked = system_lockdown == enabled && racadm == disabled;
            let is_bmc_unlocked = system_lockdown == disabled && racadm == enabled;

            Ok(Status {
                message,
                status: if is_bmc_locked {
                    StatusInternal::Enabled
                } else if is_bmc_unlocked {
                    StatusInternal::Disabled
                } else {
                    StatusInternal::Partial
                },
            })
        })
    }

    fn setup_serial_console<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.delete_job_queue().await?;

            self.setup_bmc_remote_access().await?;

            // Detect BIOS format from current values and use appropriate targets
            let curr_bios_attributes = self.s.bios_attributes().await?;

            // Detect newer iDRAC by checking SerialPortAddress format.
            // Newer Dell BIOS uses Serial1Com*Serial2Com* format and OnConRedirAuto for SerialComm.
            let is_newer_idrac = curr_bios_attributes
                .get("SerialPortAddress")
                .and_then(|v| v.as_str())
                .map(|v| v.starts_with("Serial1"))
                .unwrap_or(false);

            let (serial_port_address, serial_comm) = if is_newer_idrac {
                (
                    dell::SerialPortSettings::Serial1Com2Serial2Com1,
                    dell::SerialCommSettings::OnConRedirAuto,
                )
            } else {
                (
                    dell::SerialPortSettings::Com1,
                    dell::SerialCommSettings::OnConRedir,
                )
            };

            // RedirAfterBoot: Not available in iDRAC 10
            let redir_after_boot = curr_bios_attributes
                .get("RedirAfterBoot")
                .is_some()
                .then_some(EnabledDisabled::Enabled);

            let apply_time = dell::SetSettingsApplyTime {
                apply_time: dell::RedfishSettingsApplyTime::OnReset, // requires reboot to apply
            };
            let serial_console = dell::BiosSerialAttrs {
                serial_comm,
                serial_port_address,
                ext_serial_connector: dell::SerialPortExtSettings::Serial1,
                fail_safe_baud: "115200".to_string(),
                con_term_type: dell::SerialPortTermSettings::Vt100Vt220,
                redir_after_boot,
            };
            let set_serial_attrs = dell::SetBiosSerialAttrs {
                redfish_settings_apply_time: apply_time,
                attributes: serial_console,
            };

            let url = format!("Systems/{}/Bios/Settings/", self.s.system_id());
            self.s
                .client
                .patch(&url, set_serial_attrs)
                .await
                .map(|_status_code| ())
        })
    }

    fn serial_console_status<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            let Status {
                status: remote_access_status,
                message: remote_access_message,
            } = self.bmc_remote_access_status().await?;
            let Status {
                status: bios_serial_status,
                message: bios_serial_message,
            } = self.bios_serial_console_status().await?;

            let final_status = {
                use StatusInternal::*;
                match (remote_access_status, bios_serial_status) {
                    (Enabled, Enabled) => Enabled,
                    (Disabled, Disabled) => Disabled,
                    _ => Partial,
                }
            };
            Ok(Status {
                status: final_status,
                message: format!("BMC: {remote_access_message}. BIOS: {bios_serial_message}."),
            })
        })
    }

    fn boot_once<'a>(&'a self, target: Boot) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            match target {
                Boot::Pxe => self.set_boot_first(dell::BootDevices::PXE, true).await,
                Boot::HardDisk => self.set_boot_first(dell::BootDevices::HDD, true).await,
                Boot::UefiHttp => Err(RedfishError::NotSupported(
                    "No Dell UefiHttp implementation".to_string(),
                )),
            }
        })
    }

    fn boot_first<'a>(
        &'a self,
        target: Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            match target {
                Boot::Pxe => self.set_boot_first(dell::BootDevices::PXE, false).await,
                Boot::HardDisk => self.set_boot_first(dell::BootDevices::HDD, false).await,
                Boot::UefiHttp => Err(RedfishError::NotSupported(
                    "No Dell UefiHttp implementation".to_string(),
                )),
            }
        })
    }

    /// Dell iDRAC does not expose the standard Redfish `Boot.HttpBootUri`
    /// property, and rejects PATCHes to `/Systems/{id}/Settings` that include
    /// `BootSourceOverrideTarget` or `BootSourceOverrideEnabled` (only
    /// `Boot.BootOrder` and `Boot.BootSourceOverrideMode` are accepted via
    /// that endpoint). The Dell-specific path for pinning a UEFI HTTP boot URL
    /// is via the `HttpDev1Uri` BIOS attribute (plus its `HttpDev1EnDis`,
    /// `HttpDev1DhcpEnDis`, `HttpDev1Protocol` siblings) PATCH'd to
    /// `/Systems/{id}/Bios/Settings` with `@Redfish.SettingsApplyTime: OnReset`.
    ///
    /// That creates a BIOS config job that applies on next reboot; the job ID
    /// is returned so callers can `DELETE` it to cancel before reboot.
    ///
    /// This has been tested (and verified) on:
    /// - iDRAC9: R760 (BIOS 2.5.4), R760xd2 (BIOS 1.7.5), XE9680.
    /// - iDRAC10: R670 (BIOS 1.7.5).
    ///
    /// HOWEVER, there seems to be some behavior in OTHER machines that I can't
    /// quite narrow down, where `HttpDev1Uri.ReadOnly: true` in the BIOS Attribute
    /// Registry, despite `HttpDev1EnDis: Enabled`. Systems were not in lockdown,
    /// at least it didn't look like it, so I'm not sure what put those systems
    /// into that state, and I couldn't actually figure out how to get them
    /// unlocked (this was as part of running across an entire development fleet).
    ///
    /// On these locked hosts, it returns HTTP 400 with a Dell-specific
    /// MessageId of the form `IDRAC.<ver>.SYS410` ("Unable to modify the
    /// attribute because the attribute is read-only and depends on other
    /// attributes"). We translate that specific error into `NotSupported` so
    /// the caller can fall back to DHCP option 67 for the URL. Any other 400
    /// or error propagates unchanged.
    ///
    /// Once we figure out the weird locked state, callers can opt machines
    /// into the BMC-pinning path more aggressively.
    fn set_boot_override<'a>(
        &'a self,
        settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let Some(uri) = settings.http_boot_uri else {
                // Dell does not accept BootSourceOverrideTarget/Enabled PATCHes
                // via /Systems/{id}/Settings. Without an http_boot_uri to set
                // via the BIOS attribute path, there's no Dell-specific
                // operation for this method to perform.
                return Err(RedfishError::NotSupported(
                    "Dell set_boot_override requires http_boot_uri; BootSourceOverrideTarget/Enabled are not settable via Redfish on iDRAC".to_string(),
                ));
            };

            let url = format!("Systems/{}/Bios/Settings", self.s.system_id());
            let body = serde_json::json!({
                "@Redfish.SettingsApplyTime": {"ApplyTime": "OnReset"},
                "Attributes": {
                    "HttpDev1Uri": uri,
                    "HttpDev1EnDis": "Enabled",
                    "HttpDev1DhcpEnDis": "Disabled",
                    "HttpDev1Protocol": "IPv4",
                }
            });

            match self.s.client.patch(&url, body).await {
                Ok((_, Some(headers))) => {
                    let job_id = self
                        .parse_job_id_from_response_headers(&url, headers)
                        .await?;
                    Ok(Some(job_id))
                }
                Ok((_, None)) => Err(RedfishError::NoHeader),
                Err(RedfishError::HTTPErrorCode {
                    status_code,
                    response_body,
                    ..
                }) if status_code == StatusCode::BAD_REQUEST
                    && response_body.contains("SYS410") =>
                {
                    Err(RedfishError::NotSupported(format!(
                        "Dell iDRAC rejected HttpDev1Uri PATCH as ReadOnly (MessageId SYS410). Response: {response_body}"
                    )))
                }
                Err(e) => Err(e),
            }
        })
    }

    fn clear_tpm<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.delete_job_queue().await?;

            let apply_time = dell::SetSettingsApplyTime {
                apply_time: dell::RedfishSettingsApplyTime::OnReset,
            };
            let tpm = dell::BiosTpmAttrs {
                tpm_security: OnOff::On,
                tpm2_hierarchy: dell::Tpm2HierarchySettings::Clear,
            };
            let set_tpm_clear = dell::SetBiosTpmAttrs {
                redfish_settings_apply_time: apply_time,
                attributes: tpm,
            };
            let url = format!("Systems/{}/Bios/Settings/", self.s.system_id());
            self.s
                .client
                .patch(&url, set_tpm_clear)
                .await
                .map(|_status_code| ())
        })
    }

    fn clear_pending<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.delete_job_queue().await })
    }

    /// update_firmware_multipart returns a string with the task ID
    fn update_firmware_multipart<'a>(
        &'a self,
        filename: &'a Path,
        reboot: bool,
        timeout: Duration,
        _component_type: ComponentType,
    ) -> crate::RedfishFuture<'a, Result<String, RedfishError>> {
        Box::pin(async move {
            let firmware = File::open(&filename)
                .await
                .map_err(|e| RedfishError::FileError(format!("Could not open file: {e}")))?;

            let parameters =
                serde_json::to_string(&UpdateParameters::new(reboot)).map_err(|e| {
                    RedfishError::JsonSerializeError {
                        url: "".to_string(),
                        object_debug: "".to_string(),
                        source: e,
                    }
                })?;

            let (_status_code, loc, _body) = self
                .s
                .client
                .req_update_firmware_multipart(
                    filename,
                    firmware,
                    parameters,
                    "UpdateService/MultipartUpload",
                    false,
                    timeout,
                )
                .await?;

            let loc = match loc {
                None => "Unknown".to_string(),
                Some(x) => x,
            };

            // iDRAC returns the full endpoint, we just want the task ID
            Ok(loc.replace("/redfish/v1/TaskService/Tasks/", ""))
        })
    }

    fn get_network_device_function<'a>(
        &'a self,
        chassis_id: &'a str,
        id: &'a str,
        port: Option<&'a str>,
    ) -> crate::RedfishFuture<'a, Result<NetworkDeviceFunction, RedfishError>> {
        Box::pin(async move {
            let Some(port) = port else {
                return Err(RedfishError::GenericError {
                    error: "Port is missing for Dell.".to_string(),
                });
            };
            let url = format!(
                "Chassis/{}/NetworkAdapters/{}/NetworkDeviceFunctions/{}",
                chassis_id, id, port
            );
            let (_status_code, body) = self.s.client.get(&url).await?;
            Ok(body)
        })
    }

    fn change_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
        new_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            // The uefi password cant be changed if the host is in lockdown
            if self.is_lockdown().await? {
                return Err(RedfishError::Lockdown);
            }

            // clear any pending configs/jobs before changing the UEFI password
            self.delete_job_queue().await?;

            self.s
                .change_bios_password(UEFI_PASSWORD_NAME, current_uefi_password, new_uefi_password)
                .await?;

            Ok(Some(self.create_bios_config_job().await?))
        })
    }

    fn get_job_state<'a>(
        &'a self,
        job_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<JobState, RedfishError>> {
        Box::pin(async move {
            let url = format!("Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/{}", job_id);
            let (_status_code, body): (_, HashMap<String, serde_json::Value>) =
                self.s.client.get(&url).await?;
            let job_state_value = jsonmap::get_str(&body, "JobState", &url)?;

            let job_state = match JobState::from_str(job_state_value) {
                JobState::Unknown => {
                    tracing::warn!(
                        bmc_ip = %self.s.client.host(),
                        job_id = %job_id,
                        raw_job_state = %job_state_value,
                        "Unrecognized Redfish JobState; mapping to JobState::Unknown"
                    );
                    JobState::Unknown
                }
                JobState::Scheduled => {
                    let message_value = jsonmap::get_str(&body, "Message", &url)?;
                    match message_value {
                        /* Example JSON response body for a job that is Scheduled but will never complete: the job remains stuck in a Scheduled state indefinitely.
                        {
                            "@odata.context": "/redfish/v1/$metadata#DellJob.DellJob",
                            "@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/JID_510613515077",
                            "@odata.type": "#DellJob.v1_5_0.DellJob",
                            "ActualRunningStartTime": null,
                            "ActualRunningStopTime": null,
                            "CompletionTime": null,
                            "Description": "Job Instance",
                            "EndTime": "TIME_NA",
                            "Id": "JID_510613515077",
                            "JobState": "Scheduled",
                            "JobType": "RAIDConfiguration",
                            "Message": "Job processing initialization failure.",
                            "MessageArgs": [],
                            "MessageArgs@odata.count": 0,
                            "MessageId": "PR30",
                            "Name": "Configure: BOSS.SL.16-1",
                            "PercentComplete": 1,
                            "StartTime": "2025-06-27T16:55:51",
                            "TargetSettingsURI": null
                        }
                        */
                        "Job processing initialization failure." => JobState::ScheduledWithErrors,
                        _ => JobState::Scheduled,
                    }
                }
                state => state,
            };

            Ok(job_state)
        })
    }

    // set_boot_order_dpu_first configures the boot order on the Dell to set the HTTP boot
    // option that corresponds to the primary DPU as the first boot option in the list.
    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let expected_boot_option_name: String = self
                .get_expected_dpu_boot_option_name(boot_interface)
                .await?;
            let boot_order = self.get_boot_order().await?;
            for (idx, boot_option) in boot_order.iter().enumerate() {
                if boot_option_name_matches(&expected_boot_option_name, &boot_option.display_name) {
                    if idx == 0 {
                        // Dells will not generate a bios config job below if the boot orders already configured correctly
                        tracing::info!(
                        "NO-OP: DPU ({boot_interface:?}) will already be the first netboot option ({expected_boot_option_name}) after reboot"
                    );
                        return Ok(None);
                    }

                    // Clear any committed-but-unapplied pending config first, as
                    // the sibling BIOS-config writers do (machine_setup,
                    // setup_serial_console, clear_tpm, change_uefi_password): on
                    // Dell a staged pending config makes this PATCH fail with
                    // SYS011 ("pending configuration values are already
                    // committed").
                    self.delete_job_queue().await?;

                    let url = format!("Systems/{}/Settings", self.s.system_id());
                    let body = HashMap::from([(
                        "Boot",
                        HashMap::from([("BootOrder", vec![boot_option.id.clone()])]),
                    )]);

                    return self.patch_settings_for_job_id(&url, body).await;
                }
            }

            Err(RedfishError::MissingBootOption(expected_boot_option_name))
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            match self.change_uefi_password(current_uefi_password, "").await {
                Ok(job_id) => return Ok(job_id),
                Err(e) => {
                    tracing::info!(
                    "Standard clear_uefi_password failed, trying ImportSystemConfiguration fallback: {e}"
                );
                }
            }

            // Fallback to ImportSystemConfiguration hack for older iDRAC
            // See: https://github.com/dell/iDRAC-Redfish-Scripting/issues/308
            let job_id = self
                .clear_uefi_password_via_import(current_uefi_password)
                .await?;
            Ok(Some(job_id))
        })
    }

    fn lockdown_bmc<'a>(
        &'a self,
        target: crate::EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            use EnabledDisabled::*;

            // XE9680's can't PXE boot for some reason
            let system = self.s.get_system().await?;
            let entry = match system.model.as_deref() {
                Some("PowerEdge XE9680") => dell::BootDevices::UefiHttp,
                _ => dell::BootDevices::PXE,
            };

            match target {
                Enabled => self.enable_bmc_lockdown(entry).await,
                Disabled => self.disable_bmc_lockdown(entry).await,
            }
        })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let attrs: HashMap<String, serde_json::Value> =
                HashMap::from([("BootSeqRetry".to_string(), "Enabled".into())]);
            self.set_bios(attrs).await
        })
    }

    fn is_infinite_boot_enabled<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios", self.s.system_id());
            let bios = self.bios().await?;
            let bios_attributes = jsonmap::get_object(&bios, "Attributes", &url)?;
            let infinite_boot_status =
                jsonmap::get_str(bios_attributes, "BootSeqRetry", "Bios Attributes")?;

            Ok(Some(
                infinite_boot_status == EnabledDisabled::Enabled.to_string(),
            ))
        })
    }

    fn set_idrac_lockdown<'a>(
        &'a self,
        enabled: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.set_idrac_lockdown(enabled).await })
    }

    fn get_boss_controller<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.get_boss_controller().await })
    }

    fn decommission_storage_controller<'a>(
        &'a self,
        controller_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(Some(self.decommission_controller(controller_id).await?)) })
    }

    fn create_storage_volume<'a>(
        &'a self,
        controller_id: &'a str,
        volume_name: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let drives = self.get_storage_drives(controller_id).await?;

            let raid_type = match drives.as_array().map(|a| a.len()).unwrap_or(0) {
                1 => "RAID0",
                2 => "RAID1",
                n => {
                    return Err(RedfishError::GenericError {
                        error: format!(
                            "Expected 1 or 2 drives for BOSS controller {controller_id}, found {n}"
                        ),
                    });
                }
            };

            Ok(Some(
                self.create_storage_volume(controller_id, volume_name, raid_type, drives)
                    .await?,
            ))
        })
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let (expected, actual) = self
                .get_expected_and_actual_first_boot_option(boot_interface)
                .await?;
            Ok(matches!(
                (&expected, &actual),
                (Some(expected), Some(actual)) if boot_option_name_matches(expected, actual)
            ))
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            // Pass the boot interface straight through: `diff_bios_bmc_attr`
            // resolves the NIC slot from an interface id directly, so a boot NIC
            // whose NetworkDeviceFunction MAC is empty still verifies.
            let diffs = self.diff_bios_bmc_attr(boot_interface).await?;
            Ok(diffs.is_empty())
        })
    }

    fn set_utc_timezone<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let manager_id = self.s.manager_id();
            let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");

            let mut timezone_attrs = HashMap::new();
            timezone_attrs.insert("Time.1.Timezone", "UTC");

            let body = HashMap::from([("Attributes", timezone_attrs)]);

            self.s.client.patch(&url, body).await?;
            Ok(())
        })
    }

    fn set_ntp_servers<'a>(
        &'a self,
        servers: &'a [String],
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if servers.is_empty() {
                return Ok(());
            }

            let mut attrs = HashMap::from([("NTPConfigGroup.1.NTPEnable", "Enabled")]);
            const NTP_KEYS: [&str; 3] = [
                "NTPConfigGroup.1.NTP1",
                "NTPConfigGroup.1.NTP2",
                "NTPConfigGroup.1.NTP3",
            ];
            for (i, key) in NTP_KEYS.into_iter().enumerate() {
                // blank unused slots so the set is authoritative
                attrs.insert(key, servers.get(i).map_or("", String::as_str));
            }

            // Try standard path first
            let body = HashMap::from([("Attributes", attrs)]);
            let manager_id = self.s.manager_id();
            let standard_url = format!("Managers/{manager_id}/Attributes");
            match self.s.client.patch(&standard_url, &body).await {
                Ok(_) => return Ok(()),
                Err(RedfishError::HTTPErrorCode {
                    status_code: StatusCode::NOT_FOUND,
                    ..
                }) => {
                    tracing::info!(
                        "Managers/Attributes not found, using OEM DellAttributes path for NTP server config"
                    );
                }
                Err(e) => return Err(e),
            }

            // Fallback to OEM DellAttributes path
            let oem_url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");
            self.s.client.patch(&oem_url, body).await?;
            Ok(())
        })
    }
}

impl Bmc {
    pub fn new(s: RedfishStandard) -> Result<Bmc, RedfishError> {
        Ok(Bmc { s })
    }

    /// Check BIOS and BMC attributes and return differences
    async fn diff_bios_bmc_attr(
        &self,
        boot_interface: Option<crate::BootInterfaceRef<'_>>,
    ) -> Result<Vec<MachineSetupDiff>, RedfishError> {
        let mut diffs = vec![];

        let bios = self.s.bios_attributes().await?;
        let nic_slot = self.nic_slot_for(boot_interface).await?;

        let mut expected_attrs = self.machine_setup_attrs(&nic_slot).await?;

        expected_attrs.tpm2_hierarchy = dell::Tpm2HierarchySettings::Enabled;

        macro_rules! diff {
            ($key:literal, $exp:expr, $act:ty) => {
                let key = $key;
                let exp = $exp;
                let Some(act_v) = bios.get(key) else {
                    return Err(RedfishError::MissingKey {
                        key: key.to_string(),
                        url: "bios".to_string(),
                    });
                };
                let act =
                    <$act>::deserialize(act_v).map_err(|e| RedfishError::JsonDeserializeError {
                        url: "bios".to_string(),
                        body: act_v.to_string(),
                        source: e,
                    })?;
                if exp != act {
                    diffs.push(MachineSetupDiff {
                        key: key.to_string(),
                        expected: exp.to_string(),
                        actual: act.to_string(),
                    });
                }
            };
        }

        diff!(
            "InBandManageabilityInterface",
            expected_attrs.in_band_manageability_interface,
            EnabledDisabled
        );
        diff!(
            "UefiVariableAccess",
            expected_attrs.uefi_variable_access,
            dell::UefiVariableAccessSettings
        );
        diff!(
            "SerialComm",
            expected_attrs.serial_comm,
            dell::SerialCommSettings
        );
        diff!(
            "SerialPortAddress",
            expected_attrs.serial_port_address,
            dell::SerialPortSettings
        );
        diff!("FailSafeBaud", expected_attrs.fail_safe_baud, String);
        diff!(
            "ConTermType",
            expected_attrs.con_term_type,
            dell::SerialPortTermSettings
        );
        // Only available in iDRAC 9
        if let (Some(exp), Some(_)) = (expected_attrs.redir_after_boot, bios.get("RedirAfterBoot"))
        {
            diff!("RedirAfterBoot", exp, EnabledDisabled);
        }
        diff!(
            "SriovGlobalEnable",
            expected_attrs.sriov_global_enable,
            EnabledDisabled
        );
        diff!("TpmSecurity", expected_attrs.tpm_security, OnOff);
        diff!(
            "Tpm2Hierarchy",
            expected_attrs.tpm2_hierarchy,
            dell::Tpm2HierarchySettings
        );
        diff!(
            "Tpm2Algorithm",
            expected_attrs.tpm2_algorithm,
            dell::Tpm2Algorithm
        );
        diff!(
            "HttpDev1EnDis",
            expected_attrs.http_device_1_enabled_disabled,
            EnabledDisabled
        );
        diff!(
            "PxeDev1EnDis",
            expected_attrs.pxe_device_1_enabled_disabled,
            EnabledDisabled
        );
        diff!(
            "HttpDev1Interface",
            expected_attrs.http_device_1_interface,
            String
        );
        diff!(
            "HttpDev1TlsMode",
            expected_attrs.http_device_1_tls_mode,
            dell::TlsMode
        );

        let manager_attrs = self.manager_dell_oem_attributes().await?;
        let expected = HashMap::from([
            ("WebServer.1.HostHeaderCheck", "Disabled"),
            ("IPMILan.1.Enable", "Enabled"),
            ("OS-BMC.1.AdminState", "Disabled"),
        ]);
        for (key, exp) in expected {
            let act = match manager_attrs.get(key) {
                Some(v) => v,
                // Only available in iDRAC 9, skip if it doesn't exist
                None if key == "OS-BMC.1.AdminState" => continue,
                None => {
                    return Err(RedfishError::MissingKey {
                        key: key.to_string(),
                        url: "Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}"
                            .to_string(),
                    })
                }
            };
            if act != exp {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    expected: exp.to_string(),
                    actual: act.to_string(),
                });
            }
        }

        let bmc_remote_access = self.bmc_remote_access_status().await?;
        if !bmc_remote_access.is_fully_enabled() {
            diffs.push(MachineSetupDiff {
                key: "bmc_remote_access".to_string(),
                expected: "Enabled".to_string(),
                actual: bmc_remote_access.status.to_string(),
            });
        }

        Ok(diffs)
    }

    async fn perform_ac_power_cycle(&self) -> Result<(), RedfishError> {
        self.clear_pending().await?;

        // Set PowerCycleRequest in BIOS settings
        let apply_time = dell::SetSettingsApplyTime {
            apply_time: dell::RedfishSettingsApplyTime::OnReset,
        };

        let mut attributes = HashMap::new();
        attributes.insert(
            "PowerCycleRequest".to_string(),
            serde_json::Value::String("FullPowerCycle".to_string()),
        );

        let set_attrs = dell::GenericSetBiosAttrs {
            redfish_settings_apply_time: apply_time,
            attributes,
        };

        let url = format!("Systems/{}/Bios/Settings", self.s.system_id());
        let result = self.s.client.patch(&url, set_attrs).await;

        // Handle intermittent 400 errors for read-only attributes
        if let Err(RedfishError::HTTPErrorCode {
            status_code,
            response_body,
            ..
        }) = &result
        {
            if status_code.as_u16() == 400 && response_body.contains("read-only") {
                return Err(RedfishError::GenericError {
                    error: "Failed to set PowerCycleRequest BIOS attribute due to read-only dependencies. Please reboot the machine and try again.".to_string(),
                });
            }
        }
        result?;

        // Apply the setting based on current power state
        let current_power_state = self.s.get_power_state().await?;
        match current_power_state {
            PowerState::Off => self.s.power(SystemPowerControl::On).await,
            _ => self.s.power(SystemPowerControl::GracefulRestart).await,
        }
    }

    // No changes can be applied if there are pending jobs
    async fn delete_job_queue(&self) -> Result<(), RedfishError> {
        // The queue can't be cleared if system lockdown is enabled
        if self.is_lockdown().await? {
            return Err(RedfishError::Lockdown);
        }

        let url = format!(
            "Managers/{}/Oem/Dell/DellJobService/Actions/DellJobService.DeleteJobQueue",
            self.s.manager_id()
        );
        let mut body = HashMap::new();
        body.insert("JobID", "JID_CLEARALL".to_string());
        self.s.client.post(&url, body).await.map(|_resp| ())
    }

    // is_lockdown checks if system lockdown is enabled.
    async fn is_lockdown(&self) -> Result<bool, RedfishError> {
        let (attrs, url) = self.manager_attributes().await?;
        let system_lockdown = jsonmap::get_str(&attrs, "Lockdown.1.SystemLockdown", &url)?;

        let enabled = EnabledDisabled::Enabled.to_string();
        Ok(system_lockdown == enabled)
    }

    async fn set_boot_first(
        &self,
        entry: dell::BootDevices,
        once: bool,
    ) -> Result<(), RedfishError> {
        let apply_time = dell::SetSettingsApplyTime {
            apply_time: dell::RedfishSettingsApplyTime::OnReset,
        };
        let boot_entry = dell::ServerBoot {
            first_boot_device: entry,
            boot_once: if once {
                EnabledDisabled::Enabled
            } else {
                EnabledDisabled::Disabled
            },
        };
        let boot = dell::ServerBootAttrs {
            server_boot: boot_entry,
        };
        let set_boot = dell::SetFirstBootDevice {
            redfish_settings_apply_time: apply_time,
            attributes: boot,
        };
        let manager_id = self.s.manager_id();
        let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");
        self.s
            .client
            .patch(&url, set_boot)
            .await
            .map(|_status_code| ())
    }

    async fn set_idrac_lockdown(&self, enabled: EnabledDisabled) -> Result<(), RedfishError> {
        let manager_id: &str = self.s.manager_id();
        let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");

        let mut lockdown = HashMap::new();
        lockdown.insert("Lockdown.1.SystemLockdown", enabled.to_string());

        let mut attributes = HashMap::new();
        attributes.insert("Attributes", lockdown);

        self.s
            .client
            .patch(&url, attributes)
            .await
            .map(|_status_code| ())
    }

    async fn enable_bmc_lockdown(&self, entry: dell::BootDevices) -> Result<(), RedfishError> {
        let apply_time = dell::SetSettingsApplyTime {
            apply_time: dell::RedfishSettingsApplyTime::OnReset,
        };

        // First change all settings except lockdown, because that applies immediately
        // and prevents the other settings being applied.
        let boot_entry = dell::ServerBoot {
            first_boot_device: entry,
            boot_once: EnabledDisabled::Disabled,
        };
        let lockdown = dell::BmcLockdown {
            system_lockdown: None,
            racadm_enable: Some(EnabledDisabled::Disabled),
            server_boot: Some(boot_entry),
        };
        let set_bmc_lockdown = dell::SetBmcLockdown {
            redfish_settings_apply_time: apply_time,
            attributes: lockdown,
        };
        let manager_id = self.s.manager_id();
        let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");
        self.s
            .client
            .patch(&url, set_bmc_lockdown)
            .await
            .map(|_status_code| ())?;

        // Now lockdown
        let lockdown = dell::BmcLockdown {
            system_lockdown: Some(EnabledDisabled::Enabled),
            racadm_enable: None,
            server_boot: None,
        };
        let set_bmc_lockdown = dell::SetBmcLockdown {
            redfish_settings_apply_time: apply_time,
            attributes: lockdown,
        };
        self.s
            .client
            .patch(&url, set_bmc_lockdown)
            .await
            .map(|_status_code| ())
    }

    async fn disable_bios_lockdown(&self) -> Result<(), RedfishError> {
        let apply_time = dell::SetSettingsApplyTime {
            apply_time: dell::RedfishSettingsApplyTime::OnReset, // requires reboot to apply
        };
        let lockdown = dell::BiosLockdownAttrs {
            in_band_manageability_interface: EnabledDisabled::Enabled,
            uefi_variable_access: dell::UefiVariableAccessSettings::Standard,
        };
        let set_lockdown_attrs = dell::SetBiosLockdownAttrs {
            redfish_settings_apply_time: apply_time,
            attributes: lockdown,
        };
        let url = format!("Systems/{}/Bios/Settings/", self.s.system_id());
        // Sometimes, these settings are read only.  Ignore those errors trying to set them.
        let ret = self
            .s
            .client
            .patch(&url, set_lockdown_attrs)
            .await
            .map(|_status_code| ());
        if let Err(RedfishError::HTTPErrorCode {
            url: _,
            status_code,
            response_body,
        }) = &ret
        {
            if status_code.as_u16() == 400 && response_body.contains("read-only") {
                return Ok(());
            }
        }
        ret
    }

    async fn disable_bmc_lockdown(&self, entry: dell::BootDevices) -> Result<(), RedfishError> {
        let apply_time = dell::SetSettingsApplyTime {
            apply_time: dell::RedfishSettingsApplyTime::Immediate, // bmc settings don't require reboot
        };
        let boot_entry = dell::ServerBoot {
            first_boot_device: entry,
            boot_once: EnabledDisabled::Disabled,
        };
        let lockdown = dell::BmcLockdown {
            system_lockdown: Some(EnabledDisabled::Disabled),
            racadm_enable: Some(EnabledDisabled::Enabled),
            server_boot: Some(boot_entry),
        };
        let set_bmc_lockdown = dell::SetBmcLockdown {
            redfish_settings_apply_time: apply_time,
            attributes: lockdown,
        };
        let manager_id = self.s.manager_id();
        let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");
        self.s
            .client
            .patch(&url, set_bmc_lockdown)
            .await
            .map(|_status_code| ())
    }

    async fn setup_bmc_remote_access(&self) -> Result<(), RedfishError> {
        // Try the regular Attributes path first (iDRAC 9 and earlier)
        match self.setup_bmc_remote_access_standard().await {
            Ok(()) => return Ok(()),
            Err(RedfishError::HTTPErrorCode {
                status_code: StatusCode::NOT_FOUND,
                ..
            }) => {
                // Regular path doesn't exist, fall back to OEM path (iDRAC 10+)
                tracing::info!("Managers/Attributes not found, using OEM DellAttributes path");
            }
            Err(e) => return Err(e),
        }

        self.setup_bmc_remote_access_oem().await
    }

    /// Setup BMC remote access via standard Attributes path (iDRAC 9 and earlier).
    async fn setup_bmc_remote_access_standard(&self) -> Result<(), RedfishError> {
        let apply_time = dell::SetSettingsApplyTime {
            apply_time: dell::RedfishSettingsApplyTime::Immediate,
        };
        let serial_redirect = dell::SerialRedirection {
            enable: EnabledDisabled::Enabled,
        };
        let ipmi_sol_settings = dell::IpmiSol {
            enable: EnabledDisabled::Enabled,
            baud_rate: "115200".to_string(),
            min_privilege: "Administrator".to_string(),
        };
        let remote_access = dell::BmcRemoteAccess {
            ssh_enable: EnabledDisabled::Enabled,
            serial_redirection: serial_redirect,
            ipmi_lan_enable: EnabledDisabled::Enabled,
            ipmi_sol: ipmi_sol_settings,
        };
        let set_remote_access = dell::SetBmcRemoteAccess {
            redfish_settings_apply_time: apply_time,
            attributes: remote_access,
        };
        let url = format!("Managers/{}/Attributes", self.s.manager_id());
        self.s
            .client
            .patch(&url, set_remote_access)
            .await
            .map(|_status_code| ())
    }

    /// Setup BMC remote access via OEM DellAttributes path (iDRAC 10).
    async fn setup_bmc_remote_access_oem(&self) -> Result<(), RedfishError> {
        let manager_id = self.s.manager_id();
        let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");

        let attributes = HashMap::from([
            ("SerialRedirection.1.Enable", "Enabled"),
            ("IPMISOL.1.Enable", "Enabled"),
            ("IPMISOL.1.BaudRate", "115200"),
            ("IPMISOL.1.MinPrivilege", "Administrator"),
            ("SSH.1.Enable", "Enabled"),
            ("IPMILan.1.Enable", "Enabled"),
        ]);

        let body = HashMap::from([("Attributes", attributes)]);
        self.s.client.patch(&url, body).await.map(|_| ())
    }

    async fn bmc_remote_access_status(&self) -> Result<Status, RedfishError> {
        let (attrs, _) = self.manager_attributes().await?;
        let expected = vec![
            // "any" means any value counts as correctly disabled
            ("SerialRedirection.1.Enable", "Enabled", "Disabled"),
            ("IPMISOL.1.BaudRate", "115200", "any"),
            ("IPMISOL.1.Enable", "Enabled", "Disabled"),
            ("IPMISOL.1.MinPrivilege", "Administrator", "any"),
            ("SSH.1.Enable", "Enabled", "Disabled"),
            ("IPMILan.1.Enable", "Enabled", "Disabled"),
        ];

        // url is for error messages only
        let manager_id = self.s.manager_id();
        let url = &format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");

        let mut message = String::new();
        let mut enabled = true;
        let mut disabled = true;
        for (key, val_enabled, val_disabled) in expected {
            let val_current = jsonmap::get_str(&attrs, key, url)?;
            message.push_str(&format!("{key}={val_current} "));
            if val_current != val_enabled {
                enabled = false;
            }
            if val_current != val_disabled && val_disabled != "any" {
                disabled = false;
            }
        }

        Ok(Status {
            message,
            status: match (enabled, disabled) {
                (true, _) => StatusInternal::Enabled,
                (_, true) => StatusInternal::Disabled,
                _ => StatusInternal::Partial,
            },
        })
    }

    async fn bios_serial_console_status(&self) -> Result<Status, RedfishError> {
        let mut message = String::new();

        // Start with true, then check every value to see whether it means things are not setup
        // correctly, and set the value to false.
        // Note that there are three results: Enabled, Disabled, and Partial, so enabled and
        // disabled can both be false by the end. They cannot both be true.
        let mut enabled = true;
        let mut disabled = true;

        let url = &format!("Systems/{}/Bios", self.s.system_id());
        let (_status_code, bios): (_, dell::Bios) = self.s.client.get(url).await?;
        let bios = bios.attributes;

        let val = bios.serial_comm;
        message.push_str(&format!(
            "serial_comm={} ",
            val.as_ref().unwrap_or(&"unknown".to_string())
        ));
        if let Some(x) = &val {
            match x.parse().map_err(|err| RedfishError::InvalidValue {
                err,
                url: url.to_string(),
                field: "serial_comm".to_string(),
            })? {
                dell::SerialCommSettings::OnConRedir
                | dell::SerialCommSettings::OnConRedirAuto
                | dell::SerialCommSettings::OnConRedirCom1
                | dell::SerialCommSettings::OnConRedirCom2 => {
                    // enabled
                    disabled = false;
                }
                dell::SerialCommSettings::Off => {
                    // disabled
                    enabled = false;
                }
                _ => {
                    // someone messed with it manually
                    enabled = false;
                    disabled = false;
                }
            }
        }

        let val = bios.redir_after_boot;
        message.push_str(&format!(
            "redir_after_boot={} ",
            val.as_ref().unwrap_or(&"unknown".to_string())
        ));
        if let Some(x) = &val {
            match x.parse().map_err(|err| RedfishError::InvalidValue {
                err,
                url: url.to_string(),
                field: "redir_after_boot".to_string(),
            })? {
                EnabledDisabled::Enabled => {
                    disabled = false;
                }
                EnabledDisabled::Disabled => {
                    enabled = false;
                }
            }
        }

        // All of these need a specific value for serial console access to work.
        // Any other value counts as correctly disabled.

        let val = bios.serial_port_address;
        message.push_str(&format!(
            "serial_port_address={} ",
            val.as_ref().unwrap_or(&"unknown".to_string())
        ));
        if let Some(x) = &val {
            // Accept both legacy (Com1) and newer BIOS format (Serial1Com2Serial2Com1)
            if *x != dell::SerialPortSettings::Com1.to_string()
                && *x != dell::SerialPortSettings::Serial1Com2Serial2Com1.to_string()
            {
                enabled = false;
            }
        }

        let val = bios.ext_serial_connector;
        message.push_str(&format!(
            "ext_serial_connector={} ",
            val.as_ref().unwrap_or(&"unknown".to_string())
        ));
        if let Some(x) = &val {
            if *x != dell::SerialPortExtSettings::Serial1.to_string() {
                enabled = false;
            }
        }

        let val = bios.fail_safe_baud;
        message.push_str(&format!(
            "fail_safe_baud={} ",
            val.as_ref().unwrap_or(&"unknown".to_string())
        ));
        if let Some(x) = &val {
            if x != "115200" {
                enabled = false;
            }
        }

        let val = bios.con_term_type;
        message.push_str(&format!(
            "con_term_type={} ",
            val.as_ref().unwrap_or(&"unknown".to_string())
        ));
        if let Some(x) = &val {
            if *x != dell::SerialPortTermSettings::Vt100Vt220.to_string() {
                enabled = false;
            }
        }

        Ok(Status {
            message,
            status: match (enabled, disabled) {
                (true, _) => StatusInternal::Enabled,
                (_, true) => StatusInternal::Disabled,
                _ => StatusInternal::Partial,
            },
        })
    }

    // dell stores the sel as part of the manager
    async fn get_system_event_log(&self) -> Result<Vec<LogEntry>, RedfishError> {
        let manager_id = self.s.manager_id();
        let url = format!("Managers/{manager_id}/LogServices/Sel/Entries");
        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.s.client.get(&url).await?;
        let log_entries = log_entry_collection.members;
        Ok(log_entries)
    }

    // manager_attributes fetches Dell manager attributes and returns them as a JSON Map.
    // Second value in tuple is URL we used to fetch attributes, for diagnostics.
    async fn manager_attributes(
        &self,
    ) -> Result<(serde_json::Map<String, serde_json::Value>, String), RedfishError> {
        let manager_id = self.s.manager_id();
        let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");
        let (_status_code, mut body): (_, HashMap<String, serde_json::Value>) =
            self.s.client.get(&url).await?;
        let attrs = jsonmap::extract_object(&mut body, "Attributes", &url)?;
        Ok((attrs, url))
    }

    /// Extra Dell-specific attributes we need to set that are not BIOS attributes
    async fn machine_setup_oem(
        &self,
        extra_attrs: &HashMap<String, serde_json::Value>,
    ) -> Result<(), RedfishError> {
        let manager_id = self.s.manager_id();
        let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");

        let current_attrs = self.manager_dell_oem_attributes().await?;

        let mut attributes: HashMap<String, serde_json::Value> = HashMap::new();
        // racadm set idrac.webserver.HostHeaderCheck 0
        attributes.insert(
            "WebServer.1.HostHeaderCheck".to_string(),
            serde_json::json!("Disabled"),
        );
        // racadm set iDRAC.IPMILan.Enable 1
        attributes.insert("IPMILan.1.Enable".to_string(), serde_json::json!("Enabled"));

        // Only available in iDRAC 9
        if current_attrs.get("OS-BMC.1.AdminState").is_some() {
            attributes.insert(
                "OS-BMC.1.AdminState".to_string(),
                serde_json::json!("Disabled"),
            );
        }

        // Merge config-driven OEM attributes (e.g. PSU Hot Spare settings)
        attributes.extend(extra_attrs.clone());

        let body = HashMap::from([("Attributes", attributes)]);
        self.s.client.patch(&url, body).await?;
        Ok(())
    }

    async fn manager_dell_oem_attributes(&self) -> Result<serde_json::Value, RedfishError> {
        let manager_id = self.s.manager_id();
        let url = format!("Managers/{manager_id}/Oem/Dell/DellAttributes/{manager_id}");
        let (_status_code, mut body): (_, HashMap<String, serde_json::Value>) =
            self.s.client.get(&url).await?;
        body.remove("Attributes")
            .ok_or_else(|| RedfishError::MissingKey {
                key: "Attributes".to_string(),
                url,
            })
    }

    // TPM is enabled by default so we never call this.
    #[allow(dead_code)]
    async fn enable_tpm(&self) -> Result<(), RedfishError> {
        let apply_time = dell::SetSettingsApplyTime {
            apply_time: dell::RedfishSettingsApplyTime::OnReset, // requires reboot to apply
        };
        let tpm = dell::BiosTpmAttrs {
            tpm_security: OnOff::On,
            tpm2_hierarchy: dell::Tpm2HierarchySettings::Enabled,
        };
        let set_tpm_enabled = dell::SetBiosTpmAttrs {
            redfish_settings_apply_time: apply_time,
            attributes: tpm,
        };
        let url = format!("Systems/{}/Bios/Settings/", self.s.system_id());
        self.s
            .client
            .patch(&url, set_tpm_enabled)
            .await
            .map(|_status_code| ())
    }

    // Dell supports disabling the TPM. Why would we do this?
    // Lenovo does not support disabling TPM2.0
    #[allow(dead_code)]
    async fn disable_tpm(&self) -> Result<(), RedfishError> {
        let apply_time = dell::SetSettingsApplyTime {
            apply_time: dell::RedfishSettingsApplyTime::OnReset, // requires reboot to apply
        };
        let tpm = dell::BiosTpmAttrs {
            tpm_security: OnOff::Off,
            tpm2_hierarchy: dell::Tpm2HierarchySettings::Disabled,
        };
        let set_tpm_disabled = dell::SetBiosTpmAttrs {
            redfish_settings_apply_time: apply_time,
            attributes: tpm,
        };
        let url = format!("Systems/{}/Bios/Settings/", self.s.system_id());
        self.s
            .client
            .patch(&url, set_tpm_disabled)
            .await
            .map(|_status_code| ())
    }

    pub async fn create_bios_config_job(&self) -> Result<String, RedfishError> {
        let url = "Managers/iDRAC.Embedded.1/Oem/Dell/Jobs";

        let mut arg = HashMap::new();
        arg.insert(
            "TargetSettingsURI",
            "/redfish/v1/Systems/System.Embedded.1/Bios/Settings".to_string(),
        );

        match self.s.client.post(url, arg).await? {
            (_, Some(headers)) => self.parse_job_id_from_response_headers(url, headers).await,
            (_, None) => Err(RedfishError::NoHeader),
        }
    }

    async fn machine_setup_attrs(
        &self,
        nic_slot: &str,
    ) -> Result<dell::MachineBiosAttrs, RedfishError> {
        let curr_bios_attributes = self.s.bios_attributes().await?;

        // RedirAfterBoot: Not available in iDRAC 10
        let redir_after_boot = curr_bios_attributes
            .get("RedirAfterBoot")
            .is_some()
            .then_some(EnabledDisabled::Enabled);

        // BootMode: Read-only in iDRAC 10 (UEFI-only hardware), writable in iDRAC 9
        let boot_mode = match curr_bios_attributes
            .get("BootMode")
            .and_then(|v| v.as_str())
        {
            Some("Uefi") => None,                // Already correct, don't touch it
            Some(_) => Some("Uefi".to_string()), // Try to fix it (iDRAC 9)
            None => None,                        // Attribute doesn't exist
        };

        // Detect newer iDRAC by checking SerialPortAddress format.
        // Newer Dell BIOS uses Serial1Com*Serial2Com* format and OnConRedirAuto for SerialComm.
        let is_newer_idrac = curr_bios_attributes
            .get("SerialPortAddress")
            .and_then(|v| v.as_str())
            .map(|v| v.starts_with("Serial1"))
            .unwrap_or(false);

        let (serial_port_address, serial_comm) = if is_newer_idrac {
            (
                dell::SerialPortSettings::Serial1Com2Serial2Com1,
                dell::SerialCommSettings::OnConRedirAuto,
            )
        } else {
            (
                dell::SerialPortSettings::Com1,
                dell::SerialCommSettings::OnConRedir,
            )
        };

        Ok(dell::MachineBiosAttrs {
            in_band_manageability_interface: EnabledDisabled::Disabled,
            uefi_variable_access: dell::UefiVariableAccessSettings::Standard,
            serial_comm,
            serial_port_address,
            fail_safe_baud: "115200".to_string(),
            con_term_type: dell::SerialPortTermSettings::Vt100Vt220,
            redir_after_boot,
            sriov_global_enable: EnabledDisabled::Enabled,
            tpm_security: OnOff::On,
            tpm2_hierarchy: dell::Tpm2HierarchySettings::Clear,
            tpm2_algorithm: dell::Tpm2Algorithm::SHA256,
            http_device_1_enabled_disabled: EnabledDisabled::Enabled,
            pxe_device_1_enabled_disabled: EnabledDisabled::Disabled,
            boot_mode,
            http_device_1_interface: nic_slot.to_string(),
            set_boot_order_en: nic_slot.to_string(),
            http_device_1_tls_mode: dell::TlsMode::None,
            // We used to use this to disable all boot options other than the PXE boot option we wanted
            // We found that it can cause the boot disk option to be disabled in the termination flow.
            set_boot_order_dis: String::new(),
        })
    }

    /// Dells endpoint to change the UEFI password has a bug for updating it once it is set.
    /// Use the ImportSystemConfiguration endpoint as a hack to clear the UEFI password instead.
    /// Detailed here: https://github.com/dell/iDRAC-Redfish-Scripting/issues/308
    async fn clear_uefi_password_via_import(
        &self,
        current_uefi_password: &str,
    ) -> Result<String, RedfishError> {
        let system_configuration = SystemConfiguration {
            shutdown_type: "Forced".to_string(),
            share_parameters: ShareParameters {
                target: "BIOS".to_string(),
            },
            import_buffer: format!(
                r##"<SystemConfiguration><Component FQDD="BIOS.Setup.1-1"><!-- <Attribute Name="OldSysPassword"></Attribute>--><!-- <Attribute Name="NewSysPassword"></Attribute>--><Attribute Name="OldSetupPassword">{}</Attribute><Attribute Name="NewSetupPassword"></Attribute></Component></SystemConfiguration>"##,
                XmlPcdata(current_uefi_password)
            ),
        };

        self.import_system_configuration(system_configuration).await
    }

    /// PATCHes a Dell settings endpoint and resolves the optional config job
    /// id from the response.
    ///
    /// Dell responds to a settings PATCH in one of two ways:
    /// - iDRAC9/iDRAC10 schedule a config job and return `202 Accepted` with a
    ///   `location` header pointing at the job; we parse and return its id so
    ///   the caller can wait on the job (`Some(job_id)`).
    /// - Newer iDRAC (e.g. 17G PowerEdge R770), when the requested settings are
    ///   already staged in the pending-settings buffer, return `200 OK` with no
    ///   `location` header (message ID SYS011 "successfully committed", or
    ///   SYS413). No job was scheduled, so there is nothing to wait on and we
    ///   return `None`.
    ///
    /// We deliberately do NOT parse the response message ID: it varies across
    /// iDRAC versions and conditions (SYS011 vs SYS413, etc.), so gating on a
    /// specific value is brittle. A `200` with no job `location` is unambiguous
    /// on its own; any other status surfaces as [`RedfishError::NoHeader`].
    async fn patch_settings_for_job_id<B>(
        &self,
        url: &str,
        body: B,
    ) -> Result<Option<String>, RedfishError>
    where
        B: Serialize + std::fmt::Debug,
    {
        // Pass `body` by reference so it stays available to log on the no-job
        // path below; serde implements `Serialize`/`Debug` for `&B`.
        let (status_code, resp_headers) = self.s.client.patch(url, &body).await?;

        match resp_headers
            .as_ref()
            .filter(|headers| headers.contains_key("location"))
        {
            Some(headers) => Ok(Some(
                self.parse_job_id_from_response_headers(url, headers.clone())
                    .await?,
            )),
            None if status_code == StatusCode::OK => {
                tracing::info!(
                    bmc_ip = %self.s.client.host(),
                    %url,
                    %status_code,
                    ?resp_headers,
                    ?body,
                    "Dell settings PATCH applied without scheduling a job (no location header); treating as success"
                );
                Ok(None)
            }
            None => Err(RedfishError::NoHeader),
        }
    }

    async fn parse_job_id_from_response_headers(
        &self,
        url: &str,
        resp_headers: HeaderMap,
    ) -> Result<String, RedfishError> {
        let key = "location";
        let jid = resp_headers
            .get(key)
            .ok_or_else(|| RedfishError::MissingKey {
                key: key.to_string(),
                url: url.to_string(),
            })?
            .to_str()
            .map_err(|e| RedfishError::InvalidValue {
                url: url.to_string(),
                field: key.to_string(),
                err: InvalidValueError(e.to_string()),
            })?
            .split('/')
            .next_back()
            .ok_or_else(|| RedfishError::InvalidValue {
                url: url.to_string(),
                field: key.to_string(),
                err: InvalidValueError("unable to parse job_id from location string".to_string()),
            })?
            .to_string();
        Ok(jid)
    }

    /// import_system_configuration returns the job ID for importing this sytem configuration
    async fn import_system_configuration(
        &self,
        system_configuration: SystemConfiguration,
    ) -> Result<String, RedfishError> {
        let url = "Managers/iDRAC.Embedded.1/Actions/Oem/EID_674_Manager.ImportSystemConfiguration";
        let (_status_code, _resp_body, resp_headers): (
            _,
            Option<HashMap<String, serde_json::Value>>,
            Option<HeaderMap>,
        ) = self
            .s
            .client
            .req(
                Method::POST,
                url,
                Some(system_configuration),
                None,
                None,
                Vec::new(),
            )
            .await?;

        match resp_headers {
            Some(headers) => self.parse_job_id_from_response_headers(url, headers).await,
            None => Err(RedfishError::NoHeader),
        }
    }

    async fn get_dpu_nw_device_function(
        &self,
        boot_interface: crate::BootInterfaceRef<'_>,
    ) -> Result<NetworkDeviceFunction, RedfishError> {
        let chassis = self.get_chassis(self.s.system_id()).await?;
        let na_id = match chassis.network_adapters {
            Some(id) => id,
            None => {
                let chassis_odata_url = chassis
                    .odata
                    .map(|o| o.odata_id)
                    .unwrap_or_else(|| "empty_odata_id_url".to_string());
                return Err(RedfishError::MissingKey {
                    key: "network_adapters".to_string(),
                    url: chassis_odata_url,
                });
            }
        };

        let rc_nw_adapter: ResourceCollection<NetworkAdapter> = self
            .s
            .get_collection(na_id)
            .await
            .and_then(|r| r.try_get())?;

        // Get nw_device_functions
        for nw_adapter in rc_nw_adapter.members {
            let nw_dev_func_oid = match nw_adapter.network_device_functions {
                Some(x) => x,
                None => {
                    // TODO debug
                    continue;
                }
            };

            let rc_nw_func: ResourceCollection<NetworkDeviceFunction> = self
                .get_collection(nw_dev_func_oid)
                .await
                .and_then(|r| r.try_get())?;

            for nw_dev_func in rc_nw_func.members {
                if nw_dev_func_matches(&nw_dev_func, boot_interface) {
                    return Ok(nw_dev_func);
                }
            }
        }

        Err(RedfishError::GenericError {
            error: format!("could not find network device function for {boot_interface:?}"),
        })
    }

    async fn get_dell_nic_info(
        &self,
        boot_interface: crate::BootInterfaceRef<'_>,
    ) -> Result<serde_json::Map<String, Value>, RedfishError> {
        let nw_device_function = self.get_dpu_nw_device_function(boot_interface).await?;

        let oem = nw_device_function
            .oem
            .ok_or_else(|| RedfishError::GenericError {
                error: "OEM information is missing".to_string(),
            })?;

        let oem_dell = oem.get("Dell").ok_or_else(|| RedfishError::GenericError {
            error: "Dell OEM information is missing".to_string(),
        })?;

        let oem_dell_map = oem_dell
            .as_object()
            .ok_or_else(|| RedfishError::GenericError {
                error: "Dell OEM information is not a valid object".to_string(),
            })?;

        let dell_nic_map = oem_dell_map
            .get("DellNIC")
            .and_then(|dell_nic| dell_nic.as_object())
            .ok_or_else(|| RedfishError::GenericError {
                error: "DellNIC information is not a valid object or is missing".to_string(),
            })?;

        Ok(dell_nic_map.to_owned())
    }

    /// Resolve the Dell NIC slot id (e.g. `"NIC.Slot.40-1-1"`) that the
    /// `HttpDev1Interface` BIOS attribute and the first-boot-option check key on
    /// for a boot interface.
    ///
    /// [`crate::BootInterfaceRef::InterfaceId`] and
    /// [`crate::BootInterfaceRef::Pair`] already contain the slot id, so it is
    /// used directly. This stable identifier survives a NicMode flip or any case
    /// where the `NetworkDeviceFunction`'s `Ethernet.MACAddress` is empty.
    /// [`crate::BootInterfaceRef::Mac`] is resolved to the slot through the
    /// `NetworkDeviceFunction`, matched by MAC. `None` is the zero-DPU case.
    async fn nic_slot_for(
        &self,
        boot_interface: Option<crate::BootInterfaceRef<'_>>,
    ) -> Result<String, RedfishError> {
        Ok(match boot_interface {
            Some(crate::BootInterfaceRef::InterfaceId(id))
            | Some(crate::BootInterfaceRef::Pair {
                interface_id: id, ..
            }) => id.to_string(),
            Some(crate::BootInterfaceRef::Mac(mac)) => self.dpu_nic_slot(&mac.to_string()).await?,
            None => String::new(),
        })
    }

    // Returns a string like "NIC.Slot.5-1"
    async fn dpu_nic_slot(&self, mac_address: &str) -> Result<String, RedfishError> {
        let mac: mac_address::MacAddress =
            mac_address
                .parse()
                .map_err(|e| RedfishError::GenericError {
                    error: format!("could not parse boot interface MAC `{mac_address}`: {e}"),
                })?;
        let dell_nic_info = self
            .get_dell_nic_info(crate::BootInterfaceRef::Mac(mac))
            .await?;

        let nic_slot = dell_nic_info
            .get("Id")
            .and_then(|id| id.as_str())
            .ok_or_else(|| RedfishError::GenericError {
                error: "NIC slot ID is missing or not a valid string".to_string(),
            })?
            .to_string();

        Ok(nic_slot)
    }

    async fn get_boss_controller(&self) -> Result<Option<String>, RedfishError> {
        let url = "Systems/System.Embedded.1/Storage";
        let (_status_code, storage_collection): (_, StorageCollection) =
            self.s.client.get(url).await?;
        for controller in storage_collection.members {
            if controller.odata_id.contains("BOSS") {
                let boss_controller_id =
                    controller.odata_id.split('/').next_back().ok_or_else(|| {
                        RedfishError::InvalidValue {
                            url: url.to_string(),
                            field: "odata_id".to_string(),
                            err: InvalidValueError(format!(
                                "unable to parse boss_controller_id from {}",
                                controller.odata_id
                            )),
                        }
                    })?;
                return Ok(Some(boss_controller_id.to_string()));
            }
        }

        Ok(None)
    }

    async fn decommission_controller(&self, controller_id: &str) -> Result<String, RedfishError> {
        // wait for the lifecycle controller status to become Ready before decomissioning the boss controller
        // https://github.com/dell/idrac-Redfish-Scripting/issues/323
        self.lifecycle_controller_is_ready().await?;

        let url: String = format!("Systems/System.Embedded.1/Storage/{controller_id}/Actions/Oem/DellStorage.ControllerDrivesDecommission");
        let mut arg = HashMap::new();
        arg.insert("@Redfish.OperationApplyTime", "Immediate");

        match self.s.client.post(&url, arg).await? {
            (_, Some(headers)) => self.parse_job_id_from_response_headers(&url, headers).await,
            (_, None) => Err(RedfishError::NoHeader),
        }
    }

    async fn get_storage_drives(&self, controller_id: &str) -> Result<Value, RedfishError> {
        let url = format!("Systems/System.Embedded.1/Storage/{controller_id}");
        let (_status_code, body): (_, HashMap<String, serde_json::Value>) =
            self.s.client.get(&url).await?;
        jsonmap::get_value(&body, "Drives", &url).cloned()
    }

    async fn create_storage_volume(
        &self,
        controller_id: &str,
        volume_name: &str,
        raid_type: &str,
        drive_info: Value,
    ) -> Result<String, RedfishError> {
        if volume_name.len() > 15 || volume_name.is_empty() {
            return Err(RedfishError::GenericError {
                error: format!(
                    "invalid volume name ({volume_name}); must be between 1 and 15 characters long"
                ),
            });
        }

        // wait for the lifecycle controller status to become Ready
        self.lifecycle_controller_is_ready().await?;

        let url: String = format!("Systems/System.Embedded.1/Storage/{controller_id}/Volumes");
        let arg = HashMap::from([
            ("Name", Value::String(volume_name.to_string())),
            ("RAIDType", Value::String(raid_type.to_string())),
            ("Links", serde_json::json!({ "Drives": drive_info })),
        ]);

        match self.s.client.post(&url, arg).await? {
            (_, Some(headers)) => self.parse_job_id_from_response_headers(&url, headers).await,
            (_, None) => Err(RedfishError::NoHeader),
        }
    }

    async fn get_lifecycle_controller_status(&self) -> Result<String, RedfishError> {
        let manager_id = self.s.manager_id();
        let url = format!(
            "Managers/{manager_id}/Oem/Dell/DellLCService/Actions/DellLCService.GetRemoteServicesAPIStatus"
        );
        let arg: HashMap<&'static str, Value> = HashMap::new();
        let (_status_code, resp_body, _resp_headers): (
            _,
            Option<HashMap<String, serde_json::Value>>,
            Option<HeaderMap>,
        ) = self
            .s
            .client
            .req(Method::POST, &url, Some(arg), None, None, Vec::new())
            .await?;

        let lc_status = match resp_body.unwrap_or_default().get("LCStatus") {
            Some(status) => status.as_str().unwrap_or_default().to_string(),
            None => todo!(),
        };

        Ok(lc_status)
    }

    async fn lifecycle_controller_is_ready(&self) -> Result<(), RedfishError> {
        let lc_status = self.get_lifecycle_controller_status().await?;
        if lc_status == "Ready" {
            return Ok(());
        }

        Err(RedfishError::GenericError { error: format!("the lifecycle controller is not ready to accept provisioning requests; lc_status: {lc_status}") })
    }

    // get_expected_dpu_boot_option_name assumes that the HTTP Device One boot option has been enabled
    // and points to the NIC for the boot interface. In the future, we can relax the string matching if
    // we configure other HTTP devices and just match on the NIC's device description.
    async fn get_expected_dpu_boot_option_name(
        &self,
        boot_interface: crate::BootInterfaceRef<'_>,
    ) -> Result<String, RedfishError> {
        let dell_nic_info = self.get_dell_nic_info(boot_interface).await?;

        let device_description = dell_nic_info
            .get("DeviceDescription")
            .and_then(|device_description| device_description.as_str())
            .ok_or_else(|| RedfishError::GenericError {
                error: format!("the NIC Device Description for {boot_interface:?} is missing or not a valid string"),
            })?
            .to_string();

        Ok(format!("HTTP Device 1: {device_description}",))
    }

    async fn get_boot_order(&self) -> Result<Vec<BootOption>, RedfishError> {
        let system = self.s.get_system().await?;

        let boot_options_id =
            system
                .boot
                .boot_options
                .clone()
                .ok_or_else(|| RedfishError::MissingKey {
                    key: "Boot.BootOptions".to_string(),
                    url: system.odata.odata_id.clone(),
                })?;

        let all_boot_options = self
            .s
            .get_collection(boot_options_id)
            .await?
            .try_get::<BootOption>()?
            .members;

        let boot_order = system
            .boot
            .boot_order
            .iter()
            .filter_map(|reference| {
                all_boot_options
                    .iter()
                    .find(|opt| opt.boot_option_reference == *reference)
                    .cloned()
            })
            .collect();

        Ok(boot_order)
    }

    // get_expected_and_actual_first_boot_option assumes that the HTTP Device One boot option has been enabled
    // and points to the NIC for the boot interface. In the future, we can relax the string matching if
    // we configure other HTTP devices and just match on the NIC's device description.
    async fn get_expected_and_actual_first_boot_option(
        &self,
        boot_interface: crate::BootInterfaceRef<'_>,
    ) -> Result<(Option<String>, Option<String>), RedfishError> {
        let expected_first_boot_option = Some(
            self.get_expected_dpu_boot_option_name(boot_interface)
                .await?,
        );
        let boot_order = self.get_boot_order().await?;
        let actual_first_boot_option = boot_order.first().map(|opt| opt.display_name.clone());

        Ok((expected_first_boot_option, actual_first_boot_option))
    }
}

// UpdateParameters is what is sent for a multipart firmware upload's metadata.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct UpdateParameters {
    targets: Vec<String>,
    #[serde(rename = "@Redfish.OperationApplyTime")]
    pub apply_time: String,
    oem: Empty,
}

// The BMC expects to have a {} in its JSON, even though it doesn't seem to do anything with it.  Their implementation must be... interesting.
#[derive(Serialize)]
struct Empty {}

impl UpdateParameters {
    pub fn new(reboot_immediate: bool) -> UpdateParameters {
        let apply_time = match reboot_immediate {
            true => "Immediate",
            false => "OnReset",
        }
        .to_string();
        UpdateParameters {
            targets: vec![],
            apply_time,
            oem: Empty {},
        }
    }
}

// Escapes XML character data for element text content (PCDATA).
// Use this for values inserted between tags, e.g. <x>value</x>.
//
// Do not use for XML attributes; attribute values require different escaping.
struct XmlPcdata<'a>(&'a str);

impl std::fmt::Display for XmlPcdata<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // XML: 2.4 Character Data and Markup
        for c in self.0.chars() {
            match c {
                // The ampersand character (&) and the left angle
                // bracket (<) MUST NOT appear in their literal form,
                // except ... . If they are needed elsewhere, they
                // MUST be escaped.
                '&' => "&amp;".fmt(f)?,
                '<' => "&lt;".fmt(f)?,
                // The right angle bracket (>) may be represented
                // using the string " &gt; ", and MUST, for
                // compatibility, be escaped using ... "&gt;"
                '>' => "&gt;".fmt(f)?,
                _ => c.fmt(f)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{boot_option_name_matches, nw_dev_func_matches, Bmc, XmlPcdata};
    use crate::model::network_device_function::{Ethernet, NetworkDeviceFunction};
    use crate::{BootInterfaceRef, Endpoint, RedfishClientPool};
    use std::collections::HashMap;

    fn ndf_with(id: Option<&str>, mac: Option<&str>) -> NetworkDeviceFunction {
        NetworkDeviceFunction {
            odata: None,
            description: None,
            id: id.map(str::to_string),
            ethernet: Some(Ethernet {
                ethernet_interfaces: None,
                mac_address: mac.map(str::to_string),
                mtu_size: None,
            }),
            name: None,
            net_dev_func_capabilities: None,
            net_dev_func_type: None,
            links: None,
            oem: None,
        }
    }

    // A NicMode-stripped (or otherwise empty-MAC) partition: the EthernetInterface
    // still reports the MAC, but the NetworkDeviceFunction's MACAddress is empty. A
    // by-MAC match can't find it; the stable interface id can. This is the
    // resolution `is_bios_setup` / `machine_setup` rely on so such a host finishes
    // provisioning instead of looping in PollingBiosSetup.
    #[test]
    fn nw_dev_func_matches_by_interface_id_when_ndf_mac_empty() {
        let stripped = ndf_with(Some("NIC.Slot.40-1-1"), None);

        // Exact id matches; a parent NDF id matches a partitioned target.
        assert!(nw_dev_func_matches(
            &stripped,
            BootInterfaceRef::InterfaceId("NIC.Slot.40-1-1")
        ));
        assert!(nw_dev_func_matches(
            &ndf_with(Some("NIC.Slot.40-1"), None),
            BootInterfaceRef::InterfaceId("NIC.Slot.40-1-1")
        ));

        // A MAC can't match an NDF whose MACAddress is empty.
        let mac: mac_address::MacAddress = "C4:70:BD:2C:3C:0A".parse().unwrap();
        assert!(!nw_dev_func_matches(&stripped, BootInterfaceRef::Mac(mac)));

        // A different interface id does not match.
        assert!(!nw_dev_func_matches(
            &stripped,
            BootInterfaceRef::InterfaceId("NIC.Slot.7-1-1")
        ));
    }

    // When the NDF does report a MAC, by-MAC matching still works
    // (case-insensitive), so the existing host path is unchanged.
    #[test]
    fn nw_dev_func_matches_by_mac_when_present() {
        let populated = ndf_with(Some("NIC.Slot.40-1-1"), Some("c4:70:bd:2c:3c:0a"));
        let mac: mac_address::MacAddress = "C4:70:BD:2C:3C:0A".parse().unwrap();
        assert!(nw_dev_func_matches(&populated, BootInterfaceRef::Mac(mac)));
    }

    #[test]
    fn nw_dev_func_pair_matches_by_interface_id() {
        let mac: mac_address::MacAddress = "C4:70:BD:2C:3C:0A".parse().unwrap();
        let pair = BootInterfaceRef::Pair {
            mac_address: mac,
            interface_id: "NIC.Slot.40-1-1",
        };

        assert!(nw_dev_func_matches(
            &ndf_with(Some("NIC.Slot.40-1-1"), None),
            pair,
        ));
        assert!(nw_dev_func_matches(
            &ndf_with(Some("NIC.Slot.40-1"), None),
            pair,
        ));
    }

    #[test]
    fn nw_dev_func_pair_does_not_fall_back_to_mac() {
        let mac: mac_address::MacAddress = "C4:70:BD:2C:3C:0A".parse().unwrap();
        let pair = BootInterfaceRef::Pair {
            mac_address: mac,
            interface_id: "NIC.Slot.40-1-1",
        };
        let matching_mac_wrong_id = ndf_with(Some("NIC.Slot.7-1-1"), Some("c4:70:bd:2c:3c:0a"));

        assert!(!nw_dev_func_matches(&matching_mac_wrong_id, pair));
    }

    #[tokio::test]
    async fn nic_slot_for_pair_uses_interface_id_directly() {
        let pool = RedfishClientPool::builder().build().unwrap();
        let standard = pool
            .create_standard_client(Endpoint::default())
            .expect("test Redfish client should be constructed without a request");
        let bmc = Bmc::new(*standard).unwrap();
        let mac: mac_address::MacAddress = "C4:70:BD:2C:3C:0A".parse().unwrap();

        let got = bmc
            .nic_slot_for(Some(BootInterfaceRef::Pair {
                mac_address: mac,
                interface_id: "NIC.Slot.40-1-1",
            }))
            .await
            .expect("pair should use its interface ID without querying the empty endpoint");

        assert_eq!(got, "NIC.Slot.40-1-1");
    }

    #[test]
    fn boot_option_name_matches_legacy_and_extended_names() {
        let expected = "HTTP Device 1: NIC in Slot 4 Port 1 Partition 1";
        let cases = [
            (expected, true),
            (
                "HTTP Device 1: NIC in Slot 4 Port 1 Partition 1 - Nvidia Network Adapter - 00:11:22:33:44:55 - IPv4",
                true,
            ),
            (
                "HTTP Device 1: NIC in Slot 4 Port 1 Partition 10 - Nvidia Network Adapter - 00:11:22:33:44:55 - IPv4",
                false,
            ),
            (
                "HTTP Device 1: NIC in Slot 4 Port 2 Partition 1 - Nvidia Network Adapter - 00:11:22:33:44:55 - IPv4",
                false,
            ),
            (
                "HTTP Device 1: NIC in Slot 4 Port 1 Partition 1 unexpected suffix",
                false,
            ),
        ];

        for (actual, should_match) in cases {
            assert_eq!(
                boot_option_name_matches(expected, actual),
                should_match,
                "unexpected match result for {actual}"
            );
        }
    }

    // Mirrors the attribute-merge logic in machine_setup_oem so we can test it
    // without a live HTTP connection.
    fn build_oem_attributes(
        extra_attrs: &HashMap<String, serde_json::Value>,
        has_os_bmc: bool,
    ) -> HashMap<String, serde_json::Value> {
        let mut attributes: HashMap<String, serde_json::Value> = HashMap::new();
        attributes.insert(
            "WebServer.1.HostHeaderCheck".to_string(),
            serde_json::json!("Disabled"),
        );
        attributes.insert("IPMILan.1.Enable".to_string(), serde_json::json!("Enabled"));
        if has_os_bmc {
            attributes.insert(
                "OS-BMC.1.AdminState".to_string(),
                serde_json::json!("Disabled"),
            );
        }
        attributes.extend(extra_attrs.clone());
        attributes
    }

    #[test]
    fn test_machine_setup_oem_hardcoded_attrs_present() {
        let extra: HashMap<String, serde_json::Value> = HashMap::new();
        let attrs = build_oem_attributes(&extra, false);
        assert_eq!(
            attrs["WebServer.1.HostHeaderCheck"],
            serde_json::json!("Disabled")
        );
        assert_eq!(attrs["IPMILan.1.Enable"], serde_json::json!("Enabled"));
        assert!(!attrs.contains_key("OS-BMC.1.AdminState"));
    }

    #[test]
    fn test_machine_setup_oem_idrac9_attr_conditionally_added() {
        let extra: HashMap<String, serde_json::Value> = HashMap::new();
        let attrs = build_oem_attributes(&extra, true);
        assert_eq!(attrs["OS-BMC.1.AdminState"], serde_json::json!("Disabled"));
    }

    #[test]
    fn test_machine_setup_oem_extra_attrs_merged() {
        let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
        extra.insert(
            "System.1.psuHotSpare".to_string(),
            serde_json::json!("Disabled"),
        );
        let attrs = build_oem_attributes(&extra, false);
        assert_eq!(attrs["System.1.psuHotSpare"], serde_json::json!("Disabled"));
        // Hardcoded attrs are still present
        assert_eq!(
            attrs["WebServer.1.HostHeaderCheck"],
            serde_json::json!("Disabled")
        );
    }

    #[test]
    fn test_machine_setup_oem_extra_attrs_override_hardcoded() {
        // Ensure extra_attrs win over any hardcoded defaults if keys collide.
        let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
        extra.insert(
            "IPMILan.1.Enable".to_string(),
            serde_json::json!("Disabled"),
        );
        let attrs = build_oem_attributes(&extra, false);
        assert_eq!(attrs["IPMILan.1.Enable"], serde_json::json!("Disabled"));
    }

    #[test]
    fn test_xml_pcdata_escapes_markup_chars() {
        let input = r#"before & <tag> > after"#;
        let escaped = XmlPcdata(input).to_string();
        assert_eq!(escaped, "before &amp; &lt;tag&gt; &gt; after");
    }

    #[test]
    fn test_xml_pcdata_leaves_plain_text_unchanged() {
        let input = "abcXYZ123_- ";
        let escaped = XmlPcdata(input).to_string();
        assert_eq!(escaped, input);
    }

    #[test]
    fn test_xml_pcdata_in_import_buffer_context() {
        let password = r#"a&<b>c"#;
        let xml = format!(
            r##"<Attribute Name="OldSetupPassword">{}</Attribute>"##,
            XmlPcdata(password)
        );
        assert_eq!(
            xml,
            r#"<Attribute Name="OldSetupPassword">a&amp;&lt;b&gt;c</Attribute>"#
        );
    }
}
