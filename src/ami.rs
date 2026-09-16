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

use crate::{
    jsonmap,
    model::{
        boot::{self, BootOverride, BootSourceOverrideEnabled, BootSourceOverrideMode},
        service_root::RedfishVendor,
        update_service::ComponentType,
        BootOption, ComputerSystem, ManagerResetType,
    },
    standard::RedfishStandard,
    BiosProfileType, Boot, EnabledDisabled, MachineSetupDiff, MachineSetupStatus, Redfish,
    RedfishError, Status, StatusInternal,
};

/// AMI uses BIOS attribute SETUP001 for Administrator Password (UEFI password)
const UEFI_PASSWORD_NAME: &str = "SETUP001";

/// LenovoGB300 has no "EndlessBoot" BIOS attribute; infinite
/// boot is expressed via the LEM0003 boot-retry count, where 50 is the
/// firmware's representation for endless retries.
const GB300_INFINITE_BOOT_RETRY: i64 = 50;

/// Build a lockdown `Status` from the fully-locked / fully-unlocked booleans,
/// defaulting to `Partial` when the readout is neither.
fn lockdown_status_from(message: String, is_locked: bool, is_unlocked: bool) -> Status {
    Status {
        message,
        status: if is_locked {
            StatusInternal::Enabled
        } else if is_unlocked {
            StatusInternal::Disabled
        } else {
            StatusInternal::Partial
        },
    }
}

fn is_target_or_network_boot_option(option: &BootOption, target_reference: &str) -> bool {
    option.boot_option_reference == target_reference
        || matches!(option.alias.as_deref(), Some("Pxe" | "UefiHttp"))
}

fn find_boot_option_for_mac<'a>(
    boot_options: &'a [BootOption],
    boot_interface_mac: &str,
) -> Option<&'a BootOption> {
    let mac = boot_interface_mac.to_uppercase();
    boot_options.iter().find(|option| {
        let display = option.display_name.to_uppercase();
        display.contains("HTTP") && display.contains("IPV4") && display.contains(&mac)
    })
}

fn compare_boot_order(
    vendor: Option<RedfishVendor>,
    boot_order: &[String],
    boot_options: &[BootOption],
    boot_interface_mac: &str,
) -> Vec<MachineSetupDiff> {
    let mut diffs = Vec::new();
    let target = find_boot_option_for_mac(boot_options, boot_interface_mac);
    let actual_first_reference = boot_order
        .first()
        .map(|entry| boot::boot_order_entry_reference(entry));

    let target_is_first = target.is_some_and(|option| {
        actual_first_reference == Some(option.boot_option_reference.as_str())
    });
    if !target_is_first {
        let expected = target
            .map(|option| option.display_name.clone())
            .unwrap_or_else(|| "Not found".to_string());
        let actual = actual_first_reference
            .and_then(|reference| {
                boot_options
                    .iter()
                    .find(|option| option.boot_option_reference == reference)
            })
            .map(|option| option.display_name.clone())
            .unwrap_or_else(|| "Not found".to_string());
        diffs.push(MachineSetupDiff {
            key: "boot_first".to_string(),
            expected,
            actual,
        });
    }

    if vendor != Some(RedfishVendor::LenovoGB300) {
        return diffs;
    }
    let Some(target) = target else {
        return diffs;
    };
    for option in boot_options
        .iter()
        .filter(|option| is_target_or_network_boot_option(option, &target.boot_option_reference))
    {
        let expected = option.boot_option_reference == target.boot_option_reference;
        if option.boot_option_enabled != Some(expected) {
            diffs.push(MachineSetupDiff {
                key: format!("BootOptions/{}/BootOptionEnabled", option.id),
                expected: expected.to_string(),
                actual: option
                    .boot_option_enabled
                    .map(|enabled| enabled.to_string())
                    .unwrap_or_else(|| "_missing_".to_string()),
            });
        }
    }

    diffs
}

pub struct Bmc {
    s: RedfishStandard,
}

impl Bmc {
    pub fn new(s: RedfishStandard) -> Result<Bmc, RedfishError> {
        Ok(Bmc { s })
    }

    /// Serial-console BIOS attributes as `(key, enabled_value, disabled_value)`.
    /// A `disabled_value` of "any" means any value counts as correctly disabled.
    ///
    /// LenovoGB300's BIOS registry mostly prefixes enum values with the
    /// attribute id, but irregularly: TER001/TER010 stay bare, the port is COM0
    /// not COM1, and the hyphen in VT-UTF8 is dropped. So both forms are listed
    /// explicitly per attribute rather than derived via a prefix rule.
    fn serial_console_attrs(&self) -> Vec<(&'static str, &'static str, &'static str)> {
        let gb300 = self.s.vendor == Some(RedfishVendor::LenovoGB300);
        // (key, generic_enabled, gb300_enabled, disabled)
        const ATTRS: &[(&str, &str, &str, &str)] = &[
            ("TER001", "Enabled", "Enabled", "Disabled"), // Console Redirection
            ("TER010", "Enabled", "Enabled", "Disabled"), // Console Redirection EMS
            ("TER06B", "COM1", "TER06BCOM0", "any"),      // Out-of-Band Mgmt Port
            ("TER0021", "115200", "TER0021115200", "any"), // Bits per second
            ("TER0020", "115200", "TER0020115200", "any"), // Bits per second EMS
            ("TER012", "VT100Plus", "TER012VT100Plus", "any"), // Terminal Type
            ("TER011", "VT-UTF8", "TER011VTUTF8", "any"), // Terminal Type EMS
            ("TER05D", "None", "TER05DNone", "any"),      // Flow Control
        ];
        ATTRS
            .iter()
            .map(|&(key, generic, gb300_val, disabled)| {
                (key, if gb300 { gb300_val } else { generic }, disabled)
            })
            .collect()
    }

    /// LenovoGB300 lockdown status: USB support (attribute-id prefixed enum,
    /// e.g. "USB000Disabled") plus the host interface. There is no KCS BIOS
    /// attribute on this platform, so it is not part of the status.
    async fn lockdown_status_gb300(&self) -> Result<Status, RedfishError> {
        let bios = self.s.bios().await?;
        let url = format!("Systems/{}/Bios", self.s.system_id());
        let attrs = jsonmap::get_object(&bios, "Attributes", &url)?;
        let usb000 = jsonmap::get_str(attrs, "USB000", "Bios Attributes")?;

        let hi_url = format!("Managers/{}/HostInterfaces/Self", self.s.manager_id());
        let (_status, hi): (_, serde_json::Value) = self.s.client.get(&hi_url).await?;
        let hi_enabled = hi
            .get("InterfaceEnabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let message = format!("usb_support={usb000}, host_interface={hi_enabled}");

        let is_locked = usb000 == "USB000Disabled" && !hi_enabled;
        let is_unlocked = usb000 == "USB000Enabled" && hi_enabled;

        Ok(lockdown_status_from(message, is_locked, is_unlocked))
    }

    /// LenovoAMI-specific lockdown status via OEM ConfigBMC endpoint.
    async fn lockdown_status_lenovo_ami(&self) -> Result<Status, RedfishError> {
        const LOCKDOWN_FIELDS: &[&str] = &[
            "LockoutHostControl",
            "LockoutBiosVariableWriteMode",
            "LockdownBiosSettingsChange",
            "LockdownBiosUpgradeDowngrade",
        ];

        let config_bmc_url = format!("Managers/{}/Oem/ConfigBMC", self.s.manager_id());
        let (_status, body): (_, serde_json::Value) = self.s.client.get(&config_bmc_url).await?;

        let values: Vec<&str> = LOCKDOWN_FIELDS
            .iter()
            .map(|key| body.get(key).and_then(|v| v.as_str()).unwrap_or("unknown"))
            .collect();

        let message = LOCKDOWN_FIELDS
            .iter()
            .zip(&values)
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");

        let is_locked = values.iter().all(|&v| v == "Enable");
        let is_unlocked = values.iter().all(|&v| v == "Disable");

        Ok(lockdown_status_from(message, is_locked, is_unlocked))
    }
}
impl Redfish for Bmc {
    fn std_redfish(&self) -> &RedfishStandard {
        &self.s
    }
    /// AMI BMC requires If-Match header for password changes
    fn change_password_by_id<'a>(
        &'a self,
        account_id: &'a str,
        new_pass: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("AccountService/Accounts/{}", account_id);
            let mut data = HashMap::new();
            data.insert("Password", new_pass);
            self.s.client.patch_with_if_match(&url, data).await
        })
    }

    /// AMI BMC requires If-Match header for secure boot changes
    fn disable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut data = HashMap::new();
            data.insert("SecureBootEnable", false);
            let url = format!("Systems/{}/SecureBoot", self.s.system_id());
            self.s.client.patch_with_if_match(&url, data).await
        })
    }

    /// AMI BMC requires If-Match header for secure boot changes
    fn enable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut data = HashMap::new();
            data.insert("SecureBootEnable", true);
            let url = format!("Systems/{}/SecureBoot", self.s.system_id());
            self.s.client.patch_with_if_match(&url, data).await
        })
    }

    /// AMI BMCs are assumed to only accept `ForceRestart` for `Manager.Reset`
    /// (per the AMI support work, #34). `None` defaults to `ForceRestart`; an
    /// explicit non-`ForceRestart` `reset_type` is rejected rather than
    /// silently downgraded, so callers learn their choice was not honored.
    fn bmc_reset<'a>(
        &'a self,
        reset_type: Option<ManagerResetType>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            match reset_type {
                None | Some(ManagerResetType::ForceRestart) => {
                    self.s
                        .reset_manager(ManagerResetType::ForceRestart, None)
                        .await
                }
                Some(other) => Err(RedfishError::NotSupported(format!(
                    "AMI BMC only supports ForceRestart for Manager.Reset, but {other} was requested"
                ))),
            }
        })
    }

    /// Machine setup for AMI BMC.
    ///
    /// Sets up:
    /// 1. Serial console
    /// 2. Clears TPM
    /// 3. BIOS settings
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
            let attrs = self.machine_setup_attrs();
            self.set_bios(attrs).await?;
            Ok(None)
        })
    }

    /// Check machine setup status for AMI BMC.
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

            let mut diffs = self.diff_bios_bmc_attr().await?;

            if let Some(mac) = boot_interface_mac {
                diffs.extend(self.boot_order_diffs(mac).await?);
            }

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

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let diffs = self.diff_bios_bmc_attr().await?;
            Ok(diffs.is_empty())
        })
    }

    /// AMI BMC requires If-Match header for password policy changes
    fn set_machine_password_policy<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            use serde_json::Value;
            let body = HashMap::from([
                ("AccountLockoutThreshold", Value::Number(0.into())),
                ("AccountLockoutDuration", Value::Number(0.into())),
                ("AccountLockoutCounterResetAfter", Value::Number(0.into())),
            ]);
            self.s
                .client
                .patch_with_if_match("AccountService", body)
                .await
        })
    }

    /// AMI lockdown - controls KCS access, USB support, and Host Interface.
    /// On LenovoAMI, uses the OEM ConfigBMC endpoint to control host lockout,
    /// BIOS variable write, BIOS settings change, and BIOS upgrade/downgrade.
    fn lockdown<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            use EnabledDisabled::*;
            if self.s.vendor == Some(RedfishVendor::LenovoAMI) {
                let value = match target {
                    Enabled => "Enable",
                    Disabled => "Disable",
                };
                let body = HashMap::from([
                    ("LockoutHostControl", value),
                    ("LockoutBiosVariableWriteMode", value),
                    ("LockdownBiosSettingsChange", value),
                    ("LockdownBiosUpgradeDowngrade", value),
                ]);
                let config_bmc_url = format!("Managers/{}/Oem/ConfigBMC", self.s.manager_id());
                return self.s.client.post(&config_bmc_url, body).await.map(|_| ());
            }

            // LenovoGB300 has neither the OEM ConfigBMC endpoint nor
            // the generic AMI `KCSACP` BIOS attribute, and its USB enum values
            // are attribute-id prefixed (e.g. "USB000Disabled").
            let hi_enabled = target == Disabled;
            let bios_attrs = if self.s.vendor == Some(RedfishVendor::LenovoGB300) {
                let usb = match target {
                    Enabled => "USB000Disabled",
                    Disabled => "USB000Enabled",
                };
                HashMap::from([("USB000".to_string(), usb.into())])
            } else {
                let (kcsacp, usb) = match target {
                    Enabled => ("Deny All", "Disabled"),
                    Disabled => ("Allow All", "Enabled"),
                };
                HashMap::from([
                    ("KCSACP".to_string(), kcsacp.into()),
                    ("USB000".to_string(), usb.into()),
                ])
            };
            self.set_bios(bios_attrs).await?;

            let hi_url = format!("Managers/{}/HostInterfaces/Self", self.s.manager_id());
            let hi_body = HashMap::from([("InterfaceEnabled", hi_enabled)]);
            self.s.client.patch_with_if_match(&hi_url, hi_body).await
        })
    }

    /// AMI lockdown status - checks KCS access, USB support, and Host Interface.
    /// On LenovoAMI, reads the OEM ConfigBMC endpoint instead. On LenovoGB300,
    /// checks USB support and the host interface (no KCS/ConfigBMC there).
    fn lockdown_status<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            if self.s.vendor == Some(RedfishVendor::LenovoAMI) {
                return self.lockdown_status_lenovo_ami().await;
            }

            if self.s.vendor == Some(RedfishVendor::LenovoGB300) {
                return self.lockdown_status_gb300().await;
            }

            let bios = self.s.bios().await?;
            let url = format!("Systems/{}/Bios", self.s.system_id());
            let attrs = jsonmap::get_object(&bios, "Attributes", &url)?;
            let kcsacp = jsonmap::get_str(attrs, "KCSACP", "Bios Attributes")?;
            let usb000 = jsonmap::get_str(attrs, "USB000", "Bios Attributes")?;

            let hi_url = format!("Managers/{}/HostInterfaces/Self", self.s.manager_id());
            let (_status, hi): (_, serde_json::Value) = self.s.client.get(&hi_url).await?;
            let hi_enabled = hi
                .get("InterfaceEnabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let message = format!(
                "kcs_access={}, usb_support={}, host_interface={}",
                kcsacp, usb000, hi_enabled
            );

            let is_locked = kcsacp == "Deny All" && usb000 == "Disabled" && !hi_enabled;
            let is_unlocked = kcsacp == "Allow All" && usb000 == "Enabled" && hi_enabled;

            Ok(lockdown_status_from(message, is_locked, is_unlocked))
        })
    }

    /// Setup serial console for AMI BMC via BIOS attributes.
    fn setup_serial_console<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let attributes: HashMap<String, serde_json::Value> = self
                .serial_console_attrs()
                .into_iter()
                .map(|(key, enabled, _)| (key.to_string(), enabled.into()))
                .collect();

            self.set_bios(attributes).await
        })
    }

    /// Check serial console status for AMI BMC.
    fn serial_console_status<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            let bios = self.bios().await?;
            let url = format!("Systems/{}/Bios", self.s.system_id());
            let attrs = jsonmap::get_object(&bios, "Attributes", &url)?;

            let expected = self.serial_console_attrs();

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

    fn boot_first<'a>(
        &'a self,
        target: Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let alias = match target {
                Boot::Pxe => "Pxe",
                Boot::HardDisk => "Hdd",
                Boot::UefiHttp => "UefiHttp",
            };
            self.set_boot_order(alias).await
        })
    }

    /// AMI requires patching `/Systems/{id}` (NOT `/SD`) with an `If-Match` header.
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
            // AMI BMCs default to UEFI mode when the caller doesn't specify one.
            let mode = settings.mode.unwrap_or(BootSourceOverrideMode::UEFI);
            boot_data.insert(
                "BootSourceOverrideMode".to_string(),
                mode.to_string().into(),
            );
            if let Some(uri) = settings.http_boot_uri {
                boot_data.insert("HttpBootUri".to_string(), uri.into());
            }
            let url = format!("Systems/{}", self.s.system_id());
            self.s
                .client
                .patch_with_if_match(&url, HashMap::from([("Boot", boot_data)]))
                .await?;
            Ok(None)
        })
    }

    /// AMI BMC requires If-Match header for boot order changes
    fn change_boot_order<'a>(
        &'a self,
        boot_array: Vec<String>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let body = HashMap::from([("Boot", HashMap::from([("BootOrder", boot_array)]))]);
            let url = format!("Systems/{}/SD", self.s.system_id());
            self.s.client.patch_with_if_match(&url, body).await
        })
    }

    fn clear_tpm<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // GB300's Grace BIOS registry prefixes the TCG006 enum value with
            // the attribute id ("TCG006TPMClear"); other AMI platforms use the
            // bare "TPM Clear".
            let clear_value = if self.s.vendor == Some(RedfishVendor::LenovoGB300) {
                "TCG006TPMClear"
            } else {
                "TPM Clear"
            };
            self.set_bios(HashMap::from([("TCG006".to_string(), clear_value.into())]))
                .await
        })
    }

    fn update_firmware_multipart<'a>(
        &'a self,
        filename: &'a Path,
        reboot: bool,
        timeout: Duration,
        component_type: ComponentType,
    ) -> crate::RedfishFuture<'a, Result<String, RedfishError>> {
        Box::pin(async move {
            // The hardcoded URIs and keys below are only verified for the Lenovo HS350x, so
            // restrict this native path to LenovoAMI; other AMI platforms fall back to NotSupported.
            if self.s.vendor != Some(RedfishVendor::LenovoAMI) {
                return self
                    .s
                    .update_firmware_multipart(filename, reboot, timeout, component_type)
                    .await;
            }

            let (oem, targets) = ami_update_targets(&component_type)?;

            // BMC images preserve all BMC configuration via a pre-upload PATCH; BIOS preservation
            // is expressed inside OemParameters (PreserveBIOS) instead.
            if matches!(component_type, ComponentType::BMC) {
                self.set_preserve_configuration_all().await?;
            }

            let file = tokio::fs::File::open(filename)
                .await
                .map_err(|e| RedfishError::FileError(format!("Could not open file: {}", e)))?;

            let update_parameters = serde_json::to_string(&AmiUpdateParameters { targets })
                .map_err(|e| RedfishError::JsonSerializeError {
                    url: "UpdateService/upload".to_string(),
                    object_debug: "AmiUpdateParameters".to_string(),
                    source: e,
                })?;
            let oem_parameters =
                serde_json::to_string(&oem).map_err(|e| RedfishError::JsonSerializeError {
                    url: "UpdateService/upload".to_string(),
                    object_debug: "AmiOemParameters".to_string(),
                    source: e,
                })?;

            let (_status_code, loc, body) = self
                .s
                .client
                .req_update_firmware_multipart_with_oem(
                    filename,
                    file,
                    update_parameters,
                    Some(oem_parameters),
                    "UpdateService/upload", // AMI MegaRAC does "upload" instead of "MultiPartUpload"
                    false,
                    timeout,
                )
                .await?;

            extract_ami_task_id(loc.as_deref(), &body).ok_or_else(|| RedfishError::GenericError {
                error: format!("Could not locate AMI update task in response: {}", body),
            })
        })
    }

    /// AMI BMC requires If-Match header for BIOS changes
    fn set_bios<'a>(
        &'a self,
        values: HashMap<String, serde_json::Value>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/SD", self.s.system_id());
            let body = HashMap::from([("Attributes", values)]);
            self.s.client.patch_with_if_match(&url, body).await
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.factory_reset_bios().await })
    }

    /// AMI uses /Bios/SD for pending settings
    fn pending<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/SD", self.s.system_id());
            self.s.pending_with_url(&url).await
        })
    }

    /// AMI clear_pending - uses /Bios/SD instead of /Bios/Settings
    fn clear_pending<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let pending_url = format!("Systems/{}/Bios/SD", self.s.system_id());
            let pending_attrs = self.s.pending_attributes(&pending_url).await?;
            let current_attrs = self.s.bios_attributes().await?;

            let reset_attrs: HashMap<_, _> = pending_attrs
                .iter()
                .filter(|(k, v)| current_attrs.get(*k) != Some(v))
                .map(|(k, _)| (k.clone(), current_attrs.get(k).cloned()))
                .collect();

            if reset_attrs.is_empty() {
                return Ok(());
            }

            let body = HashMap::from([("Attributes", reset_attrs)]);
            self.s.client.patch_with_if_match(&pending_url, body).await
        })
    }

    /// AMI uses BIOS attribute SETUP001 for Administrator Password
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

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    /// Set the DPU as the first boot option.
    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mac = crate::resolve_boot_interface_mac(self, boot_interface)
                .await?
                .to_uppercase();
            let (system, all_boot_options) = self.get_system_and_boot_options().await?;

            let target = find_boot_option_for_mac(&all_boot_options, &mac);

            let Some(target) = target else {
                let all_names: Vec<_> = all_boot_options
                    .iter()
                    .map(|b| format!("{}: {}", b.id, b.display_name))
                    .collect();
                return Err(RedfishError::MissingBootOption(format!(
                    "No HTTP IPv4 boot option found for MAC {mac}; available: {:#?}",
                    all_names
                )));
            };

            let target_id = target.boot_option_reference.clone();
            let mut boot_order = system.boot.boot_order;

            let boot_order_is_set = boot_order
                .first()
                .is_some_and(|entry| boot::boot_order_entry_reference(entry) == target_id);
            if boot_order_is_set {
                tracing::info!("NO-OP: DPU ({mac}) is already first in boot order ({target_id})");
            } else {
                if !boot::promote_boot_order_entry_first(&mut boot_order, &target_id) {
                    boot_order.insert(0, target_id.clone());
                }
                self.change_boot_order(boot_order).await?;
            }

            if self.s.vendor == Some(RedfishVendor::LenovoGB300) {
                for option in all_boot_options
                    .iter()
                    .filter(|option| is_target_or_network_boot_option(option, &target_id))
                {
                    let should_be_enabled = option.boot_option_reference == target_id;
                    if option.boot_option_enabled != Some(should_be_enabled) {
                        let url = format!(
                            "Systems/{}/BootOptions/{}/SD",
                            self.s.system_id(),
                            option.id
                        );
                        self.s
                            .client
                            .patch_with_if_match(
                                &url,
                                HashMap::from([("BootOptionEnabled", should_be_enabled)]),
                            )
                            .await?;
                    }
                }
            }

            Ok(None)
        })
    }

    /// Check if boot order is setup correctly
    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let mac = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            Ok(self.boot_order_diffs(&mac).await?.is_empty())
        })
    }

    /// AMI lockdown_bmc - BMC-only lockdown (Host Interface only)
    fn lockdown_bmc<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let interface_enabled = target == EnabledDisabled::Disabled;
            let hi_body = HashMap::from([("InterfaceEnabled", interface_enabled)]);
            let hi_url = format!("Managers/{}/HostInterfaces/Self", self.s.manager_id());
            self.s.client.patch_with_if_match(&hi_url, hi_body).await
        })
    }

    /// AMI BMC requires If-Match header for network protocol changes
    fn enable_ipmi_over_lan<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Managers/{}/NetworkProtocol", self.s.manager_id());
            let ipmi_data = HashMap::from([("ProtocolEnabled", target.is_enabled())]);
            let data = HashMap::from([("IPMI", ipmi_data)]);
            self.s.client.patch_with_if_match(&url, data).await
        })
    }

    /// AMI clear_nvram - sets RECV000 (Reset NVRAM) to "Enabled"
    fn clear_nvram<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // The GB300 Grace BIOS registry has no RECV000 (Reset NVRAM)
            // attribute, so there is no equivalent knob to set.
            if self.s.vendor == Some(RedfishVendor::LenovoGB300) {
                return Err(RedfishError::NotSupported(
                    "clear_nvram: no RECV000 BIOS attribute on LenovoGB300".to_string(),
                ));
            }
            self.set_bios(HashMap::from([("RECV000".to_string(), "Enabled".into())]))
                .await
        })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if self.s.vendor == Some(RedfishVendor::LenovoGB300) {
                return self
                    .set_bios(HashMap::from([(
                        "LEM0003".to_string(),
                        GB300_INFINITE_BOOT_RETRY.into(),
                    )]))
                    .await;
            }
            self.set_bios(HashMap::from([(
                "EndlessBoot".to_string(),
                "Enabled".into(),
            )]))
            .await
        })
    }

    fn is_infinite_boot_enabled<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        Box::pin(async move {
            let bios = self.s.bios().await?;
            let url = format!("Systems/{}/Bios", self.s.system_id());
            let attrs = jsonmap::get_object(&bios, "Attributes", &url)?;
            if self.s.vendor == Some(RedfishVendor::LenovoGB300) {
                // LEM0003 may be reported as a JSON number or a numeric string.
                let Some(value) = attrs.get("LEM0003") else {
                    return Ok(None);
                };
                let retry = value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()));
                return Ok(retry.map(|r| r == GB300_INFINITE_BOOT_RETRY));
            }
            let endless_boot = jsonmap::get_str(attrs, "EndlessBoot", "Bios Attributes")?;
            Ok(Some(endless_boot == "Enabled"))
        })
    }

    /// AMI doesn't support AC power cycle through standard power action
    fn ac_powercycle_supported_by_power(&self) -> bool {
        false
    }

    fn set_ntp_servers<'a>(
        &'a self,
        servers: &'a [String],
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let servers = &servers[..servers.len().min(2)];
            self.s.set_manager_ntp_servers(servers).await
        })
    }
}

impl Bmc {
    /// Enable preservation of all BMC configuration before a BMC firmware flash. PATCHes
    /// `UpdateService` with `If-Match: *` setting every `Oem.AMIUpdateService.PreserveConfiguration`
    /// key to `true`. A non-2xx response is an error so we never silently flash without preserving
    /// config.
    async fn set_preserve_configuration_all(&self) -> Result<(), RedfishError> {
        let preserve: HashMap<&str, bool> = AMI_PRESERVE_CONFIGURATION_KEYS
            .iter()
            .map(|key| (*key, true))
            .collect();
        let body = serde_json::json!({
            "Oem": {
                "AMIUpdateService": {
                    "PreserveConfiguration": preserve,
                }
            }
        });
        self.s
            .client
            .patch_with_if_match("UpdateService", body)
            .await
    }

    async fn get_system_and_boot_options(
        &self,
    ) -> Result<(ComputerSystem, Vec<BootOption>), RedfishError> {
        let system = self.get_system().await?;
        let boot_options_id =
            system
                .boot
                .boot_options
                .clone()
                .ok_or_else(|| RedfishError::MissingKey {
                    key: "boot.boot_options".to_string(),
                    url: system.odata.odata_id.clone(),
                })?;
        let all_boot_options: Vec<BootOption> = self
            .get_collection(boot_options_id)
            .await
            .and_then(|c| c.try_get::<BootOption>())?
            .members;
        Ok((system, all_boot_options))
    }

    async fn boot_order_diffs(
        &self,
        boot_interface_mac: &str,
    ) -> Result<Vec<MachineSetupDiff>, RedfishError> {
        let (system, boot_options) = self.get_system_and_boot_options().await?;
        Ok(compare_boot_order(
            self.s.vendor,
            &system.boot.boot_order,
            &boot_options,
            boot_interface_mac,
        ))
    }

    /// Finds the first boot option matching the given alias and moves it to the front
    /// of the boot order.
    async fn set_boot_order(&self, alias: &str) -> Result<(), RedfishError> {
        let (system, all_boot_options) = self.get_system_and_boot_options().await?;

        let target = all_boot_options
            .iter()
            .find(|opt| opt.alias.as_deref() == Some(alias));

        let target_ref = target
            .ok_or_else(|| {
                let all_names: Vec<_> = all_boot_options
                    .iter()
                    .map(|b| {
                        format!(
                            "{}: {} (alias={})",
                            b.boot_option_reference,
                            b.display_name,
                            b.alias.as_deref().unwrap_or("none")
                        )
                    })
                    .collect();
                RedfishError::MissingBootOption(format!(
                    "No boot option with alias {:?} found; available: {:#?}",
                    alias, all_names
                ))
            })?
            .boot_option_reference
            .clone();

        let mut boot_order = system.boot.boot_order;

        if boot_order
            .first()
            .is_some_and(|entry| boot::boot_order_entry_reference(entry) == target_ref)
        {
            return Ok(());
        }

        if !boot::promote_boot_order_entry_first(&mut boot_order, &target_ref) {
            boot_order.insert(0, target_ref);
        }
        self.change_boot_order(boot_order).await
    }

    /// Get the BIOS attributes for machine setup.
    fn machine_setup_attrs(&self) -> HashMap<String, serde_json::Value> {
        // The LenovoGB300 (Grace-based) uses a distinct BIOS registry: enum
        // values are prefixed with the attribute ID (e.g. "PCIS007Enabled"),
        // there is no Intel VMX knob (VMXEN) or boot-mode selector (FBO001),
        // and "Infinite Boot" is expressed via the LEM0003 retry count (50 =
        // endless boot) instead of the "EndlessBoot" attribute.
        if self.s.vendor == Some(RedfishVendor::LenovoGB300) {
            return HashMap::from([
                ("PCIS007".to_string(), "PCIS007Enabled".into()), // SR-IOV Support
                ("LEM0001".to_string(), 0.into()),                // PXE retry count
                ("NWSK000".to_string(), "NWSK000Enabled".into()), // Network Stack
                ("NWSK001".to_string(), "NWSK001Disabled".into()), // IPv4 PXE Support
                ("NWSK006".to_string(), "NWSK006Enabled".into()), // IPv4 HTTP Support
                ("NWSK002".to_string(), "NWSK002Disabled".into()), // IPv6 PXE Support
                ("NWSK007".to_string(), "NWSK007Disabled".into()), // IPv6 HTTP Support
                ("LEM0003".to_string(), GB300_INFINITE_BOOT_RETRY.into()), // Infinite Boot
            ]);
        }

        HashMap::from([
            ("VMXEN".to_string(), "Enable".into()), // VMX (Intel Virtualization)
            ("PCIS007".to_string(), "Enabled".into()), // SR-IOV Support
            ("LEM0001".to_string(), 3.into()),      // PXE retry count (remove on future FW update)
            ("NWSK000".to_string(), "Enabled".into()), // Network Stack
            ("NWSK001".to_string(), "Disabled".into()), // IPv4 PXE Support
            ("NWSK006".to_string(), "Enabled".into()), // IPv4 HTTP Support
            ("NWSK002".to_string(), "Disabled".into()), // IPv6 PXE Support
            ("NWSK007".to_string(), "Disabled".into()), // IPv6 HTTP Support
            ("FBO001".to_string(), "UEFI".into()),  // Boot Mode Select
            ("EndlessBoot".to_string(), "Enabled".into()), // Infinite Boot
        ])
    }

    /// Check BIOS/BMC attributes against expected values for machine setup status.
    async fn diff_bios_bmc_attr(&self) -> Result<Vec<MachineSetupDiff>, RedfishError> {
        let mut diffs = vec![];

        // Check serial console status
        let sc = self.serial_console_status().await?;
        if !sc.is_fully_enabled() {
            diffs.push(MachineSetupDiff {
                key: "serial_console".to_string(),
                expected: "Enabled".to_string(),
                actual: sc.status.to_string(),
            });
        }

        // Check BIOS attributes
        let bios = self.s.bios_attributes().await?;
        let expected_attrs = self.machine_setup_attrs();

        for (key, expected) in expected_attrs {
            let Some(actual) = bios.get(&key) else {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    expected: expected.to_string(),
                    actual: "_missing_".to_string(),
                });
                continue;
            };
            let act = actual.as_str().unwrap_or(&actual.to_string()).to_string();
            let exp = expected
                .as_str()
                .unwrap_or(&expected.to_string())
                .to_string();
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
}

/// FirmwareInventory collection prefix for AMI update `Targets` URIs (absolute).
const AMI_FIRMWARE_INVENTORY: &str = "/redfish/v1/UpdateService/FirmwareInventory";

/// All `Oem.AMIUpdateService.PreserveConfiguration` keys set to `true` for a BMC flash.
const AMI_PRESERVE_CONFIGURATION_KEYS: [&str; 14] = [
    "Authentication",
    "EXTLOG",
    "FRU",
    "IPMI",
    "KVM",
    "NTP",
    "Network",
    "REDFISH",
    "SDR",
    "SEL",
    "SNMP",
    "SSH",
    "Syslog",
    "WEB",
];

/// `UpdateParameters` JSON part for an AMI multipart upload. Unlike the standard Redfish path, AMI
/// names explicit `FirmwareInventory` targets and does not use `@Redfish.OperationApplyTime`.
#[derive(serde::Serialize)]
#[serde(rename_all = "PascalCase")]
struct AmiUpdateParameters {
    targets: Vec<String>,
}

/// `OemParameters` JSON part for an AMI multipart upload. `image_type` is "BMC" or "BIOS";
/// `preserve_bios` carries BIOS NVRAM preservation ("true"/"false") and is omitted for BMC.
#[derive(serde::Serialize)]
#[serde(rename_all = "PascalCase")]
struct AmiOemParameters {
    image_type: String,
    // PascalCase would produce "PreserveBios"; the AMI MegaRAC API expects BIOS
    // fully capitalized, so rename explicitly.
    #[serde(rename = "PreserveBIOS", skip_serializing_if = "Option::is_none")]
    preserve_bios: Option<String>,
}

/// Map a `ComponentType` to the AMI (`OemParameters`, `Targets`) pair using the HS350x's exact
/// URIs. BMC flashes both ROM banks; BIOS flashes `BIOSImage1` with NVRAM preservation. ROM-bank
/// selection is not expressible via `ComponentType`, so it is hardcoded.
fn ami_update_targets(
    component: &ComponentType,
) -> Result<(AmiOemParameters, Vec<String>), RedfishError> {
    match component {
        ComponentType::BMC => Ok((
            AmiOemParameters {
                image_type: "BMC".to_string(),
                preserve_bios: None,
            },
            vec![
                format!("{AMI_FIRMWARE_INVENTORY}/BMCImage1"),
                format!("{AMI_FIRMWARE_INVENTORY}/BMCImage2"),
            ],
        )),
        ComponentType::UEFI => Ok((
            AmiOemParameters {
                image_type: "BIOS".to_string(),
                preserve_bios: Some("true".to_string()),
            },
            vec![format!("{AMI_FIRMWARE_INVENTORY}/BIOSImage1")],
        )),
        other => Err(RedfishError::NotSupported(format!(
            "AMI multipart update not implemented for component type {:?}",
            other
        ))),
    }
}

/// Extract the task id from an AMI multipart upload response. AMI returns the task reference in
/// `Messages[]` where `MessageId == "Task.1.0.New"` and `MessageArgs[0]` is the task URI; we return
/// its last path segment because `get_task` formats `TaskService/Tasks/{id}`. Falls back to a
/// top-level `Id` (some firmware returns a Task object) and then the `Location` header.
fn extract_ami_task_id(location: Option<&str>, body: &str) -> Option<String> {
    let last_segment = |s: &str| {
        s.trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(str::to_string)
    };

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(messages) = value.get("Messages").and_then(|m| m.as_array()) {
            for message in messages {
                if message.get("MessageId").and_then(|x| x.as_str()) == Some("Task.1.0.New") {
                    if let Some(arg) = message
                        .get("MessageArgs")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.first())
                        .and_then(|x| x.as_str())
                    {
                        return last_segment(arg);
                    }
                }
            }
        }
        if let Some(id) = value.get("Id").and_then(|x| x.as_str()) {
            return Some(id.to_string());
        }
    }

    location.and_then(last_segment)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT_INTERFACE_MAC: &str = "B8:E9:24:17:6D:72";
    const TARGET_DISPLAY_NAME: &str =
        "[Slot2]UEFI: HTTP IPv4 Nvidia Network Adapter - B8:E9:24:17:6D:72 P1";

    fn boot_option(
        reference: &str,
        alias: &str,
        display_name: &str,
        enabled: Option<bool>,
    ) -> BootOption {
        BootOption {
            odata: Default::default(),
            alias: Some(alias.to_string()),
            description: None,
            boot_option_enabled: enabled,
            boot_option_reference: reference.to_string(),
            display_name: display_name.to_string(),
            id: reference.to_string(),
            name: display_name.to_string(),
            uefi_device_path: None,
        }
    }

    fn boot_options(
        target_enabled: Option<bool>,
        other_network_enabled: Option<bool>,
        disk_enabled: Option<bool>,
    ) -> Vec<BootOption> {
        vec![
            boot_option("Boot0001", "UefiHttp", TARGET_DISPLAY_NAME, target_enabled),
            boot_option(
                "Boot0002",
                "Pxe",
                "UEFI PXEv4 Nvidia Network Adapter - 9C:6B:00:11:22:33",
                other_network_enabled,
            ),
            boot_option("Boot0003", "Hdd", "UEFI Disk", disk_enabled),
        ]
    }

    fn boot_order(references: &[&str]) -> Vec<String> {
        references
            .iter()
            .map(|reference| reference.to_string())
            .collect()
    }

    fn gb300_boot_order_diffs(
        boot_order: &[String],
        boot_options: &[BootOption],
    ) -> Vec<MachineSetupDiff> {
        compare_boot_order(
            Some(RedfishVendor::LenovoGB300),
            boot_order,
            boot_options,
            BOOT_INTERFACE_MAC,
        )
    }

    fn assert_diff(
        diff: &MachineSetupDiff,
        expected_key: &str,
        expected_value: &str,
        actual_value: &str,
    ) {
        assert_eq!(diff.key, expected_key);
        assert_eq!(diff.expected, expected_value);
        assert_eq!(diff.actual, actual_value);
    }

    #[test]
    fn gb300_boot_order_accepts_complete_configuration() {
        let options = boot_options(Some(true), Some(false), Some(true));
        let diffs = gb300_boot_order_diffs(&boot_order(&["Boot0001"]), &options);

        assert!(diffs.is_empty());
    }

    #[test]
    fn gb300_boot_order_accepts_decorated_boot_order_reference() {
        let options = boot_options(Some(true), Some(false), Some(true));
        let diffs = gb300_boot_order_diffs(
            &boot_order(&["Boot0001: Selected network adapter"]),
            &options,
        );

        assert!(diffs.is_empty());
    }

    #[test]
    fn gb300_boot_order_reports_disabled_selected_option() {
        let options = boot_options(Some(false), Some(false), Some(true));
        let diffs = gb300_boot_order_diffs(&boot_order(&["Boot0001"]), &options);

        assert_eq!(diffs.len(), 1);
        assert_diff(
            &diffs[0],
            "BootOptions/Boot0001/BootOptionEnabled",
            "true",
            "false",
        );
    }

    #[test]
    fn gb300_boot_order_reports_disabled_selected_option_without_alias() {
        let mut options = boot_options(Some(false), Some(false), Some(true));
        options[0].alias = None;
        let diffs = gb300_boot_order_diffs(&boot_order(&["Boot0001"]), &options);

        assert_eq!(diffs.len(), 1);
        assert_diff(
            &diffs[0],
            "BootOptions/Boot0001/BootOptionEnabled",
            "true",
            "false",
        );
    }

    #[test]
    fn gb300_boot_order_reports_enabled_competing_network_option() {
        let options = boot_options(Some(true), Some(true), Some(true));
        let diffs = gb300_boot_order_diffs(&boot_order(&["Boot0001"]), &options);

        assert_eq!(diffs.len(), 1);
        assert_diff(
            &diffs[0],
            "BootOptions/Boot0002/BootOptionEnabled",
            "false",
            "true",
        );
    }

    #[test]
    fn gb300_boot_order_reports_missing_enablement_values() {
        for (target_enabled, other_network_enabled, expected_reference, expected_value) in [
            (None, Some(false), "Boot0001", "true"),
            (Some(true), None, "Boot0002", "false"),
        ] {
            let options = boot_options(target_enabled, other_network_enabled, Some(true));
            let diffs = gb300_boot_order_diffs(&boot_order(&["Boot0001"]), &options);

            assert_eq!(diffs.len(), 1);
            assert_diff(
                &diffs[0],
                &format!("BootOptions/{expected_reference}/BootOptionEnabled"),
                expected_value,
                "_missing_",
            );
        }
    }

    #[test]
    fn gb300_boot_order_reports_target_not_first() {
        let options = boot_options(Some(true), Some(false), Some(true));
        let diffs = gb300_boot_order_diffs(&boot_order(&["Boot0003", "Boot0001"]), &options);

        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].key, "boot_first");
        assert_eq!(diffs[0].expected, TARGET_DISPLAY_NAME);
        assert_eq!(diffs[0].actual, "UEFI Disk");
    }

    #[test]
    fn gb300_boot_order_compares_boot_option_references() {
        let mut options = boot_options(Some(true), Some(false), Some(true));
        options[1].display_name = TARGET_DISPLAY_NAME.to_string();
        let diffs = gb300_boot_order_diffs(&boot_order(&["Boot0002", "Boot0001"]), &options);

        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].key, "boot_first");
    }

    #[test]
    fn gb300_boot_order_ignores_non_network_enablement() {
        let options = boot_options(Some(true), Some(false), None);
        let diffs = gb300_boot_order_diffs(&boot_order(&["Boot0001"]), &options);

        assert!(diffs.is_empty());
    }

    #[test]
    fn other_ami_vendors_ignore_network_option_enablement() {
        let options = boot_options(Some(false), Some(true), Some(true));
        let order = boot_order(&["Boot0001"]);

        for vendor in [RedfishVendor::AMI, RedfishVendor::LenovoAMI] {
            let diffs = compare_boot_order(Some(vendor), &order, &options, BOOT_INTERFACE_MAC);
            assert!(diffs.is_empty(), "{vendor} unexpectedly reported a diff");
        }
    }

    #[test]
    fn update_parameters_serializes_to_pascal_case_targets() {
        let params = AmiUpdateParameters {
            targets: vec![
                format!("{AMI_FIRMWARE_INVENTORY}/BMCImage1"),
                format!("{AMI_FIRMWARE_INVENTORY}/BMCImage2"),
            ],
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&params).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "Targets": [
                    "/redfish/v1/UpdateService/FirmwareInventory/BMCImage1",
                    "/redfish/v1/UpdateService/FirmwareInventory/BMCImage2",
                ]
            })
        );
    }

    #[test]
    fn oem_parameters_bmc_omits_preserve_bios() {
        let oem = AmiOemParameters {
            image_type: "BMC".to_string(),
            preserve_bios: None,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&oem).unwrap()).unwrap();
        assert_eq!(json, serde_json::json!({ "ImageType": "BMC" }));
    }

    #[test]
    fn oem_parameters_bios_includes_preserve_bios() {
        let oem = AmiOemParameters {
            image_type: "BIOS".to_string(),
            preserve_bios: Some("true".to_string()),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&oem).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "ImageType": "BIOS", "PreserveBIOS": "true" })
        );
    }

    #[test]
    fn ami_update_targets_maps_bmc_to_both_rom_banks() {
        let (oem, targets) = ami_update_targets(&ComponentType::BMC).unwrap();
        assert_eq!(oem.image_type, "BMC");
        assert!(oem.preserve_bios.is_none());
        assert_eq!(
            targets,
            vec![
                "/redfish/v1/UpdateService/FirmwareInventory/BMCImage1".to_string(),
                "/redfish/v1/UpdateService/FirmwareInventory/BMCImage2".to_string(),
            ]
        );
    }

    #[test]
    fn ami_update_targets_maps_uefi_to_bios_image() {
        let (oem, targets) = ami_update_targets(&ComponentType::UEFI).unwrap();
        assert_eq!(oem.image_type, "BIOS");
        assert_eq!(oem.preserve_bios.as_deref(), Some("true"));
        assert_eq!(
            targets,
            vec!["/redfish/v1/UpdateService/FirmwareInventory/BIOSImage1".to_string()]
        );
    }

    #[test]
    fn ami_update_targets_rejects_unsupported_component() {
        let result = ami_update_targets(&ComponentType::PSU { num: 0 });
        assert!(matches!(result, Err(RedfishError::NotSupported(_))));
    }

    #[test]
    fn ami_update_targets_rejects_unknown_component() {
        let result = ami_update_targets(&ComponentType::Unknown);
        assert!(matches!(result, Err(RedfishError::NotSupported(_))));
    }

    #[test]
    fn extract_task_id_from_messages() {
        let body = serde_json::json!({
            "Messages": [
                {
                    "MessageId": "Base.1.0.Success",
                    "MessageArgs": []
                },
                {
                    "MessageId": "Task.1.0.New",
                    "MessageArgs": ["/redfish/v1/TaskService/Tasks/3"]
                }
            ]
        })
        .to_string();
        assert_eq!(extract_ami_task_id(None, &body), Some("3".to_string()));
    }

    #[test]
    fn extract_task_id_from_top_level_id() {
        let body = serde_json::json!({ "Id": "42" }).to_string();
        assert_eq!(extract_ami_task_id(None, &body), Some("42".to_string()));
    }

    #[test]
    fn extract_task_id_falls_back_to_location_header() {
        assert_eq!(
            extract_ami_task_id(Some("/redfish/v1/TaskService/Tasks/7"), "not json"),
            Some("7".to_string())
        );
    }

    #[test]
    fn extract_task_id_returns_none_when_absent() {
        assert_eq!(extract_ami_task_id(None, "{}"), None);
    }
}
