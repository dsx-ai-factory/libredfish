// SPDX-FileCopyrightText: Copyright (c) 2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: MIT
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
// THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Giga Computing (AMI MegaRAC) BMC support.
//!
//! # Warning
//!
//! This implementation cannot fully lock the BMC down from the host. The IPMI
//! KCS interface is not exposed over Redfish on this platform (Megarac/AST2600),
//! so it can be neither disabled nor inspected from here. Any process on the
//! host with access to KCS can talk to the BMC with no credentials — including
//! fetching bootstrap credentials with an administrator role — regardless of
//! what [`Redfish::lockdown`] reports.
//!
//! Use it only in development or otherwise trusted environments where the host
//! OS is not an adversary, unless KCS has been disabled out of band at BMC
//! firmware level.

use std::{collections::HashMap, path::Path, time::Duration};

use serde::Deserialize;

use crate::{
    jsonmap,
    model::{
        account_service::ManagerAccount,
        boot::{
            self, BootOverride, BootSourceOverrideEnabled, BootSourceOverrideMode,
            BootSourceOverrideTarget,
        },
        certificate::Certificate,
        chassis::{Assembly, Chassis, NetworkAdapter},
        component_integrity::ComponentIntegrities,
        network_device_function::NetworkDeviceFunction,
        oem::nvidia_dpu::{HostPrivilegeLevel, NicMode},
        power::Power,
        secure_boot::SecureBoot,
        sel::LogEntry,
        sensor::GPUSensors,
        service_root::{RedfishVendor, ServiceRoot},
        software_inventory::SoftwareInventory,
        storage::Drives,
        task::Task,
        thermal::Thermal,
        update_service::{ComponentType, TransferProtocolType, UpdateService},
        BootOption, ComputerSystem, Manager, ManagerResetType,
    },
    standard::RedfishStandard,
    BiosProfileType, Boot, BootOptions, Collection, EnabledDisabled, JobState, MachineSetupDiff,
    MachineSetupStatus, ODataId, PCIeDevice, PowerState, Redfish, RedfishError, Resource, RoleId,
    Status, StatusInternal, SystemPowerControl,
};

/// AMI uses BIOS attribute SETUP001 for Administrator Password (UEFI password)
const UEFI_PASSWORD_NAME: &str = "SETUP001";

/// Giga Computing exposes infinite boot as the vendor-coded BIOS attribute
/// GBT0183 ("Endless Retry Boot", MenuPath ./Boot) instead of AMI's generic
/// "EndlessBoot", which this BIOS does not define.
const INFINITE_BOOT_NAME: &str = "GBT0183";

/// Giga Computing R263-ZG0 exposes exactly one host interface, at a fixed path.
const HOST_INTERFACE_URL: &str = "Managers/Self/HostInterfaces/Self";

/// `AuthenticationModes` value that lets the host talk to the BMC with no
/// credentials at all, as whatever role `AuthNoneRoleId` names (on this BMC:
/// `HostInterfaceAdministrator`). Removing it closes the unauthenticated path.
const AUTH_NONE: &str = "AuthNone";

/// What `AuthenticationModes` becomes under lockdown: session auth only, so the
/// host has to present real credentials.
const AUTH_SESSION: &str = "RedfishSessionAuth";

/// The subset of `HostInterface.v1_3_0` that decides whether the host can reach
/// the BMC without credentials.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HostInterface {
    interface_enabled: Option<bool>,
    authentication_modes: Option<Vec<String>>,
    credential_bootstrapping: Option<CredentialBootstrapping>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CredentialBootstrapping {
    enabled: Option<bool>,
}

impl HostInterface {
    /// An absent field reads as open. A firmware that doesn't report one must
    /// not be mistaken for a locked-down machine.
    fn interface_enabled(&self) -> bool {
        self.interface_enabled.unwrap_or(true)
    }

    fn auth_none_allowed(&self) -> bool {
        self.authentication_modes
            .as_ref()
            .is_none_or(|modes| modes.iter().any(|m| m == AUTH_NONE))
    }

    fn credential_bootstrapping_enabled(&self) -> bool {
        self.credential_bootstrapping
            .as_ref()
            .and_then(|cb| cb.enabled)
            .unwrap_or(true)
    }
}

/// Verdict over the host→BMC paths this BMC exposes over Redfish.
///
/// Two of them let the host in with no prior credentials, and they are
/// independent — closing one leaves the other open:
///
///   * `AuthNone` on an enabled interface: unauthenticated requests over the
///     USB NIC are served as `HostInterfaceAdministrator`.
///   * `CredentialBootstrapping`: the host asks the BMC to mint a real
///     username/password with that same admin role. Per DSP0270 the host
///     fetches these over IPMI/KCS, so disabling the USB NIC does not
///     necessarily close this one.
///
/// KCS itself is deliberately absent from this verdict — see `lockdown_status`.
fn lockdown_verdict(hi: &HostInterface) -> Status {
    let interface = hi.interface_enabled();
    let auth_none = hi.auth_none_allowed();
    let bootstrap = hi.credential_bootstrapping_enabled();

    let message = format!(
        "host_interface={interface}, auth_none={auth_none}, cred_bootstrap={bootstrap}; \
         KCS not exposed via Redfish on this platform (state unknown)"
    );

    let is_locked = !interface && !auth_none && !bootstrap;
    let is_unlocked = interface && auth_none && bootstrap;

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

    fn get_accounts<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<ManagerAccount>, RedfishError>> {
        Box::pin(async move { self.s.get_accounts().await })
    }

    /// Giga/AMI pre-populates the account collection with fixed empty slots, so
    /// a POST fills a slot rather than creating a row - and the slot keeps its
    /// `Enabled=false`. The shared `create_user` sends only
    /// UserName/Password/RoleId, which leaves the caller with an account that
    /// exists, reports no error, and rejects every password. Send `Enabled`
    /// explicitly so the account is usable when this returns.
    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        role_id: RoleId,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let data = HashMap::from([
                ("UserName", serde_json::json!(username)),
                ("Password", serde_json::json!(password)),
                ("RoleId", serde_json::json!(role_id.to_string())),
                ("Enabled", serde_json::json!(true)),
            ]);
            self.s.client.post("AccountService/Accounts", data).await?;
            Ok(())
        })
    }

    fn delete_user<'a>(
        &'a self,
        username: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.delete_user(username).await })
    }

    fn get_firmware<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<SoftwareInventory, RedfishError>> {
        Box::pin(async move { self.s.get_firmware(id).await })
    }

    fn get_software_inventories<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_software_inventories().await })
    }

    fn get_tasks<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_tasks().await })
    }

    fn get_task<'a>(&'a self, id: &'a str) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move { self.s.get_task(id).await })
    }

    fn get_power_state<'a>(&'a self) -> crate::RedfishFuture<'a, Result<PowerState, RedfishError>> {
        Box::pin(async move { self.s.get_power_state().await })
    }

    fn get_service_root<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ServiceRoot, RedfishError>> {
        Box::pin(async move { self.s.get_service_root().await })
    }

    fn get_systems<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_systems().await })
    }

    fn get_system<'a>(&'a self) -> crate::RedfishFuture<'a, Result<ComputerSystem, RedfishError>> {
        Box::pin(async move { self.s.get_system().await })
    }

    fn get_managers<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_managers().await })
    }

    fn get_manager<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Manager, RedfishError>> {
        Box::pin(async move { self.s.get_manager().await })
    }

    fn get_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<SecureBoot, RedfishError>> {
        Box::pin(async move { self.s.get_secure_boot().await })
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

    fn get_power_metrics<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Power, RedfishError>> {
        Box::pin(async move { self.s.get_power_metrics().await })
    }

    fn power<'a>(
        &'a self,
        action: SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.power(action).await })
    }

    /// AMI BMC only supports ForceRestart
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

    fn chassis_reset<'a>(
        &'a self,
        chassis_id: &'a str,
        reset_type: SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.chassis_reset(chassis_id, reset_type).await })
    }

    fn bmc_reset_to_defaults<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.bmc_reset_to_defaults().await })
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
        Box::pin(async move { self.s.get_system_event_log().await })
    }

    fn get_bmc_event_log<'a>(
        &'a self,
        from: Option<chrono::DateTime<chrono::Utc>>,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move { self.s.get_bmc_event_log(from).await })
    }

    fn get_drives_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<Drives>, RedfishError>> {
        Box::pin(async move { self.s.get_drives_metrics().await })
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

    /// Giga Computing R263-ZG0 lockdown — closes both credential-free host
    /// paths on the host interface, then verifies by reading the result back.
    ///
    /// `Enabled` disables the interface, drops `AuthNone` from
    /// `AuthenticationModes`, and turns off `CredentialBootstrapping`;
    /// `Disabled` restores all three. Dropping `AuthNone` on its own would
    /// achieve nothing — the host could still bootstrap credentials and log in
    /// with them — so the three are written together or not at all.
    ///
    /// KCS is out of reach here; see `lockdown_status`.
    fn lockdown<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let locking = target == EnabledDisabled::Enabled;
            let auth_mode = if locking { AUTH_SESSION } else { AUTH_NONE };
            let hi_body = HashMap::from([
                ("InterfaceEnabled", serde_json::json!(!locking)),
                ("AuthenticationModes", serde_json::json!([auth_mode])),
                (
                    "CredentialBootstrapping",
                    serde_json::json!({ "Enabled": !locking }),
                ),
            ]);
            self.s
                .client
                .patch_with_if_match(HOST_INTERFACE_URL, hi_body)
                .await?;

            // A Redfish PATCH may answer 200 while silently dropping properties
            // the firmware doesn't implement, reporting them only in
            // @Message.ExtendedInfo. Trusting the status code here would report
            // a machine as locked down while the host can still reach the BMC,
            // so confirm against what the BMC actually stored.
            let verdict = lockdown_verdict(&self.host_interface().await?);
            let applied = if locking {
                verdict.is_fully_enabled()
            } else {
                verdict.is_fully_disabled()
            };
            if !applied {
                return Err(RedfishError::GenericError {
                    error: format!(
                        "lockdown({target}) did not take effect on {HOST_INTERFACE_URL}: {}",
                        verdict.message()
                    ),
                });
            }
            Ok(())
        })
    }

    /// Giga Computing R263-ZG0 lockdown status — host interface only.
    ///
    /// Reports `Enabled` only when every path visible over Redfish is shut:
    /// the interface is disabled, `AuthNone` is gone, and credential
    /// bootstrapping is off. Any partial combination is `Partial` — in
    /// particular an interface that is disabled while bootstrapping stays on,
    /// which is what machines locked down by earlier versions of this code
    /// look like.
    ///
    /// KCS is not represented. It is not configurable over Redfish on
    /// Megarac/AST2600 — it has to be set at BMC firmware level (e.g.
    /// `ipmitool channel setaccess`) at provisioning time, and its state can't
    /// be read from here either. So `Enabled` means "every path we can see is
    /// closed", not "the host cannot reach the BMC".
    fn lockdown_status<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move { Ok(lockdown_verdict(&self.host_interface().await?)) })
    }

    /// Setup serial console for AMI BMC via BIOS attributes.
    fn setup_serial_console<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            use serde_json::Value;

            let attributes: HashMap<String, Value> = HashMap::from([
                ("TER001".to_string(), "Enabled".into()), // Console Redirection
                ("TER010".to_string(), "Enabled".into()), // Console Redirection EMS
                ("TER06B".to_string(), "COM1/SOL".into()), // Out-of-Band Mgmt Port
                ("TER021".to_string(), "115200".into()),  // Bits per second
                ("TER020".to_string(), "115200".into()),  // Bits per second EMS
                ("TER012".to_string(), "VT100Plus".into()), // Terminal Type
                ("TER011".to_string(), "VT-UTF8".into()), // Terminal Type EMS
                ("TER05D".to_string(), "None".into()),    // Flow Control
            ]);

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

            let expected = vec![
                ("TER001", "Enabled", "Disabled"),
                ("TER010", "Enabled", "Disabled"),
                ("TER06B", "COM1/SOL", "any"),
                ("TER021", "115200", "any"),
                ("TER020", "115200", "any"),
                ("TER012", "VT100Plus", "any"),
                ("TER011", "VT-UTF8", "any"),
                ("TER05D", "None", "any"),
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
            let override_target = match target {
                Boot::Pxe => BootSourceOverrideTarget::Pxe,
                Boot::HardDisk => BootSourceOverrideTarget::Hdd,
                Boot::UefiHttp => BootSourceOverrideTarget::UefiHttp,
            };
            self.set_boot_override(override_target, BootSourceOverrideEnabled::Once)
                .await
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

    /// AMI requires patching `/Systems/{id}` (NOT the `@Redfish.Settings` object
    /// `/SD`, which this firmware reserves for the BIOS-staged `BootOrder` /
    /// `AliasBootOrder` -- see [`Redfish::change_boot_order`]) with an `If-Match`
    /// header.
    ///
    /// `Boot.HttpBootUri` is the only mechanism for pinning a UEFI HTTP boot URL
    /// on this platform: unlike Dell (`HttpDev1Uri`) and HPE (`UrlBootFile`), the
    /// R263-ZG0 BIOS registry exposes no URI attribute at all, only the
    /// `NWSK006`/`NWSK007` IPv4/IPv6 HTTP enable switches.
    ///
    /// When `settings.http_boot_uri` is `None` the key is omitted, leaving
    /// whatever URI the BMC already holds -- it does not clear a previously
    /// pinned URL. Mode defaults to `UEFI` when unspecified, matching generic AMI.
    ///
    /// Returns `Ok(None)`: the change applies immediately. This firmware reports
    /// `JobService.Status.State: Disabled` and stages nothing, so there is no job
    /// ID to hand back.
    ///
    /// Verified against: R263-ZG0-AAL2-000, BIOS R23_F20, `ComputerSystem.v1_21_0`.
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
            // Giga R263-ZG0: TCG006 is "TCM State" (Disabled/Enabled), not TPM.
            // The pending-operation slot that accepts "TPM Clear" is TCG008.
            self.set_bios(HashMap::from([("TCG008".to_string(), "TPM Clear".into())]))
                .await
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
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move { self.s.update_firmware(firmware).await })
    }

    fn update_firmware_multipart<'a>(
        &'a self,
        filename: &'a Path,
        reboot: bool,
        timeout: Duration,
        component_type: ComponentType,
    ) -> crate::RedfishFuture<'a, Result<String, RedfishError>> {
        Box::pin(async move {
            self.s
                .update_firmware_multipart(filename, reboot, timeout, component_type)
                .await
        })
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

    fn bios<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        Box::pin(async move { self.s.bios().await })
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

    fn get_network_device_functions<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.s.get_network_device_functions(chassis_id).await })
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

    fn get_job_state<'a>(
        &'a self,
        job_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<JobState, RedfishError>> {
        Box::pin(async move { self.s.get_job_state(job_id).await })
    }

    fn get_resource<'a>(
        &'a self,
        id: ODataId,
    ) -> crate::RedfishFuture<'a, Result<Resource, RedfishError>> {
        Box::pin(async move { self.s.get_resource(id).await })
    }

    fn get_collection<'a>(
        &'a self,
        id: ODataId,
    ) -> crate::RedfishFuture<'a, Result<Collection, RedfishError>> {
        Box::pin(async move { self.s.get_collection(id).await })
    }

    /// Set the DPU (identified by MAC address) as the first boot option.
    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mac_address = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            let mac = mac_address.to_uppercase();
            let (system, all_boot_options) = self.get_system_and_boot_options().await?;

            let target = all_boot_options.iter().find(|opt| {
                let display = opt.display_name.to_uppercase();
                display.contains("HTTP") && display.contains("IPV4") && display.contains(&mac)
            });

            let Some(target) = target else {
                let all_names: Vec<_> = all_boot_options
                    .iter()
                    .map(|b| format!("{}: {}", b.id, b.display_name))
                    .collect();
                return Err(RedfishError::MissingBootOption(format!(
                    "No HTTP IPv4 boot option found for MAC {mac_address}; available: {:#?}",
                    all_names
                )));
            };

            let target_id = target.boot_option_reference.clone();
            let mut boot_order = system.boot.boot_order;

            let boot_order_is_set = boot_order
                .first()
                .is_some_and(|entry| boot::boot_order_entry_reference(entry) == target_id);
            if boot_order_is_set {
                tracing::info!(
                    "NO-OP: DPU ({mac_address}) is already first in boot order ({target_id})"
                );
                return Ok(None);
            }

            if !boot::promote_boot_order_entry_first(&mut boot_order, &target_id) {
                boot_order.insert(0, target_id);
            }
            self.change_boot_order(boot_order).await?;
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
            let (expected, actual) = self.get_expected_and_actual_first_boot_option(&mac).await?;
            Ok(expected.is_some() && expected == actual)
        })
    }

    fn get_update_service<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<UpdateService, RedfishError>> {
        Box::pin(async move { self.s.get_update_service().await })
    }

    fn get_base_mac_address<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.s.get_base_mac_address().await })
    }

    /// AMI lockdown_bmc - BMC-only lockdown (Host Interface only)
    fn lockdown_bmc<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let interface_enabled = target == EnabledDisabled::Disabled;
            let hi_body = HashMap::from([("InterfaceEnabled", interface_enabled)]);
            self.s
                .client
                .patch_with_if_match(HOST_INTERFACE_URL, hi_body)
                .await
        })
    }

    fn is_ipmi_over_lan_enabled<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { self.s.is_ipmi_over_lan_enabled().await })
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

    fn enable_rshim_bmc<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.enable_rshim_bmc().await })
    }

    /// AMI clear_nvram - sets RECV000 (Reset NVRAM) to "Enabled"
    fn clear_nvram<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.set_bios(HashMap::from([("RECV000".to_string(), "Enabled".into())]))
                .await
        })
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
            self.set_bios(HashMap::from([(
                INFINITE_BOOT_NAME.to_string(),
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
            // Absent on BIOS revisions that don't define it; report unknown
            // rather than failing the whole readout.
            let Some(endless_boot) = attrs.get(INFINITE_BOOT_NAME).and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            Ok(Some(endless_boot == "Enabled"))
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

    fn get_component_integrities<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        Box::pin(async move { self.s.get_component_integrities().await })
    }

    fn get_firmware_for_component<'a>(
        &'a self,
        component_integrity_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<SoftwareInventory, RedfishError>> {
        Box::pin(async move {
            self.s
                .get_firmware_for_component(component_integrity_id)
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

    /// AMI doesn't support AC power cycle through standard power action
    fn ac_powercycle_supported_by_power(&self) -> bool {
        false
    }

    fn set_utc_timezone<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.set_utc_timezone().await })
    }

    fn set_ntp_servers<'a>(
        &'a self,
        servers: &'a [String],
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.set_ntp_servers(servers).await })
    }
}

impl Bmc {
    /// AMI requires patching to /Systems/{id} (NOT /SD) with If-Match header
    async fn set_boot_override(
        &self,
        override_target: BootSourceOverrideTarget,
        override_enabled: BootSourceOverrideEnabled,
    ) -> Result<(), RedfishError> {
        let boot_data = HashMap::from([
            ("BootSourceOverrideMode".to_string(), "UEFI".to_string()),
            (
                "BootSourceOverrideEnabled".to_string(),
                override_enabled.to_string(),
            ),
            (
                "BootSourceOverrideTarget".to_string(),
                override_target.to_string(),
            ),
        ]);
        let data = HashMap::from([("Boot", boot_data)]);
        let url = format!("Systems/{}", self.s.system_id());
        self.s.client.patch_with_if_match(&url, data).await
    }

    /// The BMC's single host interface, narrowed to the lockdown-relevant fields.
    async fn host_interface(&self) -> Result<HostInterface, RedfishError> {
        let (_status, hi) = self.s.client.get(HOST_INTERFACE_URL).await?;
        Ok(hi)
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

    /// Get expected and actual first boot option for checking boot order setup.
    ///
    /// AMI boot option format example:
    /// DisplayName: "[Slot2]UEFI: HTTP IPv4 Nvidia Network Adapter - B8:E9:24:17:6D:72 P1"
    /// BootOptionReference: "Boot0001"
    ///
    async fn get_expected_and_actual_first_boot_option(
        &self,
        boot_interface_mac: &str,
    ) -> Result<(Option<String>, Option<String>), RedfishError> {
        let mac = boot_interface_mac.to_uppercase();
        let (system, all_boot_options) = self.get_system_and_boot_options().await?;

        let expected_first_boot_option = all_boot_options
            .iter()
            .find(|opt| {
                let display = opt.display_name.to_uppercase();
                display.contains("HTTP") && display.contains("IPV4") && display.contains(&mac)
            })
            .map(|opt| opt.display_name.clone());

        let actual_first_boot_option = system.boot.boot_order.first().and_then(|first_entry| {
            let first_ref = boot::boot_order_entry_reference(first_entry);
            all_boot_options
                .iter()
                .find(|opt| opt.boot_option_reference == first_ref)
                .map(|opt| opt.display_name.clone())
        });

        Ok((expected_first_boot_option, actual_first_boot_option))
    }

    /// Get the BIOS attributes for machine setup.
    //
    // Dropped vs. ami::machine_setup_attrs (keys absent on Giga Computing R263-ZG0):
    //   LEM0001     — PXE retry count; no LEM* keys on this BMC
    //   FBO001      — Boot Mode Select; platform appears UEFI-only (only FBO201–205 UEFI entries exist)
    //   EndlessBoot — Infinite Boot; spelled GBT0183 on this BIOS (see INFINITE_BOOT_NAME)
    fn machine_setup_attrs(&self) -> HashMap<String, serde_json::Value> {
        HashMap::from([
            ("CPU005".to_string(), "Enabled".into()), // Enable/disable CPU Virtualization
            ("PCIS007".to_string(), "Enabled".into()), // SR-IOV Support
            ("NWSK000".to_string(), "Enabled".into()), // Network Stack
            ("NWSK001".to_string(), "Disabled".into()), // IPv4 PXE Support
            ("NWSK006".to_string(), "Enabled".into()), // IPv4 HTTP Support
            ("NWSK002".to_string(), "Disabled".into()), // IPv6 PXE Support
            ("NWSK007".to_string(), "Disabled".into()), // IPv6 HTTP Support
            (INFINITE_BOOT_NAME.to_string(), "Enabled".into()), // Infinite Boot
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
