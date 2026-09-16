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
use std::collections::HashMap;

use serde_json::Value;

use crate::{
    model::{
        boot::BootOverride,
        chassis::NetworkAdapter,
        oem::hpe::{self, BootDevices},
        sel::{LogEntry, LogEntryCollection},
        service_root::RedfishVendor,
        storage::{self},
        Slot, SystemStatus,
    },
    network::REDFISH_ENDPOINT,
    standard::RedfishStandard,
    BiosProfileType, Boot, Deserialize,
    EnabledDisabled::{self, Disabled, Enabled},
    MachineSetupDiff, MachineSetupStatus, OData, ODataId, PCIeDevice, PowerState, Redfish,
    RedfishError, Serialize, Status, StatusInternal, SystemPowerControl,
};

// The following is specific for the HPE machine since the HPE redfish
// doesn't return pcie odata.id during power on transition
// HpeOData structure will try to capture all those 4 properties.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct HpeOData {
    #[serde(rename = "@odata.id")]
    pub odata_id: Option<String>, // This is unique for HPE machine
    #[serde(rename = "@odata.type")]
    pub odata_type: String,
    #[serde(rename = "@odata.etag")]
    pub odata_etag: Option<String>,
    #[serde(rename = "@odata.context")]
    pub odata_context: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct HpePCIeDevice {
    #[serde(flatten)]
    pub odata: HpeOData,
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
            if action == SystemPowerControl::ForceRestart {
                // hpe ilo does warm reset with gracefulrestart op
                self.s.power(SystemPowerControl::GracefulRestart).await
            } else if action == SystemPowerControl::ACPowercycle {
                let power_state = self.get_power_state().await?;
                match power_state {
                    PowerState::Off => {}
                    _ => {
                        self.s.power(SystemPowerControl::ForceOff).await?;
                    }
                }
                let args: HashMap<String, String> =
                    HashMap::from([("ResetType".to_string(), "AuxCycle".to_string())]);
                let url = format!(
                    "Systems/{}/Actions/Oem/Hpe/HpeComputerSystemExt.SystemReset",
                    self.s.system_id()
                );
                return self.s.client.post(&url, args).await.map(|_status_code| ());
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
            let manager_id = self.s.manager_id();
            let url = format!("Managers/{manager_id}/LogServices/IEL/Entries");
            self.s.fetch_bmc_event_log(url, from).await
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let hp_bios = self.s.bios().await?;
            // Access the Actions map
            let actions = hp_bios
                .get("Actions")
                .and_then(|v: &Value| v.as_object())
                .ok_or(RedfishError::NoContent)?;
            // Access the "#Bios.ResetBios" action
            let reset = actions
                .get("#Bios.ResetBios")
                .and_then(|v| v.as_object())
                .ok_or(RedfishError::NoContent)?;
            // Access the "target" URL
            let target = reset
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or(RedfishError::NoContent)?;
            let url = target.replace(&format!("/{REDFISH_ENDPOINT}/"), "");
            self.s
                .client
                .req::<(), ()>(reqwest::Method::POST, &url, None, None, None, Vec::new())
                .await
                .map(|_resp| Ok(()))?
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
            self.setup_serial_console().await?;
            self.clear_tpm().await?;
            self.set_virt_enable().await?;
            self.set_uefi_nic_boot().await?;
            self.set_boot_order(BootDevices::Pxe).await?;
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
            use serde_json::Value;
            let hpe = Value::Object(serde_json::Map::from_iter(vec![
                (
                    "AuthFailureDelayTimeSeconds".to_string(),
                    Value::Number(2.into()), // Hpe iLO 5 only allows 2, 5, 10, 30
                ),
                (
                    "AuthFailureLoggingThreshold".to_string(),
                    Value::Number(0.into()), // Hpe iLO 5 only allows 0, 1, 2, 3, 5
                ),
                (
                    "AuthFailuresBeforeDelay".to_string(),
                    Value::Number(0.into()), // Hpe iLO 5 only allows 0, 1, 3, 5
                ),
                ("EnforcePasswordComplexity".to_string(), Value::Bool(false)),
            ]));
            let mut oem = serde_json::Map::new();
            oem.insert("Hpe".to_string(), hpe);

            let mut body = HashMap::new();
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
            match target {
                Enabled => self.enable_lockdown().await,
                Disabled => self.disable_lockdown().await,
            }
        })
    }

    fn lockdown_status<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            let mut url = format!("Systems/{}/Bios", self.s.system_id());
            let (_status_code, bios): (_, hpe::Bios) = self.s.client.get(url.as_str()).await?;
            let bios = bios.attributes;
            url = format!("Managers/{}", self.s.manager_id());
            let (_status, bmc): (_, hpe::SetOemHpeLockdown) =
                self.s.client.get(url.as_str()).await?;
            let message = format!(
                "usb_boot={}, virtual_nic_enabled={}",
                bios.usb_boot.as_deref().unwrap_or("Unknown"),
                bmc.oem.hpe.virtual_nic_enabled
            );
            // todo: kcs_enabled
            Ok(Status {
                message,
                status: if bios.usb_boot.as_deref() == Some("Disabled")
                    && !bmc.oem.hpe.virtual_nic_enabled
                // todo: && bios.kcs_enabled.as_deref() == Some("false")
                {
                    StatusInternal::Enabled
                // todo: if bios.usb_boot.as_deref() == Some("Enabled") && bios.kcs_enabled.as_deref() == Some("true")
                } else if bios.usb_boot.as_deref() == Some("Enabled")
                    && bmc.oem.hpe.virtual_nic_enabled
                {
                    StatusInternal::Disabled
                } else {
                    StatusInternal::Partial
                },
            })
        })
    }

    fn setup_serial_console<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let serial_console = hpe::BiosSerialConsoleAttributes {
                embedded_serial_port: "Com2Irq3".to_string(),
                ems_console: "Virtual".to_string(),
                serial_console_baud_rate: "BaudRate115200".to_string(),
                serial_console_emulation: "Vt100Plus".to_string(),
                serial_console_port: "Virtual".to_string(),
                uefi_serial_debug_level: "ErrorsOnly".to_string(),
                virtual_serial_port: "Com1Irq4".to_string(),
            };
            let set_serial_attrs = hpe::SetBiosSerialConsoleAttributes {
                attributes: serial_console,
            };
            let url = format!("Systems/{}/Bios/settings/", self.s.system_id());
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
            self.bios_serial_console_status().await
            // TODO: add bmc serial console service status
        })
    }

    fn boot_first<'a>(
        &'a self,
        target: Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // TODO: possibly remove this redundant matching, the enum is based on the bmc capabilities
            match target {
                Boot::Pxe => self.set_boot_order(BootDevices::Pxe).await,
                Boot::HardDisk => self.set_boot_order(BootDevices::Hdd).await,
                Boot::UefiHttp => self.set_boot_order(BootDevices::UefiHttp).await,
            }
        })
    }

    fn boot_once<'a>(&'a self, target: Boot) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.boot_first(target).await })
    }

    /// HPE iLO does not implement the standard Redfish `Boot.HttpBootUri`
    /// PATCH path. The `HttpBootUri` field appears in the Boot block (as
    /// `null` in GET responses) and `UefiHttp` is advertised in
    /// `BootSourceOverrideTarget@Redfish.AllowableValues`, but PATCHing
    /// either rejects with `PropertyNotWritableOrUnknown` and
    /// `UnsupportedOperation` respectively. In fact, the entire
    /// `BootSourceOverride` PATCH mechanism is non-functional on iLO 6
    /// (at least in v1.58); even `BootSourceOverrideTarget: "Pxe"` is
    /// rejected.
    ///
    /// The HPE-specific path for pinning a UEFI HTTP boot URL is the
    /// `UrlBootFile` BIOS attribute (with optional `UrlBootFile2/3/4`
    /// secondaries and `PreBootNetwork` for IPv4/IPv6/Auto). Setting it
    /// creates a UEFI boot entry called "URL File" pointing at the URI;
    /// applies on next reset. Equivalent to the iLO HPREST/ilorest
    /// `select Bios; set UrlBootFile=...; commit` workflow, which is
    /// documented on Gen10/iLO 5 and Gen11/iLO 6.
    ///
    /// Verified working on ProLiant DL380a Gen11 machines (running both
    /// iLO 6 v1.58 and v1.72). Unlike Dell, there doesn't seem to be any
    /// sort of attribute engine dependency drama going on; UrlBootFile is
    /// a straightforward string attribute that's writable across the fleet.
    ///
    /// Returns `Ok(None)` on success: HPE doesn't return a Location/job ID
    /// here (unlike Dell). The pending state lives in `/Bios/Settings` and
    /// applies on next reboot; the response includes `SystemResetRequired`
    /// as confirmation that the change is staged.
    /// Callers reverting the change PATCH `UrlBootFile: ""` back.
    fn set_boot_override<'a>(
        &'a self,
        settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let Some(uri) = settings.http_boot_uri else {
                // HPE iLO 6 does not accept BootSourceOverrideTarget/Enabled
                // PATCHes. Even non-HTTP targets like Pxe are rejected with
                // UnsupportedOperation. Without an http_boot_uri to set via
                // the UrlBootFile BIOS attribute, no Redfish operation here.
                return Err(RedfishError::NotSupported(
                    "HPE set_boot_override requires http_boot_uri; \
                     BootSourceOverrideTarget/Enabled PATCHes are not \
                     functional via Redfish on iLO 6 (verified on v1.58 / \
                     v1.72: UnsupportedOperation for all override targets)"
                        .to_string(),
                ));
            };

            // HPE iLO uses lowercase `settings/` in its @Redfish.Settings
            // pointer (matches the existing helpers in this file, e.g.,
            // setup_serial_console / clear_tpm).
            let url = format!("Systems/{}/Bios/settings/", self.s.system_id());
            let body = serde_json::json!({
                "Attributes": {
                    "UrlBootFile": uri,
                    "PreBootNetwork": "IPv4",
                }
            });
            self.s.client.patch(&url, body).await?;
            Ok(None)
        })
    }

    fn clear_tpm<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let tpm = hpe::TpmAttributes {
                tpm2_operation: "Clear".to_string(),
                tpm_visibility: "Visible".to_string(),
            };
            let set_tpm_attrs = hpe::SetTpmAttributes { attributes: tpm };
            let url = format!("Systems/{}/Bios/settings/", self.s.system_id());
            self.s
                .client
                .patch(&url, set_tpm_attrs)
                .await
                .map(|_status_code| ())
        })
    }

    fn pending<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/settings/", self.s.system_id());
            self.s.pending_with_url(&url).await
        })
    }

    fn clear_pending<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // TODO
            Ok(())
        })
    }

    fn pcie_devices<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<PCIeDevice>, RedfishError>> {
        Box::pin(async move {
            let mut out = Vec::new();
            let chassis = self.get_chassis(self.s.system_id()).await?;
            let pcie_devices_odata = match chassis.pcie_devices {
                Some(odata) => odata,
                None => return Ok(vec![]),
            };
            let url = pcie_devices_odata
                .odata_id
                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
            let pcie_devices = self.s.get_members(&url).await?;
            let mut devices: Vec<HpePCIeDevice> = Vec::new();
            for pcie_oid in pcie_devices {
                let dev_url = format!("{}/{}", &url, pcie_oid);
                let (_, hpe_pcie) = self.s.client.get(&dev_url).await?;
                devices.push(hpe_pcie);
            }
            // for mut pcie in devices.members {
            for hpe_pcie in devices {
                let mut pcie = PCIeDevice {
                    odata: OData {
                        odata_type: hpe_pcie.odata.odata_type,
                        odata_id: hpe_pcie.odata.odata_id.unwrap_or_default(),
                        odata_etag: hpe_pcie.odata.odata_etag,
                        odata_context: hpe_pcie.odata.odata_context,
                    },
                    description: hpe_pcie.description,
                    firmware_version: hpe_pcie.firmware_version,
                    id: hpe_pcie.id,
                    manufacturer: hpe_pcie.manufacturer,
                    gpu_vendor: hpe_pcie.gpu_vendor,
                    name: hpe_pcie.name,
                    part_number: hpe_pcie.part_number,
                    serial_number: hpe_pcie.serial_number,
                    status: hpe_pcie.status,
                    slot: hpe_pcie.slot,
                    pcie_functions: hpe_pcie.pcie_functions,
                };
                if pcie.status.is_none() {
                    continue;
                }
                if let Some(serial) = pcie.serial_number.take() {
                    // DPUs has serial numbers like this: "MT2246XZ0908   "
                    pcie.serial_number = Some(serial.trim().to_string())
                }
                out.push(pcie);
            }
            out.sort_unstable_by(|a, b| a.manufacturer.cmp(&b.manufacturer));

            Ok(out)
        })
    }

    fn get_chassis_all<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_members("Chassis").await })
    }

    fn get_chassis_network_adapters<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let chassis = self.s.get_chassis(chassis_id).await?;
            if let Some(network_adapters_odata) = chassis.network_adapters {
                let url = network_adapters_odata
                    .odata_id
                    .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                // let url = format!("Chassis/{}/NetworkAdapters", chassis_id);
                self.s.get_members(&url).await
            } else {
                Ok(Vec::new())
            }
        })
    }

    fn get_base_network_adapters<'a>(
        &'a self,
        system_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/BaseNetworkAdapters", system_id);
            self.s.get_members(&url).await
        })
    }

    fn get_base_network_adapter<'a>(
        &'a self,
        system_id: &'a str,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/BaseNetworkAdapters/{}", system_id, id);
            let (_, body) = self.s.client.get(&url).await?;
            Ok(body)
        })
    }

    fn change_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
        new_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let hp_bios = self.s.bios().await?;
            // Access the Actions map
            let actions = hp_bios
                .get("Actions")
                .and_then(|v| v.as_object())
                .ok_or(RedfishError::NoContent)?;
            // Access the "#Bios.ChangePassword" action
            let change_password = actions
                .get("#Bios.ChangePassword")
                .and_then(|v| v.as_object())
                .ok_or(RedfishError::NoContent)?;
            // Access the "target" URL
            let target = change_password
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or(RedfishError::NoContent)?;

            let mut arg = HashMap::new();
            arg.insert("PasswordName", "AdministratorPassword".to_string());
            arg.insert("OldPassword", current_uefi_password.to_string());
            arg.insert("NewPassword", new_uefi_password.to_string());

            let url = target.replace(&format!("/{REDFISH_ENDPOINT}/"), "");
            self.s.client.post(&url, arg).await?;

            Ok(None)
        })
    }

    fn bmc_reset_to_defaults<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Managers/{}/Actions/Oem/Hpe/HpeiLO.ResetToFactoryDefaults",
                self.s.manager_id()
            );
            let mut arg = HashMap::new();
            arg.insert("Action", "HpeiLO.ResetToFactoryDefaults".to_string());
            arg.insert("ResetType", "Default".to_string());
            self.s.client.post(&url, arg).await.map(|_resp| Ok(()))?
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mac = crate::resolve_boot_interface_mac(self, boot_interface)
                .await?
                .to_uppercase();

            let all = self.get_boot_options().await?;
            let mut boot_ref = None;
            for b in all.members {
                let id = b.odata_id_get()?;
                let opt = self.get_boot_option(id).await?;
                let opt_name = opt.display_name.to_uppercase();
                if opt_name.contains("HTTP") && opt_name.contains("IPV4") && opt_name.contains(&mac)
                {
                    boot_ref = Some(opt.boot_option_reference);
                    break;
                }
            }
            let Some(boot_ref) = boot_ref else {
                return Err(RedfishError::MissingBootOption(format!("HTTP IPv4 {mac}")));
            };

            match self.set_first_boot(&boot_ref).await {
                Err(RedfishError::HTTPErrorCode {
                    url,
                    status_code,
                    response_body,
                }) => {
                    if response_body.contains("UnableToModifyDuringSystemPOST") {
                        tracing::info!(
                        "redfish set_first_boot might fail due to HPE POST race condition, ignore."
                    );
                        Ok(None)
                    } else {
                        Err(RedfishError::HTTPErrorCode {
                            url,
                            status_code,
                            response_body,
                        })
                    }
                }
                Ok(()) => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let mac = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            let (expected, actual) = self.get_expected_and_actual_first_boot_option(&mac).await?;
            Ok(expected.is_some() && expected == actual)
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

    fn set_ntp_servers<'a>(
        &'a self,
        servers: &'a [String],
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if servers.is_empty() {
                return Ok(());
            }

            // iLO supports at most 2 static NTP servers; extra entries are ignored.
            if servers.len() > 2 {
                tracing::warn!(
                    "iLO supports at most 2 static NTP servers; ignoring {} extra entries",
                    servers.len() - 2,
                );
            }
            let static_ntp_servers = vec![
                servers.first().cloned().unwrap_or_default(),
                servers.get(1).cloned().unwrap_or_default(),
            ];

            let url = format!("Managers/{}/DateTime", self.s.manager_id());
            let body = HashMap::from([("StaticNTPServers", static_ntp_servers)]);
            self.s.client.patch(&url, body).await.map(|_resp| ())
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

        // clear_tpm has no 'check' operation, so skip that

        let virt = self.get_virt_enabled().await?;
        if virt != EnabledDisabled::Enabled {
            diffs.push(MachineSetupDiff {
                key: "Processors_IntelVirtualizationTechnology".to_string(),
                expected: EnabledDisabled::Enabled.to_string(),
                actual: virt.to_string(),
            });
        }

        let (dhcpv4, http_support) = self.get_uefi_nic_boot().await?;
        if dhcpv4 != EnabledDisabled::Enabled {
            diffs.push(MachineSetupDiff {
                key: "Dhcpv4".to_string(),
                expected: EnabledDisabled::Enabled.to_string(),
                actual: dhcpv4.to_string(),
            });
        }
        if http_support != "Auto" {
            diffs.push(MachineSetupDiff {
                key: "HttpSupport".to_string(),
                expected: "Auto".to_string(),
                actual: http_support,
            });
        }

        Ok(diffs)
    }

    async fn enable_bios_lockdown(&self) -> Result<(), RedfishError> {
        let lockdown_attrs = hpe::BiosLockdownAttributes {
            //            kcs_enabled: None, // todo: this needs to be set to "false" based on the bmc and bios ver
            usb_boot: Disabled,
        };
        let set_lockdown = hpe::SetBiosLockdownAttributes {
            attributes: lockdown_attrs,
        };
        let url = format!("Systems/{}/Bios/settings/", self.s.system_id());
        self.s
            .client
            .patch(&url, set_lockdown)
            .await
            .map(|_status_code| ())
    }

    async fn enable_bmc_lockdown(&self) -> Result<(), RedfishError> {
        let lockdown_attrs = hpe::OemHpeLockdownAttrs {
            virtual_nic_enabled: false,
        };
        let set_lockdown1 = hpe::OemHpeLockdown {
            hpe: lockdown_attrs,
        };
        let set_lockdown2 = hpe::SetOemHpeLockdown { oem: set_lockdown1 };
        let url = format!("Managers/{}/", self.s.manager_id());
        self.s
            .client
            .patch(&url, set_lockdown2)
            .await
            .map(|_status_code| ())
    }

    async fn enable_bmc_lockdown2(&self) -> Result<(), RedfishError> {
        let netlockdown_attrs = hpe::OemHpeLockdownNetworkProtocolAttrs { kcs_enabled: false };
        let set_netlockdown1 = hpe::OemHpeNetLockdown {
            hpe: netlockdown_attrs,
        };
        let set_netlockdown2 = hpe::SetOemHpeNetLockdown {
            oem: set_netlockdown1,
        };
        let url = format!("Managers/{}/NetworkProtocol", self.s.manager_id());
        self.s
            .client
            .patch(&url, set_netlockdown2)
            .await
            .map(|_status_code| ())
    }

    async fn check_fw_version(&self) -> bool {
        let ilo_manager = self.get_manager().await;
        match ilo_manager {
            Ok(manager) => {
                let Some(fw_version) = manager.firmware_version else {
                    return false;
                };
                let fw_parts: Vec<&str> = fw_version.split_whitespace().collect();
                let fw_major: i32 = fw_parts[1].parse().unwrap_or_default();
                let fw_minor: f32 = fw_parts[2][1..].parse().unwrap_or(0.0);
                fw_major >= 6 && fw_minor >= 1.40
            }
            Err(_) => false,
        }
    }

    async fn enable_lockdown(&self) -> Result<(), RedfishError> {
        if self.check_fw_version().await {
            self.enable_bmc_lockdown2().await?;
        }
        self.enable_bios_lockdown().await?;
        self.enable_bmc_lockdown().await
    }

    async fn disable_bios_lockdown(&self) -> Result<(), RedfishError> {
        let lockdown_attrs = hpe::BiosLockdownAttributes {
            //            kcs_enabled: None, // todo: this needs to be set to "false" based on the bmc and bios ver
            usb_boot: Enabled,
        };
        let set_lockdown = hpe::SetBiosLockdownAttributes {
            attributes: lockdown_attrs,
        };
        let url = format!("Systems/{}/Bios/settings/", self.s.system_id());
        self.s
            .client
            .patch(&url, set_lockdown)
            .await
            .map(|_status_code| ())
    }

    async fn disable_bmc_lockdown(&self) -> Result<(), RedfishError> {
        let lockdown_attrs = hpe::OemHpeLockdownAttrs {
            virtual_nic_enabled: true,
        };
        let set_lockdown1 = hpe::OemHpeLockdown {
            hpe: lockdown_attrs,
        };
        let set_lockdown2 = hpe::SetOemHpeLockdown { oem: set_lockdown1 };
        let url = format!("Managers/{}/", self.s.manager_id());
        self.s
            .client
            .patch(&url, set_lockdown2)
            .await
            .map(|_status_code| ())
    }

    async fn disable_bmc_lockdown2(&self) -> Result<(), RedfishError> {
        let netlockdown_attrs = hpe::OemHpeLockdownNetworkProtocolAttrs { kcs_enabled: false };
        let set_netlockdown1 = hpe::OemHpeNetLockdown {
            hpe: netlockdown_attrs,
        };
        let set_netlockdown2 = hpe::SetOemHpeNetLockdown {
            oem: set_netlockdown1,
        };
        let url = format!("Managers/{}/NetworkProtocol", self.s.manager_id());
        self.s
            .client
            .patch(&url, set_netlockdown2)
            .await
            .map(|_status_code| ())
    }

    async fn disable_lockdown(&self) -> Result<(), RedfishError> {
        if self.check_fw_version().await {
            self.disable_bmc_lockdown2().await?;
        }
        self.disable_bios_lockdown().await?;
        self.disable_bmc_lockdown().await
    }

    /// Both Intel and AMD have virtualization technologies that help fix the issue of x86 instruction
    /// architecture not being virtualizable.
    /// get_enable_virtualization_key returns the KEY for enabling virtualization in the bios attributes
    /// map that the HPE BMC returns when querying the bios attributes registry. The string returned
    /// will depend on the processor type and BIOS version (e.g., iLO 7 may use ProcVirtualization instead of IntelProcVtd).
    async fn get_enable_virtualization_key(
        &self,
        bios_attributes: &Value,
    ) -> Result<&str, RedfishError> {
        const INTEL_ENABLE_VIRTUALIZATION_KEY: &str = "IntelProcVtd";
        const AMD_ENABLE_VIRTUALIZATION_KEY: &str = "ProcAmdIoVt";
        const PROC_VIRTUALIZATION_KEY: &str = "ProcVirtualization";

        // Intel specific (older iLO versions)
        if bios_attributes
            .get(INTEL_ENABLE_VIRTUALIZATION_KEY)
            .is_some()
        {
            Ok(INTEL_ENABLE_VIRTUALIZATION_KEY)
        // AMD specific
        } else if bios_attributes.get(AMD_ENABLE_VIRTUALIZATION_KEY).is_some() {
            Ok(AMD_ENABLE_VIRTUALIZATION_KEY)
        // iLO 7 Intel fallback
        } else if bios_attributes.get(PROC_VIRTUALIZATION_KEY).is_some() {
            Ok(PROC_VIRTUALIZATION_KEY)
        } else {
            Err(RedfishError::MissingKey {
                key: format!(
                    "{}/{}/{}",
                    INTEL_ENABLE_VIRTUALIZATION_KEY,
                    AMD_ENABLE_VIRTUALIZATION_KEY,
                    PROC_VIRTUALIZATION_KEY
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
        let url = format!("Systems/{}/Bios/settings", self.s.system_id());
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

    async fn set_uefi_nic_boot(&self) -> Result<(), RedfishError> {
        let uefi_nic_boot = hpe::UefiHttpAttributes {
            dhcpv4: Enabled,
            http_support: "Auto".to_string(),
        };
        let set_uefi_nic_boot = hpe::SetUefiHttpAttributes {
            attributes: uefi_nic_boot,
        };
        let url = format!("Systems/{}/Bios/settings/", self.s.system_id());
        self.s
            .client
            .patch(&url, set_uefi_nic_boot)
            .await
            .map(|_status_code| ())
    }

    async fn get_uefi_nic_boot(&self) -> Result<(EnabledDisabled, String), RedfishError> {
        let bios = self.s.bios_attributes().await?;

        let dhcpv4 = bios
            .get("Dhcpv4")
            .and_then(|v| v.as_str())
            .ok_or(RedfishError::MissingKey {
                key: "Dhcpv4".to_string(),
                url: "bios".to_string(),
            })?
            .parse()
            .map_err(|_| RedfishError::InvalidKeyType {
                key: "Dhcpv4".to_string(),
                expected_type: "EnabledDisabled".to_string(),
                url: "bios".to_string(),
            })?;

        let http_support = bios
            .get("HttpSupport")
            .and_then(|v| v.as_str())
            .ok_or(RedfishError::MissingKey {
                key: "HttpSupport".to_string(),
                url: "bios".to_string(),
            })?
            .to_string();

        Ok((dhcpv4, http_support))
    }

    async fn change_boot_order(&self, boot_array: Vec<String>) -> Result<(), RedfishError> {
        let new_boot_order = hpe::SetOemHpeBoot {
            persistent_boot_config_order: boot_array,
        };
        let url = format!("Systems/{}/Bios/oem/hpe/boot/settings/", self.s.system_id());
        self.s
            .client
            .patch(&url, new_boot_order)
            .await
            .map(|_status_code| ())
    }

    async fn set_boot_order(&self, name: BootDevices) -> Result<(), RedfishError> {
        let boot_array = match self.get_boot_options_ids_with_first(name).await? {
            None => {
                return Err(RedfishError::MissingBootOption(name.to_string()));
            }
            Some(b) => b,
        };
        self.change_boot_order(boot_array).await
    }

    async fn get_boot_options_ids_with_first(
        &self,
        device: BootDevices,
    ) -> Result<Option<Vec<String>>, RedfishError> {
        let with_name_str = match device {
            BootDevices::Pxe => "nic.",
            BootDevices::UefiHttp => "nic.",
            BootDevices::Hdd => "hd.",
            _ => ".",
        };
        let mut ordered = Vec::new(); // the final boot options
        let url = format!("Systems/{}/Bios/oem/hpe/boot/", self.s.system_id());
        let (_, body): (_, hpe::OemHpeBoot) = self.s.client.get(&url).await?;

        for member in body.persistent_boot_config_order {
            if member.to_ascii_lowercase().contains(with_name_str) {
                ordered.insert(0, member);
                continue;
            }
            ordered.push(member);
        }
        Ok(Some(ordered))
    }

    async fn get_system_event_log(&self) -> Result<Vec<LogEntry>, RedfishError> {
        let url = format!("Systems/{}/LogServices/IML/Entries", self.s.system_id());
        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.s.client.get(&url).await?;
        let log_entries = log_entry_collection.members;
        Ok(log_entries)
    }

    async fn bios_serial_console_status(&self) -> Result<Status, RedfishError> {
        let message = String::new();

        let enabled = true;
        let disabled = false;
        /*
        let url = &format!("Systems/{}/Bios", self.s.system_id());
        let (_status_code, bios): (_, hpe::Bios) = self.s.client.get(url).await?;
        let bios = bios.attributes;

        let val = bios.embedded_serial_port;
        message.push_str(&format!("embedded_serial_port={val} "));
        if &val == "Com2Irq3" {
            // enabled
            disabled = false;
        } else {
            // disabled
            enabled = false;
        }

        let val = bios.ems_console;
        message.push_str(&format!("ems_console={val} "));
        if &val == "Virtual" {
            disabled = false;
        } else {
            enabled = false;
        }

        let val = bios.serial_console_baud_rate;
        message.push_str(&format!("serial_console_baud_rate={val} "));
        if &val != "BaudRate115200" {
            enabled = false;
        }

        let val = bios.serial_console_emulation;
        message.push_str(&format!("serial_console_emulation={val} "));
        if &val != "Vt100Plus" {
            enabled = false;
        }

        let val = bios.serial_console_port;
        message.push_str(&format!("serial_console_port={val} "));
        if &val != "Virtual" {
            enabled = false;
        }

        let val = bios.virtual_serial_port;
        message.push_str(&format!("virtual_serial_port={val} "));
        if &val != "Com1Irq4" {
            enabled = false;
        }
        */
        Ok(Status {
            message,
            status: match (enabled, disabled) {
                (true, _) => StatusInternal::Enabled,
                (_, true) => StatusInternal::Disabled,
                _ => StatusInternal::Partial,
            },
        })
    }

    /// Set this option as the first one in BootOrder.
    /// boot_ref should look like e.g. "Boot0028"
    async fn set_first_boot(&self, boot_ref: &str) -> Result<(), RedfishError> {
        let mut order = self.get_system().await?.boot.boot_order;
        let Some(source_pos) = order.iter().position(|bo| bo == boot_ref) else {
            return Err(RedfishError::MissingBootOption(format!(
                "BootOrder does not contain '{boot_ref}'"
            )));
        };
        order.swap(0, source_pos);

        let body = HashMap::from([("Boot", HashMap::from([("BootOrder", order)]))]);
        let url = format!("Systems/{}", self.s.system_id());
        self.s.client.patch(&url, body).await.map(|_status_code| ())
    }

    async fn get_expected_and_actual_first_boot_option(
        &self,
        boot_interface_mac: &str,
    ) -> Result<(Option<String>, Option<String>), RedfishError> {
        let mac = boot_interface_mac.to_string().to_uppercase();

        let all = self.get_boot_options().await?;
        let mut expected_first_boot_option = None;
        for b in all.members {
            let id = b.odata_id_get()?;
            let opt = self.get_boot_option(id).await?;
            let opt_name = opt.display_name.to_uppercase();
            if opt_name.contains("HTTP") && opt_name.contains("IPV4") && opt_name.contains(&mac) {
                expected_first_boot_option = Some(opt.boot_option_reference);
                break;
            }
        }

        let order = self.get_system().await?.boot.boot_order;
        let actual_first_boot_option = order.first().cloned();

        Ok((expected_first_boot_option, actual_first_boot_option))
    }

    // move hpe specific code here
    #[allow(dead_code)]
    pub async fn get_array_controller(
        &self,
        controller_id: u64,
    ) -> Result<storage::ArrayController, RedfishError> {
        let url = format!(
            "Systems/{}/SmartStorage/ArrayControllers/{}/",
            self.s.system_id(),
            controller_id
        );
        let (_status_code, body) = self.s.client.get(&url).await?;
        Ok(body)
    }

    #[allow(dead_code)]
    pub async fn get_array_controllers(&self) -> Result<storage::ArrayControllers, RedfishError> {
        let url = format!(
            "Systems/{}/SmartStorage/ArrayControllers/",
            self.s.system_id()
        );
        let (_status_code, body) = self.s.client.get(&url).await?;
        Ok(body)
    }

    /// Query the smart array status from the server
    #[allow(dead_code)]
    pub async fn get_smart_array_status(
        &self,
        controller_id: u64,
    ) -> Result<storage::SmartArray, RedfishError> {
        let url = format!(
            "Systems/{}/SmartStorage/ArrayControllers/{}/",
            self.s.system_id(),
            controller_id
        );
        let (_status_code, body) = self.s.client.get(&url).await?;
        Ok(body)
    }

    #[allow(dead_code)]
    pub async fn get_logical_drives(
        &self,
        controller_id: u64,
    ) -> Result<storage::LogicalDrives, RedfishError> {
        let url = format!(
            "Systems/{}/SmartStorage/ArrayControllers/{}/LogicalDrives/",
            self.s.system_id(),
            controller_id
        );
        let (_status_code, body) = self.s.client.get(&url).await?;
        Ok(body)
    }

    #[allow(dead_code)]
    pub async fn get_physical_drive(
        &self,
        drive_id: u64,
        controller_id: u64,
    ) -> Result<storage::DiskDrive, RedfishError> {
        let url = format!(
            "Systems/{}/SmartStorage/ArrayControllers/{}/DiskDrives/{}/",
            self.s.system_id(),
            controller_id,
            drive_id,
        );
        let (_status_code, body) = self.s.client.get(&url).await?;
        Ok(body)
    }

    #[allow(dead_code)]
    pub async fn get_physical_drives(
        &self,
        controller_id: u64,
    ) -> Result<storage::DiskDrives, RedfishError> {
        let url = format!(
            "Systems/{}/SmartStorage/ArrayControllers/{}/DiskDrives/",
            self.s.system_id(),
            controller_id
        );
        let (_status_code, body) = self.s.client.get(&url).await?;
        Ok(body)
    }

    #[allow(dead_code)]
    pub async fn get_storage_enclosures(
        &self,
        controller_id: u64,
    ) -> Result<storage::StorageEnclosures, RedfishError> {
        let url = format!(
            "Systems/{}/SmartStorage/ArrayControllers/{}/StorageEnclosures/",
            self.s.system_id(),
            controller_id
        );
        let (_status_code, body) = self.s.client.get(&url).await?;
        Ok(body)
    }

    #[allow(dead_code)]
    pub async fn get_storage_enclosure(
        &self,
        controller_id: u64,
        enclosure_id: u64,
    ) -> Result<storage::StorageEnclosure, RedfishError> {
        let url = format!(
            "Systems/{}/SmartStorage/ArrayControllers/{}/StorageEnclosures/{}/",
            self.s.system_id(),
            controller_id,
            enclosure_id,
        );
        let (_status_code, body) = self.s.client.get(&url).await?;
        Ok(body)
    }
}
