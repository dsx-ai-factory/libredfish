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

use chrono::Utc;
use regex::Regex;
use reqwest::header::HeaderMap;
use reqwest::Method;
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::Value;
use tokio::fs::File;
use tokio::time::sleep;
use tracing::debug;

use crate::model::account_service::ManagerAccount;
use crate::model::boot::BootOverride;
use crate::model::certificate::Certificate;
use crate::model::component_integrity::ComponentIntegrities;
use crate::model::oem::lenovo::{BootSettings, FrontPanelUSB, LenovoBootOrder};
use crate::model::oem::nvidia_dpu::{HostPrivilegeLevel, NicMode};
use crate::model::sel::LogService;
use crate::model::service_root::{RedfishVendor, ServiceRoot};
use crate::model::task::Task;
use crate::model::update_service::{ComponentType, TransferProtocolType, UpdateService};
use crate::model::{secure_boot::SecureBoot, ComputerSystem};
use crate::model::{InvalidValueError, Manager};
use crate::{
    jsonmap,
    model::{
        chassis::{Assembly, Chassis, NetworkAdapter},
        network_device_function::NetworkDeviceFunction,
        oem::lenovo,
        power::Power,
        sel::{LogEntry, LogEntryCollection},
        sensor::GPUSensors,
        software_inventory::SoftwareInventory,
        storage::Drives,
        thermal::Thermal,
        BootOption,
    },
    network::REDFISH_ENDPOINT,
    standard::RedfishStandard,
    BiosProfileType, Boot, BootOptions, Collection, EnabledDisabled, MachineSetupDiff,
    MachineSetupStatus, ODataId, PCIeDevice, PowerState, Redfish, RedfishError, Resource, Status,
    StatusInternal, SystemPowerControl,
};
use crate::{JobState, RoleId};

const UEFI_PASSWORD_NAME: &str = "UefiAdminPassword";
const GENERAL_BOOT_ORDER_HARD_DISK: &str = "Hard Disk";

pub struct Bmc {
    s: RedfishStandard,
}

impl Bmc {
    pub fn new(s: RedfishStandard) -> Result<Bmc, RedfishError> {
        Ok(Bmc { s })
    }
}
impl Redfish for Bmc {
    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        role_id: RoleId,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.create_user(username, password, role_id).await })
    }

    fn delete_user<'a>(
        &'a self,
        username: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.delete_user(username).await })
    }

    fn change_username<'a>(
        &'a self,
        old_name: &'a str,
        new_name: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.change_username(old_name, new_name).await })
    }

    fn change_password<'a>(
        &'a self,
        user: &'a str,
        new: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.change_password(user, new).await })
    }

    fn change_password_by_id<'a>(
        &'a self,
        account_id: &'a str,
        new_pass: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.change_password_by_id(account_id, new_pass).await })
    }

    fn get_accounts<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<ManagerAccount>, RedfishError>> {
        Box::pin(async move { self.s.get_accounts().await })
    }

    fn get_power_state<'a>(&'a self) -> crate::RedfishFuture<'a, Result<PowerState, RedfishError>> {
        Box::pin(async move { self.s.get_power_state().await })
    }

    fn get_power_metrics<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Power, RedfishError>> {
        Box::pin(async move { self.s.get_power_metrics().await })
    }

    fn power<'a>(
        &'a self,
        action: SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if action == SystemPowerControl::ACPowercycle {
                let args: HashMap<String, String> =
                    HashMap::from([("ResetType".to_string(), "ACPowerCycle".to_string())]);
                let url = format!(
                    "Systems/{}/Actions/Oem/LenovoComputerSystem.SystemReset",
                    self.s.system_id()
                );
                return self.s.client.post(&url, args).await.map(|_status_code| ());
            }

            if action == SystemPowerControl::ForceRestart
                && self.use_workaround_for_force_restart().await?
            {
                // We observed that issuing a ForceRestart to SR 675 V3 OVX machines can cause them to hang
                // We have observed that GracefulRestart is not a reliable mechanism to reboot hosts.
                // The most reliable workaround provided by Lenovo is to power off the machine, wait, and power on the machine
                self.s.power(SystemPowerControl::ForceOff).await?;
                sleep(Duration::from_secs(10)).await;
                if self.get_power_state().await? != PowerState::Off {
                    return Err(RedfishError::GenericError {
                        error: "Server did not turn off within 10 seconds after issuing a ForceOff"
                            .to_string(),
                    });
                }
                self.s.power(SystemPowerControl::On).await
            } else {
                self.s.power(action).await
            }
        })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        true
    }

    fn bmc_reset<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.bmc_reset().await })
    }

    fn chassis_reset<'a>(
        &'a self,
        chassis_id: &'a str,
        reset_type: SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.chassis_reset(chassis_id, reset_type).await })
    }

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Thermal, RedfishError>> {
        Box::pin(async move { self.s.get_thermal_metrics().await })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move { self.s.get_gpu_sensors().await })
    }

    fn get_system_event_log<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move { self.get_system_event_log().await })
    }

    fn get_bmc_event_log<'a>(
        &'a self,
        from: Option<chrono::DateTime<Utc>>,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Systems/{}/LogServices/AuditLog/Entries",
                self.s.system_id()
            );
            self.s.fetch_bmc_event_log(url, from).await
        })
    }

    fn get_drives_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<Drives>, RedfishError>> {
        Box::pin(async move { self.s.get_drives_metrics().await })
    }

    fn bios<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        Box::pin(async move { self.s.bios().await })
    }

    fn set_bios<'a>(
        &'a self,
        values: HashMap<String, serde_json::Value>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut body = HashMap::new();
            body.insert("Attributes", values);
            let url = format!("Systems/{}/Bios/Pending", self.s.system_id());
            self.s.client.patch(&url, body).await.map(|_status_code| ())
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/Actions/Bios.ResetBios", self.s.system_id());
            let mut arg = HashMap::new();
            arg.insert("ResetType", "Reset".to_string());
            self.s.client.post(&url, arg).await.map(|_resp| Ok(()))?
        })
    }

    fn machine_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
        bios_profiles: &'a HashMap<
            RedfishVendor,
            HashMap<String, HashMap<BiosProfileType, HashMap<String, serde_json::Value>>>,
        >,
        selected_profile: BiosProfileType,
        _oem_manager_profiles: &'a HashMap<
            RedfishVendor,
            HashMap<String, HashMap<BiosProfileType, HashMap<String, serde_json::Value>>>,
        >,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            self.setup_serial_console().await?;
            self.clear_tpm().await?;
            match self.boot_first(Boot::Pxe).await {
                Ok(()) => {}
                Err(RedfishError::MissingBootOption(_)) => {
                    tracing::info!(
                        "Network boot option not found, setting it via OEM general boot order"
                    );
                    match self.set_general_boot_order_network_first().await {
                        Ok(()) => {}
                        Err(RedfishError::HTTPErrorCode {
                            status_code: StatusCode::NOT_FOUND,
                            ..
                        }) => {
                            tracing::info!(
                                "OEM BootOrder.BootOrder not found, skipping (newer firmware)"
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
            self.set_virt_enable().await?;
            self.set_uefi_boot_only().await?;
            if let Some(lenovo) = bios_profiles.get(&RedfishVendor::Lenovo) {
                let model = crate::model_coerce(
                    self.get_system()
                        .await?
                        .model
                        .unwrap_or("".to_string())
                        .as_str(),
                );
                if let Some(all_extra_values) = lenovo.get(&model) {
                    if let Some(extra_values) = all_extra_values.get(&selected_profile) {
                        tracing::debug!("Setting extra BIOS values: {extra_values:?}");
                        self.set_bios(extra_values.clone()).await?;
                    }
                }
            }

            Ok(None)
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
            if let Some(mac) = boot_interface_mac {
                let (expected, actual) =
                    self.get_expected_and_actual_first_boot_option(mac).await?;
                if expected.is_none() || expected != actual {
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

    /// Redfish equivalent of `accseccfg -pew 0 -pe 0 -chgnew off -rc 0 -ci 0 -lf 0`
    fn set_machine_password_policy<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            use serde_json::Value;
            let mut body = HashMap::from([
                (
                    "AccountLockoutThreshold".to_string(),
                    Value::Number(0.into()),
                ), // -lf 0
                (
                    "AccountLockoutDuration".to_string(),
                    // 60 secs is the shortest Lenovo allows. The docs say 0 disables it, but my
                    // test Lenovo rejects 0.
                    Value::Number(60.into()),
                ),
            ]);
            let lenovo = Value::Object(serde_json::Map::from_iter(vec![
                (
                    "PasswordExpirationPeriodDays".to_string(),
                    Value::Number(0.into()),
                ), // -pe 0
                (
                    "PasswordChangeOnFirstAccess".to_string(),
                    Value::Bool(false),
                ), // -chgnew off
                (
                    "MinimumPasswordChangeIntervalHours".to_string(),
                    Value::Number(0.into()),
                ), // -ci 0
                (
                    "MinimumPasswordReuseCycle".to_string(),
                    Value::Number(0.into()),
                ), // -rc 0
                (
                    "PasswordExpirationWarningPeriod".to_string(),
                    Value::Number(0.into()),
                ), // -pew 0
            ]));
            let mut oem = serde_json::Map::new();
            oem.insert("Lenovo".to_string(), lenovo);
            body.insert("Oem".to_string(), serde_json::Value::Object(oem));

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
                Enabled => self.enable_lockdown().await,
                Disabled => self.disable_lockdown().await,
            }
        })
    }

    fn lockdown_status<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            let kcs = self.get_kcs_lenovo().await?;
            let firmware_rollback = self.get_firmware_rollback_lenovo().await?;
            let eth_usb = self.get_ethernet_over_usb().await?;
            let front_usb = self.get_front_panel_usb_lenovo().await?;

            let message = format!(
            "kcs={kcs}, firmware_rollback={firmware_rollback}, ethernet_over_usb={eth_usb}, front_panel_usb={}/{}",
            front_usb.fp_mode, front_usb.port_switching_to,
        );

            let is_locked = !kcs
                && !eth_usb
                && firmware_rollback == EnabledDisabled::Disabled
                && front_usb.fp_mode == lenovo::FrontPanelUSBMode::Server;

            let is_unlocked = kcs
                && eth_usb
                && firmware_rollback == EnabledDisabled::Enabled
                && front_usb.fp_mode == lenovo::FrontPanelUSBMode::Shared
                && front_usb.port_switching_to == lenovo::PortSwitchingMode::Server;

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

    fn setup_serial_console<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let bios = self.bios().await?;
            let url = format!("Systems/{}/Bios", self.s.system_id());
            let current_attrs = jsonmap::get_object(&bios, "Attributes", &url)?;

            let mut attributes = HashMap::new();

            attributes.insert(
                "DevicesandIOPorts_COMPort1",
                EnabledDisabled::Enabled.to_string(),
            );
            attributes.insert(
                "DevicesandIOPorts_ConsoleRedirection",
                "Enabled".to_string(),
            );
            attributes.insert(
                "DevicesandIOPorts_SerialPortSharing",
                EnabledDisabled::Enabled.to_string(),
            );
            attributes.insert(
                "DevicesandIOPorts_SerialPortAccessMode",
                "Shared".to_string(),
            );

            // Only in older Lenovo systems
            if current_attrs.contains_key("DevicesandIOPorts_SPRedirection") {
                attributes.insert(
                    "DevicesandIOPorts_SPRedirection",
                    EnabledDisabled::Enabled.to_string(),
                );
            }
            if current_attrs.contains_key("DevicesandIOPorts_COMPortActiveAfterBoot") {
                attributes.insert(
                    "DevicesandIOPorts_COMPortActiveAfterBoot",
                    EnabledDisabled::Enabled.to_string(),
                );
            }

            let mut body = HashMap::new();
            body.insert("Attributes", attributes);

            let url = format!("Systems/{}/Bios/Pending", self.s.system_id());
            self.s.client.patch(&url, body).await.map(|_status_code| ())
        })
    }

    fn serial_console_status<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios", self.s.system_id());
            let bios = self.bios().await?;
            let attrs = jsonmap::get_object(&bios, "Attributes", &url)?;

            // "any" means any value counts as correctly disabled
            // Attributes are checked if present, missing attributes are skipped
            let expected = vec![
                ("DevicesandIOPorts_COMPort1", "Enabled", "any"),
                ("DevicesandIOPorts_ConsoleRedirection", "Enabled", "Auto"),
                ("DevicesandIOPorts_SerialPortSharing", "Enabled", "Disabled"),
                ("DevicesandIOPorts_SPRedirection", "Enabled", "Disabled"),
                (
                    "DevicesandIOPorts_COMPortActiveAfterBoot",
                    "Enabled",
                    "Disabled",
                ),
                (
                    "DevicesandIOPorts_SerialPortAccessMode",
                    "Shared",
                    "Disabled",
                ),
            ];

            let mut message = String::new();
            let mut enabled = true;
            let mut disabled = true;

            for (key, val_enabled, val_disabled) in expected {
                if let Some(val_current) = attrs.get(key).and_then(|v| v.as_str()) {
                    message.push_str(&format!("{key}={val_current} "));
                    if val_current != val_enabled {
                        enabled = false;
                    }
                    if val_current != val_disabled && val_disabled != "any" {
                        disabled = false;
                    }
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
        })
    }

    fn get_boot_options<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<BootOptions, RedfishError>> {
        Box::pin(async move { self.s.get_boot_options().await })
    }

    fn get_boot_option<'a>(
        &'a self,
        option_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<BootOption, RedfishError>> {
        Box::pin(async move { self.s.get_boot_option(option_id).await })
    }

    fn boot_once<'a>(&'a self, target: Boot) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            match target {
                Boot::Pxe => self.set_boot_override(lenovo::BootSource::Pxe).await,
                Boot::HardDisk => self.set_boot_override(lenovo::BootSource::Hdd).await,
                Boot::UefiHttp => Err(RedfishError::NotSupported(
                    "No Lenovo UefiHttp implementation".to_string(),
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
                Boot::Pxe => self.set_boot_first(lenovo::BootOptionName::Network).await,
                Boot::HardDisk => self.set_boot_first(lenovo::BootOptionName::HardDisk).await,
                Boot::UefiHttp => Err(RedfishError::NotSupported(
                    "No Lenovo UefiHttp implementation".to_string(),
                )),
            }
        })
    }

    /// Not yet implemented on Lenovo XCC. Like Dell, Lenovo does not expose
    /// the standard Redfish `Boot.HttpBootUri` property; setting an HTTP boot
    /// URI on Lenovo requires a vendor-specific BIOS attribute path. Planned
    /// as a follow-up PR.
    fn set_boot_override<'a>(
        &'a self,
        _settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "No Lenovo set_boot_override implementation".to_string(),
            ))
        })
    }

    fn clear_tpm<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut body = HashMap::new();
            body.insert(
                "Attributes",
                HashMap::from([("TrustedComputingGroup_DeviceOperation", "Clear")]),
            );
            let url = format!("Systems/{}/Bios/Pending", self.s.system_id());
            self.s.client.patch(&url, body).await.map(|_status_code| ())
        })
    }

    fn pending<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/Pending", self.s.system_id());
            self.s.pending_with_url(&url).await
        })
    }

    fn clear_pending<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/Pending", self.s.system_id());
            self.s.clear_pending_with_url(&url).await
        })
    }

    fn pcie_devices<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<PCIeDevice>, RedfishError>> {
        Box::pin(async move { self.s.pcie_devices().await })
    }

    fn update_firmware<'a>(
        &'a self,
        firmware: tokio::fs::File,
    ) -> crate::RedfishFuture<'a, Result<crate::model::task::Task, RedfishError>> {
        Box::pin(async move { self.s.update_firmware(firmware).await })
    }

    fn get_update_service<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<UpdateService, RedfishError>> {
        Box::pin(async move { self.s.get_update_service().await })
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

            // The Python example code followed the schema to get the actual endpoint; this may or may not be needed, but
            // it's safest not to assume that it will always be the same thing.
            let update_service = self.get_update_service().await?;

            if update_service.multipart_http_push_uri.is_empty() {
                return Err(RedfishError::NotSupported(
                    "Host BMC does not support HTTP multipart push".to_string(),
                ));
            }

            let parameters = serde_json::to_string(&UpdateParameters::new()).map_err(|e| {
                RedfishError::JsonSerializeError {
                    url: "".to_string(),
                    object_debug: "".to_string(),
                    source: e,
                }
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

    fn get_tasks<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_tasks().await })
    }

    fn get_task<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::model::task::Task, RedfishError>> {
        Box::pin(async move { self.s.get_task(id).await })
    }

    fn get_firmware<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<SoftwareInventory, RedfishError>> {
        Box::pin(async move {
            let mut inv = self.s.get_firmware(id).await?;
            // Lenovo prepends the last two characters of their "Build/Vendor" ID and a dash to most of the versions.  This confuses things, so trim off anything that's before a dash.
            inv.version = inv
                .version
                .map(|x| x.split('-').next_back().unwrap_or("").to_string());
            Ok(inv)
        })
    }

    fn get_software_inventories<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_software_inventories().await })
    }

    fn get_system<'a>(&'a self) -> crate::RedfishFuture<'a, Result<ComputerSystem, RedfishError>> {
        Box::pin(async move { self.s.get_system().await })
    }

    fn get_secure_boot_certificate<'a>(
        &'a self,
        database_id: &'a str,
        certificate_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Certificate, RedfishError>> {
        Box::pin(async move {
            self.s
                .get_secure_boot_certificate(database_id, certificate_id)
                .await
        })
    }

    fn get_secure_boot_certificates<'a>(
        &'a self,
        database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_secure_boot_certificates(database_id).await })
    }

    fn add_secure_boot_certificate<'a>(
        &'a self,
        pem_cert: &'a str,
        database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            self.s
                .add_secure_boot_certificate(pem_cert, database_id)
                .await
        })
    }

    fn get_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<SecureBoot, RedfishError>> {
        Box::pin(async move { self.s.get_secure_boot().await })
    }

    fn enable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.enable_secure_boot().await })
    }

    fn disable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.disable_secure_boot().await })
    }

    fn get_network_device_function<'a>(
        &'a self,
        chassis_id: &'a str,
        id: &'a str,
        port: Option<&'a str>,
    ) -> crate::RedfishFuture<'a, Result<NetworkDeviceFunction, RedfishError>> {
        Box::pin(async move {
            self.s
                .get_network_device_function(chassis_id, id, port)
                .await
        })
    }

    fn get_network_device_functions<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_network_device_functions(chassis_id).await })
    }

    fn get_chassis_all<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_chassis_all().await })
    }

    fn get_chassis<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Chassis, RedfishError>> {
        Box::pin(async move { self.s.get_chassis(id).await })
    }

    fn get_chassis_assembly<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Assembly, RedfishError>> {
        Box::pin(async move { self.s.get_chassis_assembly(chassis_id).await })
    }

    fn get_chassis_network_adapters<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_chassis_network_adapters(chassis_id).await })
    }

    fn get_chassis_network_adapter<'a>(
        &'a self,
        chassis_id: &'a str,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        Box::pin(async move { self.s.get_chassis_network_adapter(chassis_id, id).await })
    }

    fn get_base_network_adapters<'a>(
        &'a self,
        system_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_base_network_adapters(system_id).await })
    }

    fn get_base_network_adapter<'a>(
        &'a self,
        system_id: &'a str,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        Box::pin(async move { self.s.get_base_network_adapter(system_id, id).await })
    }

    fn get_ports<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_ports(chassis_id, network_adapter).await })
    }

    fn get_port<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::NetworkPort, RedfishError>> {
        Box::pin(async move { self.s.get_port(chassis_id, network_adapter, id).await })
    }

    fn get_manager_ethernet_interfaces<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_manager_ethernet_interfaces().await })
    }

    fn get_manager_ethernet_interface<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::EthernetInterface, RedfishError>> {
        Box::pin(async move { self.s.get_manager_ethernet_interface(id).await })
    }

    fn get_system_ethernet_interfaces<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_system_ethernet_interfaces().await })
    }

    fn get_system_ethernet_interface<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::EthernetInterface, RedfishError>> {
        Box::pin(async move { self.s.get_system_ethernet_interface(id).await })
    }

    fn change_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
        new_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            self.s
                .change_bios_password(UEFI_PASSWORD_NAME, current_uefi_password, new_uefi_password)
                .await
        })
    }

    fn change_boot_order<'a>(
        &'a self,
        boot_array: Vec<String>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let body = HashMap::from([("Boot", HashMap::from([("BootOrder", boot_array)]))]);
            let url = format!("Systems/{}/Pending", self.s.system_id());
            // BMC takes longer to respond to this one, so override timeout
            let timeout = Duration::from_secs(10);
            let (_status_code, _resp_body, _resp_headers): (
                _,
                Option<HashMap<String, serde_json::Value>>,
                Option<HeaderMap>,
            ) = self
                .s
                .client
                .req(
                    Method::PATCH,
                    &url,
                    Some(body),
                    Some(timeout),
                    None,
                    Vec::new(),
                )
                .await?;
            Ok(())
        })
    }

    fn get_service_root<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ServiceRoot, RedfishError>> {
        Box::pin(async move { self.s.get_service_root().await })
    }

    fn get_systems<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_systems().await })
    }

    fn get_managers<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_managers().await })
    }

    fn get_manager<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Manager, RedfishError>> {
        Box::pin(async move { self.s.get_manager().await })
    }

    fn bmc_reset_to_defaults<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.bmc_reset_to_defaults().await })
    }

    fn get_job_state<'a>(
        &'a self,
        job_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<JobState, RedfishError>> {
        Box::pin(async move { self.s.get_job_state(job_id).await })
    }

    fn get_collection<'a>(
        &'a self,
        id: ODataId,
    ) -> crate::RedfishFuture<'a, Result<Collection, RedfishError>> {
        Box::pin(async move { self.s.get_collection(id).await })
    }

    fn get_resource<'a>(
        &'a self,
        id: ODataId,
    ) -> crate::RedfishFuture<'a, Result<Resource, RedfishError>> {
        Box::pin(async move { self.s.get_resource(id).await })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mac_address = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            let mac_address = mac_address.as_str();
            // Try the OEM NetworkBootOrder path first (older firmware)
            match self.set_boot_order_dpu_first_oem(mac_address).await {
                Ok(result) => return Ok(result),
                Err(RedfishError::HTTPErrorCode {
                    status_code: StatusCode::NOT_FOUND,
                    ..
                }) => {
                    // OEM path doesn't exist, fall back to BIOS attributes (newer firmware)
                    tracing::info!(
                        "OEM NetworkBootOrder not found, using BIOS attributes for boot order"
                    );
                }
                Err(e) => return Err(e),
            }

            self.set_boot_order_dpu_first_bios_attr(mac_address).await
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
        Box::pin(async move { self.s.get_base_mac_address().await })
    }

    fn lockdown_bmc<'a>(
        &'a self,
        target: crate::EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.lockdown_bmc(target).await })
    }

    fn is_ipmi_over_lan_enabled<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { self.s.is_ipmi_over_lan_enabled().await })
    }

    fn enable_ipmi_over_lan<'a>(
        &'a self,
        target: crate::EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.enable_ipmi_over_lan(target).await })
    }

    fn update_firmware_simple_update<'a>(
        &'a self,
        image_uri: &'a str,
        targets: Vec<String>,
        transfer_protocol: TransferProtocolType,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            self.s
                .update_firmware_simple_update(image_uri, targets, transfer_protocol)
                .await
        })
    }

    fn enable_rshim_bmc<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.enable_rshim_bmc().await })
    }

    fn clear_nvram<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.clear_nvram().await })
    }

    fn get_nic_mode<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<NicMode>, RedfishError>> {
        Box::pin(async move { self.s.get_nic_mode().await })
    }

    fn set_nic_mode<'a>(
        &'a self,
        mode: NicMode,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.set_nic_mode(mode).await })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let attrs: HashMap<String, serde_json::Value> =
                HashMap::from([("BootModes_InfiniteBootRetry".to_string(), "Enabled".into())]);
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
            let infinite_boot_status = jsonmap::get_str(
                bios_attributes,
                "BootModes_InfiniteBootRetry",
                "Bios Attributes",
            )?;

            Ok(Some(
                infinite_boot_status == EnabledDisabled::Enabled.to_string(),
            ))
        })
    }

    fn set_host_rshim<'a>(
        &'a self,
        enabled: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.set_host_rshim(enabled).await })
    }

    fn get_host_rshim<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<EnabledDisabled>, RedfishError>> {
        Box::pin(async move { self.s.get_host_rshim().await })
    }

    fn set_idrac_lockdown<'a>(
        &'a self,
        enabled: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.set_idrac_lockdown(enabled).await })
    }

    fn get_boss_controller<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.s.get_boss_controller().await })
    }

    fn decommission_storage_controller<'a>(
        &'a self,
        controller_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.s.decommission_storage_controller(controller_id).await })
    }

    fn create_storage_volume<'a>(
        &'a self,
        controller_id: &'a str,
        volume_name: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            self.s
                .create_storage_volume(controller_id, volume_name)
                .await
        })
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            // Check if Network is first in the boot order
            let system = self.get_system().await?;
            let Some(first_boot_id) = system.boot.boot_order.first() else {
                return Ok(false);
            };
            let boot_first = self.get_boot_option(first_boot_id).await?;
            if boot_first.name != "Network" {
                return Ok(false);
            }

            // Check if the specific MAC address is first in the network boot order
            let mac = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            let (expected, actual) = self.get_expected_and_actual_first_boot_option(&mac).await?;
            if expected.is_none() || expected != actual {
                return Ok(false);
            }

            self.is_general_boot_order_hard_disk_present().await
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

    fn get_component_integrities<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        Box::pin(async move { self.s.get_component_integrities().await })
    }

    fn get_firmware_for_component<'a>(
        &'a self,
        componnent_integrity_id: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::software_inventory::SoftwareInventory, RedfishError>,
    > {
        Box::pin(async move {
            self.s
                .get_firmware_for_component(componnent_integrity_id)
                .await
        })
    }

    fn get_component_ca_certificate<'a>(
        &'a self,
        url: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::component_integrity::CaCertificate, RedfishError>,
    > {
        Box::pin(async move { self.s.get_component_ca_certificate(url).await })
    }

    fn trigger_evidence_collection<'a>(
        &'a self,
        url: &'a str,
        nonce: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move { self.s.trigger_evidence_collection(url, nonce).await })
    }

    fn get_evidence<'a>(
        &'a self,
        url: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::model::component_integrity::Evidence, RedfishError>>
    {
        Box::pin(async move { self.s.get_evidence(url).await })
    }

    fn set_host_privilege_level<'a>(
        &'a self,
        level: HostPrivilegeLevel,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.set_host_privilege_level(level).await })
    }

    fn set_utc_timezone<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.set_utc_timezone().await })
    }

    fn set_ntp_servers<'a>(
        &'a self,
        servers: &'a [String],
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.set_manager_ntp_servers(servers).await })
    }
}

impl Bmc {
    /// Set DPU as first network boot option using OEM NetworkBootOrder path (older firmware).
    /// Boot options are strings like "UEFI: SLOT2 HTTP IPv4 Nvidia Network Adapter - A0:88:C2:08:53:C4"
    async fn set_boot_order_dpu_first_oem(
        &self,
        mac_address: &str,
    ) -> Result<Option<String>, RedfishError> {
        // Lenovo BMCs can drop Hard Disk from the general boot order, causing
        // instance creation to fail because the tenant OS cannot boot.
        self.enable_hard_disk_in_general_boot_order().await?;

        let mac = mac_address.to_string();
        // We see three patterns for HTTP IPv4 DPU boot option names in a Lenovo's network boot order:
        // "UEFI:   SLOT2 (31/0/0) HTTP IPv4  Nvidia Network Adapter - A0:88:C2:08:53:C4",
        // "UEFI:   SLOT1 (4B/0/0) HTTP IPv4  Mellanox Network Adapter - B8:3F:D2:90:99:C4"
        // "UEFI:   SLOT 1 (41/0/0) HTTP IPv4  Nvidia BlueField-3 VPI QSFP112 2P 200G PCIe Gen5 x16 - 5C:25:73:79:DA:5C"
        // This regex pattern uses .*? (non-greedy match) to allow any characters to appear between "Nvidia" and the MAC address.
        let net_boot_option_pattern = format!("HTTP IPv4  (Mellanox|Nvidia).*? - {}", mac);
        let net_boot_option_regex =
            Regex::new(&net_boot_option_pattern).map_err(|err| RedfishError::GenericError {
                error: format!(
                    "could not create net_boot_option_regex from {net_boot_option_pattern}: {err}"
                ),
            })?;

        // Check boot_order_supported for the list of currently supported boot options.
        // Set boot_order_next because that's what will happen when we reboot.
        // boot_order_current is the current order.
        let mut net_boot_order = self.get_network_boot_order().await?;
        let dpu_boot_option = net_boot_order
            .boot_order_supported
            .iter()
            .find(|s| net_boot_option_regex.is_match(s))
            .ok_or_else(|| {
                RedfishError::MissingBootOption(format!(
                    "Oem/Lenovo NetworkBootOrder BootOrderSupported {mac} (matching on {net_boot_option_pattern}); currently supported boot options: {:#?}",
                    net_boot_order.boot_order_supported
                ))
            })?;

        if let Some(pos) = net_boot_order
            .boot_order_next
            .iter()
            .position(|s| s == dpu_boot_option)
        {
            // the DPU boot option is already at the first index of the boot_order_next list
            if pos == 0 {
                tracing::info!(
                    "NO-OP: DPU ({mac_address}) will already be the first netboot option ({dpu_boot_option}) after reboot"
                );
                return Ok(None);
            } else {
                // boot_order_next contains the DPU boot option. move it to the front.
                net_boot_order.boot_order_next.swap(0, pos);
            }
        } else {
            // boot_order_next did not have the DPU boot option. add it to the beginning.
            net_boot_order
                .boot_order_next
                .insert(0, dpu_boot_option.clone());
        }

        // Patch remote
        let url = format!(
            "{}/BootOrder.NetworkBootOrder",
            self.get_boot_settings_uri()
        );
        let body = HashMap::from([("BootOrderNext", net_boot_order.boot_order_next.clone())]);
        self.s
            .client
            .patch(&url, body)
            .await
            .map(|_status_code| ())?;
        Ok(None)
    }

    /// Set DPU as first network boot option using BIOS attributes (newer firmware).
    /// Boot options are BIOS attributes like BootOrder_NetworkPriority_1 with
    /// values like "Slot5Port1HTTPv4NvidiaNetworkAdapter_B8_E9_24_18_42_52".
    async fn set_boot_order_dpu_first_bios_attr(
        &self,
        mac_address: &str,
    ) -> Result<Option<String>, RedfishError> {
        let mac = mac_address.replace(':', "_").to_uppercase();
        let bios = self.s.bios_attributes().await?;

        let (pos, dpu_val) = (1u32..=10)
            .find_map(|i| {
                bios.get(format!("BootOrder_NetworkPriority_{i}"))
                    .and_then(|v| v.as_str())
                    .filter(|v| v.to_uppercase().contains(&mac) && v.contains("HTTPv4"))
                    .map(|v| (i, v.to_string()))
            })
            .ok_or_else(|| {
                RedfishError::MissingBootOption(format!(
                    "No BootOrder_NetworkPriority_* contains MAC {mac_address} (HTTPv4)"
                ))
            })?;

        if pos == 1 {
            return Ok(None);
        }

        // Swap our DPU with the old network priority
        let mut attrs = HashMap::from([("BootOrder_NetworkPriority_1".to_string(), dpu_val)]);
        if let Some(old) = bios
            .get("BootOrder_NetworkPriority_1")
            .and_then(|v| v.as_str())
        {
            attrs.insert(format!("BootOrder_NetworkPriority_{pos}"), old.to_string());
        }

        self.s
            .client
            .patch(
                &format!("Systems/{}/Bios/Pending", self.s.system_id()),
                HashMap::from([("Attributes", attrs)]),
            )
            .await
            .map(|_| ())?;

        Ok(None)
    }

    /// Get expected and actual first boot option using OEM NetworkBootOrder path (older firmware).
    async fn get_expected_and_actual_first_boot_option_oem(
        &self,
        boot_interface_mac: &str,
    ) -> Result<(Option<String>, Option<String>), RedfishError> {
        let mac = boot_interface_mac.to_string();
        // We see three patterns for HTTP IPv4 DPU boot option names in a Lenovo's network boot order:
        // "UEFI:   SLOT2 (31/0/0) HTTP IPv4  Nvidia Network Adapter - A0:88:C2:08:53:C4",
        // "UEFI:   SLOT1 (4B/0/0) HTTP IPv4  Mellanox Network Adapter - B8:3F:D2:90:99:C4"
        // "UEFI:   SLOT 1 (41/0/0) HTTP IPv4  Nvidia BlueField-3 VPI QSFP112 2P 200G PCIe Gen5 x16 - 5C:25:73:79:DA:5C"
        // This regex pattern uses .*? (non-greedy match) to allow any characters to appear between "Nvidia" and the MAC address.
        let net_boot_option_pattern = format!("HTTP IPv4  (Mellanox|Nvidia).*? - {}", mac);
        let net_boot_option_regex =
            Regex::new(&net_boot_option_pattern).map_err(|err| RedfishError::GenericError {
                error: format!(
                    "could not create net_boot_option_regex from {net_boot_option_pattern}: {err}"
                ),
            })?;

        // Check boot_order_supported for the list of currently supported boot options.
        // Set boot_order_next because that's what will happen when we reboot.
        // boot_order_current is the current order.
        let net_boot_order = self.get_network_boot_order().await?;
        let expected_first_boot_option = net_boot_order
            .boot_order_supported
            .iter()
            .find(|s| net_boot_option_regex.is_match(s))
            .cloned();

        let actual_first_boot_option = net_boot_order.boot_order_next.first().cloned();

        Ok((expected_first_boot_option, actual_first_boot_option))
    }

    /// Get expected and actual first boot option using BIOS attributes (newer firmware).
    async fn get_expected_and_actual_first_boot_option_bios_attr(
        &self,
        boot_interface_mac: &str,
    ) -> Result<(Option<String>, Option<String>), RedfishError> {
        let mac = boot_interface_mac.replace(':', "_").to_uppercase();
        let bios = self.s.bios_attributes().await?;

        let expected = (1u32..=10).find_map(|i| {
            bios.get(format!("BootOrder_NetworkPriority_{i}"))
                .and_then(|v| v.as_str())
                .filter(|v| v.to_uppercase().contains(&mac) && v.contains("HTTPv4"))
                .map(|v| v.to_string())
        });

        let actual = bios
            .get("BootOrder_NetworkPriority_1")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok((expected, actual))
    }

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

        // clear_tpm has no 'check' operation, so skip that

        let virt = self.get_virt_enabled().await?;
        if virt != EnabledDisabled::Enabled {
            diffs.push(MachineSetupDiff {
                key: "Processors_IntelVirtualizationTechnology".to_string(),
                expected: EnabledDisabled::Enabled.to_string(),
                actual: virt.to_string(),
            });
        }

        let bios = self.s.bios_attributes().await?;
        for (key, expected) in self.uefi_boot_only_attributes() {
            let Some(actual) = bios.get(key) else {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    expected: expected.to_string(),
                    actual: "_missing_".to_string(),
                });
                continue;
            };
            let actual_str = actual.as_str().unwrap_or("_wrong_type_");
            if actual_str != expected {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    expected: expected.to_string(),
                    actual: actual_str.to_string(),
                });
            }
        }

        // Get the first boot option from the actual boot order
        // Some lenovos return an unordered BootOptions collection
        let system = self.get_system().await?;
        let boot_first_name = match system.boot.boot_order.first() {
            Some(first_boot_id) => self.get_boot_option(first_boot_id).await?.name,
            None => "_empty_boot_order_".to_string(),
        };
        if boot_first_name != "Network" {
            // Boot::Pxe maps to lenovo::BootOptionName::Network
            diffs.push(MachineSetupDiff {
                key: "boot_first_type".to_string(),
                expected: lenovo::BootOptionName::Network.to_string(),
                actual: boot_first_name,
            });
        }

        Ok(diffs)
    }

    /// Lock a Lenovo server to make it ready for tenants
    async fn enable_lockdown(&self) -> Result<(), RedfishError> {
        self.set_kcs_lenovo(false).await.inspect_err(|err| {
            debug!(%err, "Failed disabling 'IPMI over KCS Access'");
        })?;
        self.set_firmware_rollback_lenovo(EnabledDisabled::Disabled)
            .await
            .inspect_err(|err| {
                debug!(%err, "Failed changing 'Prevent System Firmware Down-Level'");
            })?;
        self.set_ethernet_over_usb(false).await.inspect_err(|err| {
            debug!(%err, "Failed disabling Ethernet over USB");
        })?;
        self.set_front_panel_usb_lenovo(
            lenovo::FrontPanelUSBMode::Server,
            lenovo::PortSwitchingMode::Server,
        )
        .await
        .inspect_err(|err| {
            debug!(%err, "Failed locking front panel USB to host-only.");
        })?;
        Ok(())
    }

    /// Unlock a Lenovo server, restoring defaults
    pub async fn disable_lockdown(&self) -> Result<(), RedfishError> {
        self.set_kcs_lenovo(true).await.inspect_err(|err| {
            debug!(%err, "Failed enabling 'IPMI over KCS Access'");
        })?;
        self.set_firmware_rollback_lenovo(EnabledDisabled::Enabled)
            .await
            .inspect_err(|err| {
                debug!(%err, "Failed changing 'Prevent System Firmware Down-Level'");
            })?;
        self.set_ethernet_over_usb(true).await.inspect_err(|err| {
            debug!(%err, "Failed disabling Ethernet over USB");
        })?;
        self.set_front_panel_usb_lenovo(
            lenovo::FrontPanelUSBMode::Shared,
            lenovo::PortSwitchingMode::Server,
        )
        .await
        .inspect_err(|err| {
            debug!(%err, "Failed unlocking front panel USB to shared mode.");
        })?;
        Ok(())
    }

    async fn get_kcs_value(&self) -> Result<Value, RedfishError> {
        let url = format!("Managers/{}", self.s.manager_id());
        let (_, body): (_, HashMap<String, serde_json::Value>) = self.s.client.get(&url).await?;

        let oem_obj = jsonmap::get_object(&body, "Oem", &url)?;
        let lenovo_obj = jsonmap::get_object(oem_obj, "Lenovo", &url)?;
        let is_kcs_enabled = jsonmap::get_value(lenovo_obj, "KCSEnabled", &url)?;

        Ok(is_kcs_enabled.clone())
    }

    async fn set_kcs_lenovo(&self, is_allowed: bool) -> Result<(), RedfishError> {
        let kcs_val: Value = match self.get_kcs_value().await? {
            Value::Bool(_) => serde_json::Value::Bool(is_allowed),
            Value::String(_) => {
                if is_allowed {
                    serde_json::Value::String("Enabled".to_owned())
                } else {
                    serde_json::Value::String("Disabled".to_owned())
                }
            }
            v => {
                return Err(RedfishError::InvalidValue {
                    url: format!("Managers/{}", self.s.manager_id()),
                    field: "KCS".to_string(),
                    err: InvalidValueError(format!(
                        "expected bool or string as KCS enabled value type; got {v}"
                    )),
                })
            }
        };

        let body = HashMap::from([(
            "Oem",
            HashMap::from([("Lenovo", HashMap::from([("KCSEnabled", kcs_val)]))]),
        )]);
        let url = format!("Managers/{}", self.s.manager_id());
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_kcs_lenovo(&self) -> Result<bool, RedfishError> {
        let manager = self.get_manager().await?;
        match &manager.oem {
            Some(oem) => match &oem.lenovo {
                Some(lenovo_oem) => Ok(lenovo_oem.kcs_enabled),
                None => Err(RedfishError::GenericError {
                    error: format!(
                        "Manager is missing Lenovo specific OEM field: \n{:#?}",
                        manager.clone()
                    ),
                }),
            },
            None => Err(RedfishError::GenericError {
                error: format!("Manager is missing OEM field: \n{:#?}", manager.clone()),
            }),
        }
    }

    async fn set_firmware_rollback_lenovo(&self, set: EnabledDisabled) -> Result<(), RedfishError> {
        let body = HashMap::from([(
            "Configurator",
            HashMap::from([("FWRollback", set.to_string())]),
        )]);
        let url = format!("Managers/{}/Oem/Lenovo/Security", self.s.manager_id());
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_firmware_rollback_lenovo(&self) -> Result<EnabledDisabled, RedfishError> {
        let url = format!("Managers/{}/Oem/Lenovo/Security", self.s.manager_id());
        let (_, body): (_, HashMap<String, serde_json::Value>) = self.s.client.get(&url).await?;

        let configurator = jsonmap::get_object(&body, "Configurator", &url)?;
        let fw_rollback = jsonmap::get_str(configurator, "FWRollback", &url)?;

        let fw_typed = fw_rollback
            .parse()
            .map_err(|_| RedfishError::InvalidKeyType {
                key: "FWRollback".to_string(),
                expected_type: "EnabledDisabled".to_string(),
                url: url.to_string(),
            })?;
        Ok(fw_typed)
    }

    async fn get_front_panel_usb_kv_lenovo(&self) -> Result<(String, FrontPanelUSB), RedfishError> {
        let url = format!("Systems/{}", self.s.system_id());
        let (_, body): (_, HashMap<String, serde_json::Value>) = self.s.client.get(&url).await?;

        let oem_obj = jsonmap::get_object(&body, "Oem", &url)?;
        let lenovo_obj = jsonmap::get_object(oem_obj, "Lenovo", &url)?;

        let mut front_panel_usb_key = "FrontPanelUSB";
        let val = match lenovo_obj.get(front_panel_usb_key) {
            Some(val) => val,
            None => {
                front_panel_usb_key = "USBManagementPortAssignment";
                match lenovo_obj.get(front_panel_usb_key) {
                    Some(val) => val,
                    None => {
                        return Err(RedfishError::MissingKey {
                            key: front_panel_usb_key.to_string(),
                            url,
                        })
                    }
                }
            }
        };

        let front_panel_usb_val = serde_json::from_value(val.clone()).map_err(|err| {
            RedfishError::JsonDeserializeError {
                url,
                body: format!("{val:?}"),
                source: err,
            }
        })?;

        Ok((front_panel_usb_key.to_string(), front_panel_usb_val))
    }

    async fn set_front_panel_usb_lenovo(
        &self,
        mode: lenovo::FrontPanelUSBMode,
        owner: lenovo::PortSwitchingMode,
    ) -> Result<(), RedfishError> {
        let mut body = HashMap::new();
        let (front_panel_usb_key, _) = self.get_front_panel_usb_kv_lenovo().await?;
        body.insert(
            "Oem",
            HashMap::from([(
                "Lenovo",
                HashMap::from([(
                    front_panel_usb_key,
                    HashMap::from([
                        ("FPMode", mode.to_string()),
                        ("PortSwitchingTo", owner.to_string()),
                    ]),
                )]),
            )]),
        );
        let url = format!("Systems/{}", self.s.system_id());
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_front_panel_usb_lenovo(&self) -> Result<lenovo::FrontPanelUSB, RedfishError> {
        let (_, front_panel_usb_val) = self.get_front_panel_usb_kv_lenovo().await?;
        Ok(front_panel_usb_val)
    }

    async fn set_ethernet_over_usb(&self, is_allowed: bool) -> Result<(), RedfishError> {
        let body = HashMap::from([("InterfaceEnabled", is_allowed)]);
        let url = format!("Managers/{}/EthernetInterfaces/ToHost", self.s.manager_id());
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_ethernet_over_usb(&self) -> Result<bool, RedfishError> {
        let url = format!("Managers/{}/EthernetInterfaces/ToHost", self.s.manager_id());
        let (_, body): (_, HashMap<String, serde_json::Value>) = self.s.client.get(&url).await?;

        jsonmap::get_bool(&body, "InterfaceEnabled", &url)
    }

    /// Both Intel and AMD have virtualization technologies that help fix the issue of x86 instruction
    /// architecture not being virtualizable.
    /// get_enable_virtualization_key returns the KEY for enabling virtualization in the bios attributes
    /// map that the Lenovo's BMC returns when querying the bios attributes registry. The string returned
    /// will depend on the processors within the given Lenovo. For example, 655v3/675v3s use AMD processors
    /// whereas, 650v2/670v2s use Intel processors.
    async fn get_enable_virtualization_key(
        &self,
        bios_attributes: &Value,
    ) -> Result<&str, RedfishError> {
        const INTEL_ENABLE_VIRTUALIZATION_KEY: &str = "Processors_IntelVirtualizationTechnology";
        const AMD_ENABLE_VIRTUALIZATION_KEY: &str = "Processors_SVMMode";

        // Intel specific
        if bios_attributes
            .get(INTEL_ENABLE_VIRTUALIZATION_KEY)
            .is_some()
        {
            Ok(INTEL_ENABLE_VIRTUALIZATION_KEY)
        // AMD specific
        } else if bios_attributes.get(AMD_ENABLE_VIRTUALIZATION_KEY).is_some() {
            Ok(AMD_ENABLE_VIRTUALIZATION_KEY)
        } else {
            Err(RedfishError::MissingKey {
                key: format!(
                    "{}/{}",
                    INTEL_ENABLE_VIRTUALIZATION_KEY, AMD_ENABLE_VIRTUALIZATION_KEY
                )
                .to_string(),
                url: format!("Systems/{}/Bios", self.s.system_id()),
            })
        }
    }

    async fn set_virt_enable(&self) -> Result<(), RedfishError> {
        let bios = self.s.bios_attributes().await?;
        let mut body = HashMap::new();
        let enable_virtualization_key = self.get_enable_virtualization_key(&bios).await?;
        body.insert(
            "Attributes",
            HashMap::from([(enable_virtualization_key, "Enabled")]),
        );
        let url = format!("Systems/{}/Bios/Pending", self.s.system_id());
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_virt_enabled(&self) -> Result<EnabledDisabled, RedfishError> {
        let bios = self.s.bios_attributes().await?;
        let enable_virtualization_key = self.get_enable_virtualization_key(&bios).await?;
        let Some(val) = bios.get(enable_virtualization_key) else {
            return Err(RedfishError::MissingKey {
                key: enable_virtualization_key.to_string(),
                url: "bios".to_string(),
            });
        };
        let Some(val) = val.as_str() else {
            return Err(RedfishError::InvalidKeyType {
                key: enable_virtualization_key.to_string(),
                expected_type: "str".to_string(),
                url: "bios".to_string(),
            });
        };
        val.parse().map_err(|_e| RedfishError::InvalidKeyType {
            key: enable_virtualization_key.to_string(),
            expected_type: "EnabledDisabled".to_string(),
            url: "bios".to_string(),
        })
    }

    /// Set so that we only UEFI IPv4 HTTP boot, and we retry that.
    ///
    /// Disable PXE Boot
    /// Disable LegacyBIOS Mode (if supported)
    /// Set Bootmode to UEFI
    /// Enable IPv4 HTTP Boot
    /// Disable IPv4 PXE Boot
    /// Disable IPv6 PXE Boot
    /// Enable Infinite Boot Mode
    async fn set_uefi_boot_only(&self) -> Result<(), RedfishError> {
        let bios = self.bios().await?;
        let url = format!("Systems/{}/Bios", self.s.system_id());
        let attrs = jsonmap::get_object(&bios, "Attributes", &url)?;

        let mut attributes = self.uefi_boot_only_attributes();

        // Legacy BIOS attributes only exist in older systems
        // Only set them if they're present in the current BIOS
        if attrs.contains_key("LegacyBIOS_NonOnboardPXE") {
            attributes.insert("LegacyBIOS_NonOnboardPXE", "Disabled");
        }
        if attrs.contains_key("LegacyBIOS_LegacyBIOS") {
            attributes.insert("LegacyBIOS_LegacyBIOS", "Disabled");
        }

        let mut body = HashMap::new();
        body.insert("Attributes", attributes);
        let url = format!("Systems/{}/Bios/Pending", self.s.system_id());
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    fn uefi_boot_only_attributes(&self) -> HashMap<&str, &str> {
        HashMap::from([
            ("BootModes_SystemBootMode", "UEFIMode"),
            ("NetworkStackSettings_IPv4HTTPSupport", "Enabled"),
            ("NetworkStackSettings_IPv4PXESupport", "Disabled"),
            ("NetworkStackSettings_IPv6PXESupport", "Disabled"),
            ("BootModes_InfiniteBootRetry", "Enabled"),
            ("BootModes_PreventOSChangesToBootOrder", "Enabled"),
        ])
    }

    async fn set_boot_override(&self, target: lenovo::BootSource) -> Result<(), RedfishError> {
        let target_str = &target.to_string();
        let body = HashMap::from([(
            "Boot",
            HashMap::from([
                ("BootSourceOverrideEnabled", "Once"),
                ("BootSourceOverrideTarget", target_str),
            ]),
        )]);
        let url = format!("Systems/{}", self.s.system_id());
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    // name: The name of the device you want to make the first boot choice.
    //
    // Note that _within_ the type you choose you could also give the order. e.g for "Network"
    // see Systems/1/Oem/Lenovo/BootSettings/BootOrder.NetworkBootOrder
    // and for "HardDisk" see Systems/1/Oem/Lenovo/BootSettings/BootOrder.HardDiskBootOrder
    async fn set_boot_first(&self, name: lenovo::BootOptionName) -> Result<(), RedfishError> {
        let boot_array = match self.get_boot_options_ids_with_first(name).await? {
            None => {
                return Err(RedfishError::MissingBootOption(name.to_string()));
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
        with_name: lenovo::BootOptionName,
    ) -> Result<Option<Vec<String>>, RedfishError> {
        let with_name_str = with_name.to_string();
        let mut with_name_match = None; // the ID of the option matching with_name
        let mut ordered = Vec::new(); // the final boot options
        let boot_options = self.s.get_boot_options().await?;
        for member in boot_options.members {
            let url = member
                .odata_id
                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
            let b: BootOption = self.s.client.get(&url).await?.1;
            if b.name == with_name_str {
                with_name_match = Some(b.id);
            } else {
                ordered.push(b.id);
            }
        }
        match with_name_match {
            None => Ok(None),
            Some(with_name_id) => {
                ordered.insert(0, with_name_id);
                Ok(Some(ordered))
            }
        }
    }

    // lenovo stores the sel as part of the system
    async fn get_system_event_log(&self) -> Result<Vec<LogEntry>, RedfishError> {
        let url = format!("Systems/{}/LogServices/SEL", self.s.system_id());
        let (_status_code, log_service): (_, LogService) = self.s.client.get(&url).await?;
        // If there are no log entries, this field and the `SEL/Entries` endpoint do not exist.
        if log_service.entries.is_none() {
            return Ok(vec![]);
        }
        let url = format!("Systems/{}/LogServices/SEL/Entries", self.s.system_id());
        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.s.client.get(&url).await?;
        let log_entries = log_entry_collection.members;
        Ok(log_entries)
    }

    async fn is_lenovo_sr_675_v3_ovx(&self) -> Result<bool, RedfishError> {
        let system = self.get_system().await?;
        match system.sku {
            /*  7D9RCTOLWW is the SKU for Lenovo ThinkSystem SR675 V3 OVX
                Taken from sample redfish response against an SR675 in AZ51:
                curl -k -D - --user root:'password' -H 'Content-Type: application/json' -X GET https://10.91.48.100:443/redfish/v1/Systems/1
                {..."SKU":"7D9RCTOLWW","PowerState":"On"...}
            */
            Some(sku) => Ok(sku == "7D9RCTOLWW"),
            None => Err(RedfishError::MissingKey {
                key: "sku".to_string(),
                url: "Systems".to_string(),
            }),
        }
    }

    async fn get_bmc_version(&self) -> Result<String, RedfishError> {
        let uefi_fw_info = self.get_firmware("BMC-Primary").await?;
        Ok(uefi_fw_info.version.unwrap_or_default())
    }

    async fn get_uefi_version(&self) -> Result<String, RedfishError> {
        let uefi_fw_info = self.get_firmware("UEFI").await?;
        Ok(uefi_fw_info.version.unwrap_or_default())
    }

    async fn use_workaround_for_force_restart(&self) -> Result<bool, RedfishError> {
        if self.is_lenovo_sr_675_v3_ovx().await? {
            let uefi_version = self.get_uefi_version().await?;
            let bmc_version = self.get_bmc_version().await?;

            let is_uefi_at_7_10 = version_compare::compare(uefi_version, "7.10")
                .is_ok_and(|c| c == version_compare::Cmp::Eq);

            let is_bmc_at_9_10 = version_compare::compare(bmc_version, "9.10")
                .is_ok_and(|c| c == version_compare::Cmp::Eq);

            if is_uefi_at_7_10 && is_bmc_at_9_10 {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn get_boot_settings_uri(&self) -> String {
        format!("Systems/{}/Oem/Lenovo/BootSettings", self.s.system_id())
    }

    async fn get_general_boot_order(&self) -> Result<LenovoBootOrder, RedfishError> {
        let boot_settings_uri = self.get_boot_settings_uri();
        // Some Lenovo BMCs lazily initialize the child boot-order resources only
        // after their parent collection has been read.
        self.s
            .client
            .get::<BootSettings>(&boot_settings_uri)
            .await?;

        let url = format!("{boot_settings_uri}/BootOrder.BootOrder");
        self.s.client.get(&url).await.map(|response| response.1)
    }

    async fn is_general_boot_order_hard_disk_present(&self) -> Result<bool, RedfishError> {
        match self.get_general_boot_order().await {
            Ok(boot_order) => Ok(boot_order
                .boot_order_next
                .iter()
                .any(|option| option == GENERAL_BOOT_ORDER_HARD_DISK)),
            Err(RedfishError::HTTPErrorCode {
                status_code: StatusCode::NOT_FOUND,
                ..
            }) => Ok(true),
            Err(e) => Err(e),
        }
    }

    async fn enable_hard_disk_in_general_boot_order(&self) -> Result<(), RedfishError> {
        let mut boot_order = self.get_general_boot_order().await?;
        if boot_order
            .boot_order_next
            .iter()
            .any(|option| option == GENERAL_BOOT_ORDER_HARD_DISK)
        {
            return Ok(());
        }

        let insert_at = boot_order.boot_order_next.len().min(1);
        boot_order
            .boot_order_next
            .insert(insert_at, GENERAL_BOOT_ORDER_HARD_DISK.to_string());

        let url = format!("{}/BootOrder.BootOrder", self.get_boot_settings_uri());
        let body = HashMap::from([("BootOrderNext", boot_order.boot_order_next)]);
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_network_boot_order(&self) -> Result<LenovoBootOrder, RedfishError> {
        let url = self.get_boot_settings_uri();
        let (_status_code, boot_settings): (_, BootSettings) = self.s.client.get(&url).await?;
        for member in &boot_settings.members {
            let id = member.odata_id_get()?;
            if id.contains("BootOrder.NetworkBootOrder") {
                let (_status_code, net_boot_order): (_, LenovoBootOrder) =
                    self.s.client.get(&format!("{url}/{id}")).await?;

                return Ok(net_boot_order);
            }
        }

        Err(RedfishError::GenericError {
            error: format!(
                "Could not find the NetworkBootOrder out of Boot Settings members: {:#?}",
                boot_settings.members
            ),
        })
    }

    /// Set "Network" as the first entry in the OEM Lenovo general boot order
    /// (`BootOrder.BootOrder`). This must happen before setting the DPU-first
    /// network boot order, because the network boot options list is empty
    /// when "Network" is not already present in the general boot order.
    async fn set_general_boot_order_network_first(&self) -> Result<(), RedfishError> {
        let mut boot_order = self.get_general_boot_order().await?;
        const NETWORK: &str = "Network";

        if let Some(pos) = boot_order.boot_order_next.iter().position(|s| s == NETWORK) {
            if pos == 0 {
                tracing::info!("NO-OP: Network is already the first general boot option");
                return Ok(());
            }
            boot_order.boot_order_next.swap(0, pos);
        } else {
            boot_order.boot_order_next.insert(0, NETWORK.to_string());
        }

        let url = format!("{}/BootOrder.BootOrder", self.get_boot_settings_uri());
        let body = HashMap::from([("BootOrderNext", boot_order.boot_order_next)]);
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_expected_and_actual_first_boot_option(
        &self,
        boot_interface_mac: &str,
    ) -> Result<(Option<String>, Option<String>), RedfishError> {
        // Try the OEM NetworkBootOrder path first (older firmware)
        match self
            .get_expected_and_actual_first_boot_option_oem(boot_interface_mac)
            .await
        {
            Ok(result) => return Ok(result),
            Err(RedfishError::HTTPErrorCode {
                status_code: StatusCode::NOT_FOUND,
                ..
            }) => {
                // OEM path doesn't exist, fall back to BIOS attributes (newer firmware)
            }
            Err(e) => return Err(e),
        }

        self.get_expected_and_actual_first_boot_option_bios_attr(boot_interface_mac)
            .await
    }
}

#[derive(Debug, Default, Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
struct UpdateParameters {
    targets: Vec<String>,
    #[serde(rename = "@Redfish.OperationApplyTime")]
    operation_apply_time: String,
    force_update: bool,
}

impl UpdateParameters {
    fn new() -> Self {
        Self {
            targets: vec![],
            operation_apply_time: "Immediate".to_string(),
            force_update: true,
        }
    }
}
