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
use serde::Serialize;
use tokio::fs::File;

use crate::{
    model::{
        boot::{self, BootOverride, BootSourceOverrideEnabled, BootSourceOverrideMode},
        chassis::Chassis,
        oem::supermicro::{self, FixedBootOrder},
        service_root::RedfishVendor,
        task::Task,
        update_service::ComponentType,
        BootOption, EnableDisable, InvalidValueError,
    },
    standard::RedfishStandard,
    BiosProfileType, Boot, EnabledDisabled, MachineSetupDiff, MachineSetupStatus, ODataId,
    PCIeDevice, Redfish, RedfishError, Status, StatusInternal, SystemPowerControl,
};

const MELLANOX_UEFI_HTTP_IPV4: &str = "UEFI HTTP IPv4 Mellanox Network Adapter";
const NVIDIA_UEFI_HTTP_IPV4: &str = "UEFI HTTP IPv4 Nvidia Network Adapter";

/// MGX C2 systems use SSIF instead of x86 KCS for in-band BMC communication,
/// so the KCSInterface endpoint doesn't exist. These models require the
/// IPMIHostInterface fallback on Systems/{id}.
const MGX_C2_PROCESSOR_MODULE_MODEL_PREFIX: &str = "PG535";
const MGX_C2_PROCESSOR_MODULE_PART_NUMBER_FRAGMENT: &str = "2G535";

/// Minimum BMC firmware version that exposes `IPMIHostInterface` on
/// `Systems/{id}` for MGX C2 systems.
const MIN_BMC_FW_IPMI_HOST_IFACE: &str = "01.05.01";
const HARD_DISK: &str = "UEFI Hard Disk";
const NETWORK: &str = "UEFI Network";

fn usable_standard_boot_order(boot_order: Vec<String>) -> Option<Vec<String>> {
    (!boot_order.is_empty()).then_some(boot_order)
}

fn is_supported_uefi_http_boot_option(display_name: &str) -> bool {
    display_name.contains(MELLANOX_UEFI_HTTP_IPV4) || display_name.contains(NVIDIA_UEFI_HTTP_IPV4)
}

fn http_boot_option_matches_mac(display_name: &str, boot_interface_mac: &str) -> bool {
    is_supported_uefi_http_boot_option(display_name)
        && display_name
            .to_ascii_uppercase()
            .contains(&boot_interface_mac.to_ascii_uppercase())
}

fn standard_boot_order_diff(
    boot_order: &[String],
    boot_options: &[BootOption],
    boot_interface_mac: &str,
) -> Option<MachineSetupDiff> {
    let target = boot_options
        .iter()
        .find(|option| http_boot_option_matches_mac(&option.display_name, boot_interface_mac));
    let actual_reference = boot_order
        .first()
        .map(|entry| boot::boot_order_entry_reference(entry));

    if target.is_some_and(|option| actual_reference == Some(option.boot_option_reference.as_str()))
    {
        return None;
    }

    let expected = target
        .map(|option| option.display_name.clone())
        .unwrap_or_else(|| "Not found".to_string());
    let actual = actual_reference
        .and_then(|reference| {
            boot_options
                .iter()
                .find(|option| option.boot_option_reference == reference)
        })
        .map(|option| option.display_name.clone())
        .unwrap_or_else(|| "Not found".to_string());

    Some(MachineSetupDiff {
        key: "boot_first".to_string(),
        expected,
        actual,
    })
}

fn fixed_boot_order_diff(
    fixed_boot_order: &[String],
    uefi_network: &[String],
    boot_interface_mac: &str,
) -> Option<MachineSetupDiff> {
    let expected = uefi_network
        .iter()
        .find(|entry| http_boot_option_matches_mac(entry, boot_interface_mac))
        .cloned();
    let actual = fixed_boot_order.first().map(|entry| {
        let network_category = entry
            .strip_prefix(NETWORK)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'));
        if network_category {
            uefi_network
                .first()
                .cloned()
                .unwrap_or_else(|| entry.clone())
        } else if let Some((_, option)) = entry.split_once(':') {
            option.trim().to_string()
        } else {
            entry.clone()
        }
    });

    if expected.is_some() && expected == actual {
        return None;
    }

    Some(MachineSetupDiff {
        key: "boot_first".to_string(),
        expected: expected.unwrap_or_else(|| "Not found".to_string()),
        actual: actual.unwrap_or_else(|| "Not found".to_string()),
    })
}

pub struct Bmc {
    s: RedfishStandard,
}

impl Bmc {
    pub fn new(s: RedfishStandard) -> Result<Bmc, RedfishError> {
        Ok(Bmc { s })
    }
}
impl Redfish for Bmc {
    fn std_redfish(&self) -> &RedfishStandard {
        &self.s
    }
    fn power<'a>(
        &'a self,
        action: SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if action == SystemPowerControl::ACPowercycle {
                let args: HashMap<String, String> =
                    HashMap::from([("ResetType".to_string(), "ACCycle".to_string())]);
                let url = format!(
                    "Systems/{}/Actions/Oem/OemSystemExtensions.Reset",
                    self.s.system_id()
                );
                return self.s.client.post(&url, args).await.map(|_status_code| ());
            }
            self.s.power(action).await
        })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        true
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.factory_reset_bios().await })
    }

    /// Note that you can't use this for initial setup unless you reboot and run it twice.
    /// `boot_first` won't find the Mellanox HTTP device. `uefi_nic_boot_attrs` enables it,
    /// but it won't show until after reboot so that step will fail on first time through.
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
            self.setup_serial_console().await?;

            let bios_attrs = self.machine_setup_attrs().await?;
            let mut attrs = HashMap::new();
            attrs.extend(bios_attrs);
            let body = HashMap::from([("Attributes", attrs)]);
            let url = format!("Systems/{}/Bios", self.s.system_id());
            self.s
                .client
                .patch(&url, body)
                .await
                .map(|_status_code| None)
        })
    }

    fn machine_setup_status<'a>(
        &'a self,
        boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            // Resolve `InterfaceId` to a MAC via the Redfish-standard
            // EthernetInterface resource.
            let resolved_mac = match boot_interface {
                Some(b) => Some(crate::resolve_boot_interface_mac(self, b).await?),
                None => None,
            };
            let boot_interface_mac = resolved_mac.as_deref();

            // Check BIOS and BMC attributes
            let mut diffs = self.diff_bios_bmc_attr().await?;

            // Check the first boot option
            if let Some(mac) = boot_interface_mac {
                if let Some(diff) = self.boot_order_diff(mac).await? {
                    diffs.push(diff);
                }
            }

            // Check lockdown status
            let lockdown = self.lockdown_status().await?;
            if !lockdown.is_fully_enabled() {
                diffs.push(MachineSetupDiff {
                    key: "lockdown".to_string(),
                    expected: "Enabled".to_string(),
                    actual: lockdown.status.to_string(),
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
            use serde_json::Value::Number;
            let body = HashMap::from([
                ("AccountLockoutThreshold", Number(0.into())),
                ("AccountLockoutDuration", Number(0.into())),
                ("AccountLockoutCounterResetAfter", Number(0.into())),
            ]);
            self.s
                .client
                .patch("AccountService", body)
                .await
                .map(|_status_code| ())
        })
    }

    fn lockdown<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            use EnabledDisabled::*;
            match target {
                Enabled => {
                    // ARS-121L-DNR can't PXE boot if the host interface is disabled.
                    if !self.is_ars_121l_dnr().await? {
                        self.set_host_interfaces(Disabled).await?;
                    }
                    self.set_kcs_privilege(supermicro::Privilege::Callback)
                        .await?;
                    self.set_syslockdown(Enabled).await?; // Lock last
                }
                Disabled => {
                    self.set_syslockdown(Disabled).await?; // Unlock first
                    self.set_kcs_privilege(supermicro::Privilege::Administrator)
                        .await?;
                    self.set_host_interfaces(Enabled).await?;
                }
            }
            Ok(())
        })
    }

    fn lockdown_status<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            let is_hi_on = self.is_host_interface_enabled().await?;
            let kcs_privilege = self.get_kcs_privilege().await?;

            let is_syslockdown = self.get_syslockdown().await?;
            let message = format!("SysLockdownEnabled={is_syslockdown}, kcs_privilege={kcs_privilege:#?}, host_interface_enabled={is_hi_on}");

            let must_keep_host_interface_enabled = self.is_ars_121l_dnr().await?;
            let kcs_locked =
                kcs_privilege.is_none() || kcs_privilege == Some(supermicro::Privilege::Callback);
            let kcs_unlocked = kcs_privilege.is_none()
                || kcs_privilege == Some(supermicro::Privilege::Administrator);

            let is_locked =
                is_syslockdown && kcs_locked && (must_keep_host_interface_enabled || !is_hi_on);
            let is_unlocked = !is_syslockdown && kcs_unlocked && is_hi_on;
            Ok(Status {
                message,
                status: if is_locked {
                    StatusInternal::Enabled
                } else if is_unlocked {
                    StatusInternal::Disabled
                } else {
                    StatusInternal::Partial
                },
            })
        })
    }

    /// On Supermicro this does nothing. Serial Console is on by default and can't be disabled
    /// or enabled via redfish. The properties under Systems/1, key SerialConsole are read only.
    fn setup_serial_console<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn serial_console_status<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            let s_interface = self.s.get_serial_interface().await?;
            let system = self.s.get_system().await?;
            let Some(sr) = &system.serial_console else {
                return Err(RedfishError::NotSupported(
                "No SerialConsole in System object. Maybe it's in Manager and you have old firmware?".to_string(),
            ));
            };
            let is_enabled = sr.ssh.service_enabled
                && sr.max_concurrent_sessions != Some(0)
                && s_interface.is_supermicro_default();
            let status = if is_enabled {
                StatusInternal::Enabled
            } else {
                StatusInternal::Disabled
            };
            Ok(Status {
                message: String::new(),
                status,
            })
        })
    }

    /// Boot from this device once then go back to the normal boot order
    fn boot_once<'a>(&'a self, target: Boot) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Redfish::set_boot_override(
                self,
                BootOverride {
                    target: target.into(),
                    enabled: BootSourceOverrideEnabled::Once,
                    mode: None,
                    http_boot_uri: None,
                },
            )
            .await?;
            Ok(())
        })
    }

    /// Set which device we should boot from first.
    fn boot_first<'a>(
        &'a self,
        target: Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // Try with FixedBootOptions and fallback to BootOptions if fails
            match self.set_boot_order(target).await {
                Err(RedfishError::HTTPErrorCode {
                    status_code: StatusCode::NOT_FOUND,
                    ..
                }) => {
                    Redfish::set_boot_override(
                        self,
                        BootOverride {
                            target: target.into(),
                            enabled: BootSourceOverrideEnabled::Continuous,
                            mode: None,
                            http_boot_uri: None,
                        },
                    )
                    .await?;
                    Ok(())
                }
                res => res,
            }
        })
    }

    fn set_boot_override<'a>(
        &'a self,
        settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            // Supermicro BMCs default to UEFI mode when the caller doesn't specify one.
            let mode = Some(settings.mode.unwrap_or(BootSourceOverrideMode::UEFI));
            let boot = boot::Boot {
                boot_source_override_target: Some(settings.target),
                boot_source_override_enabled: Some(settings.enabled),
                boot_source_override_mode: mode,
                http_boot_uri: settings.http_boot_uri,
                ..Default::default()
            };
            let body = HashMap::from([("Boot", boot)]);
            let url = format!("Systems/{}", self.s.system_id());
            self.s
                .client
                .patch(&url, body)
                .await
                .map(|_status_code| ())?;
            Ok(None)
        })
    }

    /// Supermicro BMC does not appear to have this.
    /// TODO: Verify that this really clear the TPM.
    fn clear_tpm<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let bios_attrs = self.s.bios_attributes().await?;
            let Some(attrs_map) = bios_attrs.as_object() else {
                return Err(RedfishError::InvalidKeyType {
                    key: "Attributes".to_string(),
                    expected_type: "Map".to_string(),
                    url: String::new(),
                });
            };

            // Yes the BIOS attribute to clear the TPM is called "PendingOperation<something>"
            let Some(name) = attrs_map.keys().find(|k| k.starts_with("PendingOperation")) else {
                return Err(RedfishError::NotSupported(
                    "Cannot clear_tpm, PendingOperation BIOS attr missing".to_string(),
                ));
            };

            let body = HashMap::from([("Attributes", HashMap::from([(name, "TPM Clear")]))]);
            let url = format!("Systems/{}/Bios", self.s.system_id());
            self.s.client.patch(&url, body).await.map(|_status_code| ())
        })
    }

    fn pending<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/SD", self.s.system_id());
            // Supermicro doesn't include the Attributes key if there are no pending changes
            self.s
                .pending_attributes(&url)
                .await
                .map(|m| {
                    m.into_iter()
                        .collect::<HashMap<String, serde_json::Value>>()
                })
                .or_else(|err| match err {
                    RedfishError::MissingKey { .. } => Ok(HashMap::new()),
                    err => Err(err),
                })
        })
    }

    // TODO: This resets the pending Bios changes to their default values,
    // but DOES NOT CLEAR THEM. We don't know how to do that, or if Supermicro supports it at all.
    fn clear_pending<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/SD", self.s.system_id());
            self.s.clear_pending_with_url(&url).await
        })
    }

    fn pcie_devices<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<PCIeDevice>, RedfishError>> {
        Box::pin(async move {
            let Some(chassis_id) = self.get_chassis_all().await?.into_iter().next() else {
                return Err(RedfishError::NoContent);
            };
            let url = format!("Chassis/{chassis_id}/PCIeDevices");
            let device_ids = self.s.get_members(&url).await?;
            let mut out = Vec::with_capacity(device_ids.len());
            for device_id in device_ids {
                out.push(self.get_pcie_device(&chassis_id, &device_id).await?);
            }
            Ok(out)
        })
    }

    fn update_firmware_multipart<'a>(
        &'a self,
        filename: &'a Path,
        _reboot: bool,
        timeout: Duration,
        component_type: ComponentType,
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

            let parameters = serde_json::to_string(&UpdateParameters::new(component_type))
                .map_err(|e| RedfishError::JsonSerializeError {
                    url: "".to_string(),
                    object_debug: "".to_string(),
                    source: e,
                })?;
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
                .await?;

            let task: Task =
                serde_json::from_str(&body).map_err(|e| RedfishError::JsonDeserializeError {
                    url: update_service.multipart_http_push_uri,
                    body,
                    source: e,
                })?;

            Ok(task.id)
        })
    }

    fn change_boot_order<'a>(
        &'a self,
        boot_array: Vec<String>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let body = HashMap::from([("Boot", HashMap::from([("BootOrder", boot_array)]))]);
            let url = format!("Systems/{}", self.s.system_id());
            self.s.client.patch(&url, body).await.map(|_status_code| ())
        })
    }

    fn bmc_reset_to_defaults<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // Supermicro uses an OEM action whose `Option` parameter selects one
            // of three resets: `PreserveUser` keeps existing accounts,
            // `ResetToADMIN` restores the legacy ADMIN/ADMIN credentials, and
            // `ClearConfig` wipes the config and restores the unique factory
            // password printed on the chassis label.
            let url = format!(
                "Managers/{}/Actions/Oem/SmcManagerConfig.Reset",
                self.s.manager_id()
            );
            let mut arg = HashMap::new();
            arg.insert("Option", "ClearConfig".to_string());
            self.s.client.post(&url, arg).await.map(|_resp| ())
        })
    }

    /// Set the DPU to be our first netboot device.
    /// The HTTP adapter will only appear after IPv4HTTPSupport bios setting is enabled and the host rebooted.
    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mac_address = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            let mac_address = mac_address.as_str();
            match self
                .try_set_standard_boot_order_dpu_first(mac_address)
                .await
            {
                Ok(true) => return Ok(None),
                Ok(false) => {}
                Err(RedfishError::HTTPErrorCode {
                    status_code,
                    response_body,
                    ..
                }) if status_code == reqwest::StatusCode::BAD_REQUEST
                    && response_body.contains("PropertyUnknown")
                    && response_body.contains("BootOrder") =>
                {
                    // Fall back to the following method if we get this error:
                    // HTTP 400 - "The property BootOrder is not in the list of valid properties for the resource"
                }
                Err(e) => return Err(e),
            }

            // Some Supermicro models only expose the OEM fixed boot order.
            let mut fbo = self.get_boot_order().await?;

            // The network name is not consistent because it includes the interface name.
            // Falls back to 'UEFI Network' if no specific entry is found to enable network boot options.
            let network = fbo
                .fixed_boot_order
                .iter()
                .find(|entry| entry.starts_with(NETWORK))
                .map(|s| s.as_str())
                .unwrap_or(NETWORK);

            // The hard disk name is also not consistent because it includes the device specifics.
            // Falls back to 'UEFI Hard Disk' if no specific entry is found to enable hard disk boot options.
            let hard_disk = fbo
                .fixed_boot_order
                .iter()
                .find(|entry| entry.starts_with(HARD_DISK))
                .map(|s| s.as_str())
                .unwrap_or(HARD_DISK);

            // Make network the first option, hard disk second, and everything else disabled
            let mut order = ["Disabled"].repeat(fbo.fixed_boot_order.len());
            order[0] = network;
            order[1] = hard_disk;

            // Set the DPU to be the first network device to boot from
            let Some(pos) = fbo
                .uefi_network
                .iter()
                .position(|entry| http_boot_option_matches_mac(entry, mac_address))
            else {
                return Err(RedfishError::NotSupported(
                format!("No match for Mellanox/Nvidia HTTP adapter with MAC address {} in network boot order", mac_address)
            ));
            };
            fbo.uefi_network.swap(0, pos);

            let url = format!(
                "Systems/{}/Oem/Supermicro/FixedBootOrder",
                self.s.system_id()
            );
            let body = HashMap::from([
                ("FixedBootOrder", order),
                (
                    "UEFINetwork",
                    fbo.uefi_network.iter().map(|s| s.as_ref()).collect(),
                ),
            ]);
            self.s
                .client
                .patch(&url, body)
                .await
                .map(|_status_code| ())?;
            Ok(None)
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn lockdown_bmc<'a>(
        &'a self,
        target: crate::EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.set_syslockdown(target).await })
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let mac = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            Ok(self.boot_order_diff(&mac).await?.is_none())
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let diffs = self.diff_bios_bmc_attr().await?;
            Ok(diffs.is_empty())
        })
    }
}

impl Bmc {
    /// Check BIOS and BMC attributes and return differences
    async fn diff_bios_bmc_attr(&self) -> Result<Vec<MachineSetupDiff>, RedfishError> {
        let mut diffs = vec![];

        let sc = self.serial_console_status().await?;
        if !sc.is_fully_enabled() {
            diffs.push(MachineSetupDiff {
                key: "serial_console".to_string(),
                expected: "Enabled".to_string(),
                actual: sc.status.to_string(),
            });
        }

        let bios = self.s.bios_attributes().await?;
        let expected_attrs = self.machine_setup_attrs().await?;
        for (key, expected) in expected_attrs {
            let Some(actual) = bios.get(&key) else {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    expected: expected.to_string(),
                    actual: "_missing_".to_string(),
                });
                continue;
            };
            // expected and actual are serde_json::Value which are not comparable, so to_string
            let act = actual.to_string();
            let exp = expected.to_string();
            if act != exp {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    expected: exp,
                    actual: act,
                });
            }
        }

        Ok(diffs)
    }

    async fn boot_order_diff(
        &self,
        boot_interface_mac: &str,
    ) -> Result<Option<MachineSetupDiff>, RedfishError> {
        // Try using standard BootOptions first
        match self.s.get_boot_options().await {
            Ok(all) => {
                let system = self.s.get_system().await?;
                if system.boot.boot_order.is_empty() {
                    return self.load_fixed_boot_order_diff(boot_interface_mac).await;
                }

                let mut boot_options = Vec::with_capacity(all.members.len());
                for member in &all.members {
                    let id = member.odata_id_get()?;
                    boot_options.push(self.s.get_boot_option(id).await?);
                }

                Ok(standard_boot_order_diff(
                    &system.boot.boot_order,
                    &boot_options,
                    boot_interface_mac,
                ))
            }
            Err(RedfishError::HTTPErrorCode {
                status_code,
                response_body,
                ..
            }) if status_code == reqwest::StatusCode::BAD_REQUEST
                && response_body.contains("PropertyUnknown")
                && response_body.contains("BootOrder") =>
            {
                // Fall back to FixedBootOrder for platforms that don't support standard BootOptions
                self.load_fixed_boot_order_diff(boot_interface_mac).await
            }
            Err(e) => Err(e),
        }
    }

    async fn load_fixed_boot_order_diff(
        &self,
        boot_interface_mac: &str,
    ) -> Result<Option<MachineSetupDiff>, RedfishError> {
        let fbo = self.get_boot_order().await?;
        Ok(fixed_boot_order_diff(
            &fbo.fixed_boot_order,
            &fbo.uefi_network,
            boot_interface_mac,
        ))
    }

    async fn machine_setup_attrs(&self) -> Result<Vec<(String, serde_json::Value)>, RedfishError> {
        let mut bios_keys = self.bios_attributes_name_map().await?;
        let mut bios_attrs: Vec<(String, serde_json::Value)> = vec![];

        macro_rules! add_keys {
            ($name:literal, $value:expr) => {
                for real_key in bios_keys.remove($name).unwrap_or_default() {
                    bios_attrs.push((real_key, $value.into()));
                }
            };
        }
        add_keys!("QuietBoot", false);
        add_keys!("Re-tryBoot", "EFI Boot");
        add_keys!("CSMSupport", "Disabled");
        add_keys!("SecureBootEnable", false);

        // Trusted Computing / Provision Support / TXT Support
        add_keys!("TXTSupport", EnabledDisabled::Enabled);

        // registries/BiosAttributeRegistry.1.0.0.json/index.json
        add_keys!("DeviceSelect", "TPM 2.0");

        // Attributes to enable CPU virtualization support for faster VMs
        // Not that some are "Enable" and some are "Enabled". Subtle.
        add_keys!("IntelVTforDirectedI/O(VT-d)", EnableDisable::Enable);
        add_keys!("IntelVirtualizationTechnology", EnableDisable::Enable);
        add_keys!("SR-IOVSupport", EnabledDisabled::Enabled);
        add_keys!("SR_IOVSupport", EnabledDisabled::Enabled);

        // UEFI NIC boot
        add_keys!("IPv4HTTPSupport", EnabledDisabled::Enabled);
        add_keys!("IPv4PXESupport", EnabledDisabled::Disabled);
        add_keys!("IPv6HTTPSupport", EnabledDisabled::Disabled);
        add_keys!("IPv6PXESupport", EnabledDisabled::Disabled);

        // Enable TPM - check current format and use matching enum
        let current_attrs = self.s.bios_attributes().await?;
        let tpm_value = current_attrs
            .as_object()
            .and_then(|attrs| {
                attrs.iter().find(|(key, _)| {
                    key.split('_')
                        .next()
                        .unwrap_or(key)
                        .starts_with("SecurityDeviceSupport")
                })
            })
            .and_then(|(_, value)| value.as_str());

        if let Some(val) = tpm_value {
            if val == EnabledDisabled::Enabled.to_string()
                || val == EnabledDisabled::Disabled.to_string()
            {
                add_keys!("SecurityDeviceSupport", EnabledDisabled::Enabled)
            } else if val == EnableDisable::Enable.to_string()
                || val == EnableDisable::Disable.to_string()
            {
                add_keys!("SecurityDeviceSupport", EnableDisable::Enable)
            } else {
                return Err(RedfishError::GenericError {
                    error: "Unexpected SecurityDeviceSupport value".to_string(),
                });
            }
        } else {
            return Err(RedfishError::GenericError {
                error: "Missing SecurityDeviceSupport value".to_string(),
            });
        }

        Ok(bios_attrs)
    }

    async fn get_kcs_privilege(&self) -> Result<Option<supermicro::Privilege>, RedfishError> {
        if self.is_mgx_c2().await? {
            if !self.bmc_supports_ipmi_host_iface().await? {
                return Ok(None);
            }
            let enabled = self.get_ipmi_host_interface_enabled().await?;
            return if enabled {
                Ok(Some(supermicro::Privilege::Administrator))
            } else {
                Ok(Some(supermicro::Privilege::Callback))
            };
        }

        let url = format!(
            "Managers/{}/Oem/Supermicro/KCSInterface",
            self.s.manager_id()
        );
        let (_, body) = self
            .s
            .client
            .get::<HashMap<String, serde_json::Value>>(&url)
            .await?;
        let key = "Privilege";
        let p_str = body
            .get(key)
            .ok_or_else(|| RedfishError::MissingKey {
                key: key.to_string(),
                url: url.to_string(),
            })?
            .as_str()
            .ok_or_else(|| RedfishError::InvalidKeyType {
                key: key.to_string(),
                expected_type: "&str".to_string(),
                url: url.to_string(),
            })?;
        p_str
            .parse()
            .map(Some)
            .map_err(|_| RedfishError::InvalidKeyType {
                key: key.to_string(),
                expected_type: "oem::supermicro::Privilege".to_string(),
                url: url.to_string(),
            })
    }

    async fn set_kcs_privilege(
        &self,
        privilege: supermicro::Privilege,
    ) -> Result<(), RedfishError> {
        if self.is_mgx_c2().await? {
            if !self.bmc_supports_ipmi_host_iface().await? {
                tracing::warn!(
                    smc_bmc_ip = self.s.client.host(),
                    "MGX C2 BMC firmware is older than {MIN_BMC_FW_IPMI_HOST_IFACE}; \
                     skipping IPMIHostInterface write"
                );
                return Ok(());
            }
            let enabled = privilege == supermicro::Privilege::Administrator;
            return self.set_ipmi_host_interface(enabled).await;
        }

        let url = format!(
            "Managers/{}/Oem/Supermicro/KCSInterface",
            self.s.manager_id()
        );
        let body = HashMap::from([("Privilege", privilege.to_string())]);
        self.s.client.patch(&url, body).await?;
        Ok(())
    }

    /// Returns `true` when the BMC firmware version is at least
    /// [`MIN_BMC_FW_IPMI_HOST_IFACE`] (`01.05.01`), which is the first
    /// version to expose `IPMIHostInterface` on `Systems/{id}`.
    async fn bmc_supports_ipmi_host_iface(&self) -> Result<bool, RedfishError> {
        let manager = self.s.get_manager().await?;
        let fw = manager.firmware_version.unwrap_or_default();
        Ok(version_compare::compare(&fw, MIN_BMC_FW_IPMI_HOST_IFACE)
            .is_ok_and(|c| c != version_compare::Cmp::Lt))
    }

    /// Disable/enable SSIF in-band access via `IPMIHostInterface` on `Systems/{id}`.
    /// Used for MGX C2 systems that lack the KCSInterface endpoint.
    async fn set_ipmi_host_interface(&self, enabled: bool) -> Result<(), RedfishError> {
        use crate::model::system::IpmiHostInterface;
        let url = format!("Systems/{}", self.s.system_id());
        let body = HashMap::from([(
            "IPMIHostInterface",
            IpmiHostInterface {
                service_enabled: enabled,
            },
        )]);
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    /// Get whether SSIF in-band access is enabled via `IPMIHostInterface` on `Systems/{id}`.
    /// Used for MGX C2 systems that lack the KCSInterface endpoint.
    async fn get_ipmi_host_interface_enabled(&self) -> Result<bool, RedfishError> {
        let system = self.s.get_system().await?;
        let iface = system
            .ipmi_host_interface
            .ok_or_else(|| RedfishError::MissingKey {
                key: "IPMIHostInterface".to_string(),
                url: format!("Systems/{}", self.s.system_id()),
            })?;
        Ok(iface.service_enabled)
    }

    async fn is_host_interface_enabled(&self) -> Result<bool, RedfishError> {
        let url = format!("Managers/{}/HostInterfaces", self.s.manager_id());
        let host_interface_ids = self.s.get_members(&url).await?;
        let num_interfaces = host_interface_ids.len();
        if num_interfaces != 1 {
            return Err(RedfishError::InvalidValue {
                url,
                field: "Members".to_string(),
                err: InvalidValueError(format!(
                    "Expected a single host interface, found {num_interfaces}"
                )),
            });
        }

        let url = format!(
            "Managers/{}/HostInterfaces/{}",
            self.s.manager_id(),
            host_interface_ids[0]
        );
        let (_, body): (_, HashMap<String, serde_json::Value>) = self.s.client.get(&url).await?;
        let key = "InterfaceEnabled";
        body.get(key)
            .ok_or_else(|| RedfishError::MissingKey {
                key: key.to_string(),
                url: url.to_string(),
            })?
            .as_bool()
            .ok_or_else(|| RedfishError::InvalidKeyType {
                key: key.to_string(),
                expected_type: "bool".to_string(),
                url: url.to_string(),
            })
    }

    // The HostInterface allows remote BMC access
    async fn set_host_interfaces(&self, target: EnabledDisabled) -> Result<(), RedfishError> {
        let url = format!("Managers/{}/HostInterfaces", self.s.manager_id());
        // I have only seen exactly one, but you can't be too careful
        let host_iface_ids = self.s.get_members(&url).await?;
        for iface_id in host_iface_ids {
            self.set_host_interface(&iface_id, target).await?;
        }
        Ok(())
    }

    async fn set_host_interface(
        &self,
        iface_id: &str,
        target: EnabledDisabled,
    ) -> Result<(), RedfishError> {
        let url = format!("Managers/{}/HostInterfaces/{iface_id}", self.s.manager_id());
        let body = HashMap::from([("InterfaceEnabled", target == EnabledDisabled::Enabled)]);
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_syslockdown(&self) -> Result<bool, RedfishError> {
        let url = format!(
            "Managers/{}/Oem/Supermicro/SysLockdown",
            self.s.manager_id()
        );
        let (_, body): (_, HashMap<String, serde_json::Value>) = self.s.client.get(&url).await?;
        let key = "SysLockdownEnabled";
        body.get(key)
            .ok_or_else(|| RedfishError::MissingKey {
                key: key.to_string(),
                url: url.to_string(),
            })?
            .as_bool()
            .ok_or_else(|| RedfishError::InvalidKeyType {
                key: key.to_string(),
                expected_type: "bool".to_string(),
                url: url.to_string(),
            })
    }

    async fn set_syslockdown(&self, target: EnabledDisabled) -> Result<(), RedfishError> {
        let url = format!(
            "Managers/{}/Oem/Supermicro/SysLockdown",
            self.s.manager_id()
        );
        let body = HashMap::from([("SysLockdownEnabled", target.is_enabled())]);
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_boot_order(&self) -> Result<FixedBootOrder, RedfishError> {
        let url = format!(
            "Systems/{}/Oem/Supermicro/FixedBootOrder",
            self.s.system_id()
        );
        let (_, fbo) = self.s.client.get(&url).await?;
        Ok(fbo)
    }

    async fn set_boot_order(&self, target: Boot) -> Result<(), RedfishError> {
        let mut fbo = self.get_boot_order().await?;

        // The network name is not consistent because it includes the interface name.
        // Falls back to 'UEFI Network' if no specific entry is found to enable network boot options.
        let network = fbo
            .fixed_boot_order
            .iter()
            .find(|entry| entry.starts_with(NETWORK))
            .map(|s| s.as_str())
            .unwrap_or(NETWORK);

        // Make our option the first option, the other one second, and everything else (CD/ROM,
        // USB, etc) disabled.
        let mut order = ["Disabled"].repeat(fbo.fixed_boot_order.len());
        match target {
            Boot::Pxe | Boot::UefiHttp => {
                order[0] = network;
                order[1] = HARD_DISK;
            }
            Boot::HardDisk => {
                order[0] = HARD_DISK;
                order[1] = network;
            }
        }

        // Set the DPU to be the first network device to boot from, for faster boots
        if target != Boot::HardDisk {
            let Some(pos) = fbo
                .uefi_network
                .iter()
                .position(|entry| is_supported_uefi_http_boot_option(entry))
            else {
                return Err(RedfishError::NotSupported(
                    "No match for a Mellanox/NVIDIA UEFI HTTP IPv4 adapter in network boot order"
                        .to_string(),
                ));
            };
            fbo.uefi_network.swap(0, pos);
        };

        let url = format!(
            "Systems/{}/Oem/Supermicro/FixedBootOrder",
            self.s.system_id()
        );
        let body = HashMap::from([
            ("FixedBootOrder", order),
            (
                "UEFINetwork",
                fbo.uefi_network.iter().map(|s| s.as_ref()).collect(),
            ),
        ]);
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_pcie_device(
        &self,
        chassis_id: &str,
        device_id: &str,
    ) -> Result<PCIeDevice, RedfishError> {
        let url = format!("Chassis/{chassis_id}/PCIeDevices/{device_id}");
        let (_, body): (_, PCIeDevice) = self.s.client.get(&url).await?;
        Ok(body)
    }

    /// Try to select the DPU through the standard Redfish boot order.
    ///
    /// An empty standard boot order selects the OEM fixed-order fallback.
    async fn try_set_standard_boot_order_dpu_first(
        &self,
        boot_interface: &str,
    ) -> Result<bool, RedfishError> {
        let system = self.s.get_system().await?;
        let Some(mut boot_order) = usable_standard_boot_order(system.boot.boot_order) else {
            return Ok(false);
        };

        let all = self.s.get_boot_options().await?;
        let mut target_reference = None;
        for b in all.members {
            let id = b.odata_id_get()?;
            let boot_option = self.s.get_boot_option(id).await?;

            if http_boot_option_matches_mac(&boot_option.display_name, boot_interface) {
                // Here are the patterns we have seen so far:
                // UEFI HTTP IPv4 Mellanox Network Adapter - A0:88:C2:EA:84:D0(MAC:A088C2EA84D0)
                // UEFI HTTP IPv4 Nvidia Network Adapter - C4:70:BD:F0:40:AA - C470BDF040AA"
                target_reference = Some(boot_option.boot_option_reference);
                break;
            }
        }
        let Some(target_reference) = target_reference else {
            // This happens if IPv4HTTPSupport#00F7 is disabled in the bios
            return Err(RedfishError::NotSupported(
                "No match for Mellanox HTTP adapter boot".to_string(),
            ));
        };

        if boot_order
            .first()
            .is_some_and(|entry| boot::boot_order_entry_reference(entry) == target_reference)
        {
            return Ok(true);
        }
        if !boot::promote_boot_order_entry_first(&mut boot_order, &target_reference) {
            boot_order.insert(0, target_reference);
        }
        self.change_boot_order(boot_order).await?;
        Ok(true)
    }

    // BIOS attribute names by their canonical name.
    // e.g. QuietBoot -> [QuietBoot_002E]
    //      TXTSupport -> [TXTSupport_0062, TXTSupport_0072]
    async fn bios_attributes_name_map(&self) -> Result<HashMap<String, Vec<String>>, RedfishError> {
        let bios_attrs = self.s.bios_attributes().await?;

        let Some(attrs_map) = bios_attrs.as_object() else {
            return Err(RedfishError::InvalidKeyType {
                key: "Attributes".to_string(),
                expected_type: "Map".to_string(),
                url: String::new(),
            });
        };
        let mut by_name: HashMap<String, Vec<String>> = HashMap::with_capacity(attrs_map.len());
        for k in attrs_map.keys() {
            let clean_key = k
                .rsplit_once('_')
                .map_or(k.as_str(), |(name, _suffix)| name.trim_end_matches('_'))
                .to_string();
            by_name
                .entry(clean_key)
                .and_modify(|e| e.push(k.clone()))
                .or_insert(vec![k.clone()]);
        }
        Ok(by_name)
    }

    /// MGX C2 systems use SSIF instead of x86 KCS, so the KCSInterface
    /// endpoint doesn't exist. Detect them by the NVIDIA PG535
    /// processor-module identity.
    /// See: https://docs.nvidia.com/dccpu/grace-perf-tuning-guide/system.html
    async fn is_mgx_c2(&self) -> Result<bool, RedfishError> {
        let chassis = self
            .s
            .get_collection(ODataId {
                odata_id: "/redfish/v1/Chassis".to_string(),
            })
            .await?
            .try_get::<Chassis>()?;

        Ok(chassis.members.iter().any(is_mgx_c2_processor_module))
    }

    async fn is_ars_121l_dnr(&self) -> Result<bool, RedfishError> {
        Ok(self
            .s
            .get_system()
            .await?
            .model
            .unwrap_or_default()
            .contains("ARS-121L-DNR"))
    }
}

fn is_mgx_c2_processor_module(chassis: &Chassis) -> bool {
    chassis
        .manufacturer
        .as_deref()
        .is_some_and(|manufacturer| manufacturer.eq_ignore_ascii_case("NVIDIA"))
        && (chassis
            .model
            .as_deref()
            .is_some_and(|model| model.starts_with(MGX_C2_PROCESSOR_MODULE_MODEL_PREFIX))
            || chassis.part_number.as_deref().is_some_and(|part_number| {
                part_number.contains(MGX_C2_PROCESSOR_MODULE_PART_NUMBER_FRAGMENT)
            }))
}

// UpdateParameters is what is sent for a multipart firmware upload's metadata.
#[allow(clippy::type_complexity)]
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct UpdateParameters {
    targets: Vec<String>,
    #[serde(rename = "@Redfish.OperationApplyTime")]
    pub apply_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    oem: Option<HashMap<String, HashMap<String, HashMap<String, bool>>>>,
}

impl UpdateParameters {
    pub fn new(component_type: ComponentType) -> UpdateParameters {
        let target = match component_type {
            ComponentType::UEFI => "/redfish/v1/Systems/1/Bios",
            ComponentType::BMC => "/redfish/v1/Managers/1",
            ComponentType::CPLDMB => "/redfish/v1/UpdateService/FirmwareInventory/CPLD_Motherboard",
            ComponentType::CPLDMID => {
                "/redfish/v1/UpdateService/FirmwareInventory/CPLD_Backplane_1"
            }
            _ => "Unrecognized component type",
        }
        .to_string();

        let oem = match component_type {
            ComponentType::UEFI => Some(HashMap::from([(
                "Supermicro".to_string(),
                HashMap::from([(
                    "BIOS".to_string(),
                    HashMap::from([
                        ("PreserveME".to_string(), true),
                        ("PreserveNVRAM".to_string(), true),
                        ("PreserveSMBIOS".to_string(), true),
                        ("BackupBIOS".to_string(), false),
                    ]),
                )]),
            )])),
            ComponentType::BMC => Some(HashMap::from([(
                "Supermicro".to_string(),
                HashMap::from([(
                    "BMC".to_string(),
                    HashMap::from([
                        ("PreserveCfg".to_string(), true),
                        ("PreserveSdr".to_string(), true),
                        ("PreserveSsl".to_string(), true),
                        ("BackupBMC".to_string(), true),
                    ]),
                )]),
            )])),
            _ => None,
        };
        UpdateParameters {
            targets: vec![target],
            apply_time: "Immediate".to_string(),
            oem,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot_option(reference: &str, display_name: &str) -> BootOption {
        BootOption {
            odata: Default::default(),
            alias: None,
            description: None,
            boot_option_enabled: Some(true),
            boot_option_reference: reference.to_string(),
            display_name: display_name.to_string(),
            id: reference.to_string(),
            name: display_name.to_string(),
            uefi_device_path: None,
        }
    }

    #[test]
    fn standard_boot_verification_uses_system_boot_order_not_collection_order() {
        let target = format!("{NVIDIA_UEFI_HTTP_IPV4} - B8:E9:24:17:6D:72 (MAC:B8E924176D72)");
        let disk = "UEFI Hard Disk";
        let options = vec![
            boot_option("Boot0001", &target),
            boot_option("Boot0002", disk),
        ];

        let diff = standard_boot_order_diff(
            &["Boot0002".to_string(), "Boot0001".to_string()],
            &options,
            "b8:e9:24:17:6d:72",
        )
        .expect("disk is first");

        assert_eq!(diff.expected, target);
        assert_eq!(diff.actual, disk);
    }

    #[test]
    fn supported_uefi_http_boot_options_include_mellanox_and_nvidia_names() {
        for display_name in [MELLANOX_UEFI_HTTP_IPV4, NVIDIA_UEFI_HTTP_IPV4] {
            assert!(is_supported_uefi_http_boot_option(display_name));
        }
        assert!(!is_supported_uefi_http_boot_option(
            "UEFI HTTP IPv4 Intel Network Adapter"
        ));
    }

    #[test]
    fn standard_boot_verification_resolves_decorated_boot_order_reference() {
        let target = format!("{MELLANOX_UEFI_HTTP_IPV4} - B8:E9:24:17:6D:72 (MAC:B8E924176D72)");
        let options = vec![
            boot_option("Boot0002", "UEFI Hard Disk"),
            boot_option("Boot0001", &target),
        ];

        let diff = standard_boot_order_diff(
            &["Boot0001: Selected network adapter".to_string()],
            &options,
            "B8:E9:24:17:6D:72",
        );

        assert!(diff.is_none());
    }

    #[test]
    fn standard_boot_verification_compares_references_not_display_names() {
        let target = format!("{MELLANOX_UEFI_HTTP_IPV4} - B8:E9:24:17:6D:72 (MAC:B8E924176D72)");
        let options = vec![
            boot_option("Boot0001", &target),
            boot_option("Boot0002", &target),
        ];

        let diff = standard_boot_order_diff(
            &["Boot0002".to_string(), "Boot0001".to_string()],
            &options,
            "B8:E9:24:17:6D:72",
        );

        assert!(diff.is_some());
    }

    #[test]
    fn fixed_boot_verification_resolves_network_category_and_prefixed_entry() {
        let target = format!("{NVIDIA_UEFI_HTTP_IPV4} - B8:E9:24:17:6D:72 (MAC:B8E924176D72)");
        let uefi_network = vec![target.clone()];

        for fixed_order in [
            vec![NETWORK.to_string()],
            vec![format!("{NETWORK}:{target}")],
        ] {
            let diff = fixed_boot_order_diff(&fixed_order, &uefi_network, "b8:e9:24:17:6d:72");
            assert!(diff.is_none(), "unexpected diff for {fixed_order:?}");
        }
    }

    #[test]
    fn empty_standard_boot_order_is_not_usable() {
        assert!(usable_standard_boot_order(Vec::new()).is_none());
    }

    #[test]
    fn fixed_boot_verification_uses_selected_network_device_not_category_suffix() {
        let target = format!("{NVIDIA_UEFI_HTTP_IPV4} - B8:E9:24:17:6D:72 (MAC:B8E924176D72)");
        let other = format!("{NVIDIA_UEFI_HTTP_IPV4} - B8:E9:24:17:6D:73 (MAC:B8E924176D73)");

        let diff = fixed_boot_order_diff(
            &[format!("{NETWORK}:{other}")],
            &[target.clone(), other.clone()],
            "b8:e9:24:17:6d:72",
        );
        assert!(diff.is_none());

        let diff = fixed_boot_order_diff(
            &[format!("{NETWORK}:{target}")],
            &[other.clone(), target],
            "b8:e9:24:17:6d:72",
        )
        .expect("a different network device is selected");
        assert_eq!(diff.actual, other);
    }

    #[test]
    fn identifies_mgx_c2_processor_modules() {
        let cases = [
            ("PG535 model", Some("NVIDIA"), Some("PG535-B02"), None, true),
            (
                "2G535 part number",
                Some("Nvidia"),
                None,
                Some("699-2G535-0200-310"),
                true,
            ),
            (
                "unrelated NVIDIA module",
                Some("NVIDIA"),
                Some("B4240"),
                Some("900-9D3B4-00CC-EA0"),
                false,
            ),
            (
                "non-NVIDIA PG535",
                Some("Supermicro"),
                Some("PG535-B02"),
                Some("699-2G535-0200-310"),
                false,
            ),
        ];

        for (case, manufacturer, model, part_number, expected) in cases {
            let chassis = Chassis {
                manufacturer: manufacturer.map(str::to_string),
                model: model.map(str::to_string),
                part_number: part_number.map(str::to_string),
                ..Default::default()
            };
            assert_eq!(is_mgx_c2_processor_module(&chassis), expected, "{case}");
        }
    }
}
