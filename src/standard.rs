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
use std::{collections::HashMap, default, path::Path, time::Duration};

use reqwest::{header::HeaderName, Method, StatusCode};
use serde_json::json;
use tracing::debug;

use crate::model::boot::{self, BootSourceOverrideEnabled};
use crate::model::certificate::Certificate;
use crate::model::chassis::Assembly;
use crate::model::component_integrity::ComponentIntegrities;
use crate::model::oem::nvidia_dpu::HostPrivilegeLevel;
use crate::model::service_root::ServiceRoot;
use crate::model::software_inventory::SoftwareInventory;
use crate::model::task::Task;
use crate::model::thermal::Thermal;
use crate::model::update_service::ComponentType;
use crate::model::{account_service::ManagerAccount, service_root::RedfishVendor};
use crate::model::{job::Job, oem::nvidia_dpu::NicMode};
use crate::model::{
    manager_network_protocol::ManagerNetworkProtocol, update_service::TransferProtocolType,
};
use crate::model::{
    power, thermal, BootOption, ComputerSystem, InvalidValueError, Manager, Managers, ODataId,
};
use crate::model::{power::Power, update_service::UpdateService};
use crate::model::{secure_boot::SecureBoot, sensor::GPUSensors};
use crate::model::{sel::LogEntry, ManagerResetType};
use crate::model::{sel::LogEntryCollection, serial_interface::SerialInterface};
use crate::model::{storage::Drives, storage::Storage};
use crate::network::{RedfishHttpClient, REDFISH_ENDPOINT};
use crate::{jsonmap, BootOptions, Collection, PCIeDevice, RedfishError, Resource};
use crate::{
    model, BiosProfileType, Boot, BootOverride, EnabledDisabled, JobState, NetworkDeviceFunction,
    NetworkPort, PowerState, Redfish, RoleId, Status, Systems,
};
use crate::{
    model::chassis::{Chassis, NetworkAdapter},
    MachineSetupStatus,
};

const UEFI_PASSWORD_NAME: &str = "AdministratorPassword";

/// The calls that use the Redfish standard without any OEM extensions.
#[derive(Clone)]
pub struct RedfishStandard {
    pub client: RedfishHttpClient,
    pub vendor: Option<RedfishVendor>,
    manager_id: String,
    system_id: String,
    service_root: ServiceRoot,
}
impl Redfish for RedfishStandard {
    fn std_redfish(&self) -> &RedfishStandard {
        self
    }
    fn create_user<'a>(
        &'a self,
        username: &'a str,
        password: &'a str,
        role_id: RoleId,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut data = HashMap::new();
            data.insert("UserName", username.to_string());
            data.insert("Password", password.to_string());
            data.insert("RoleId", role_id.to_string());
            self.client
                .post("AccountService/Accounts", data)
                .await
                .map(|_resp| Ok(()))?
        })
    }

    fn delete_user<'a>(
        &'a self,
        username: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("AccountService/Accounts/{}", username);
            self.client.delete(&url).await.map(|_status_code| Ok(()))?
        })
    }

    fn change_username<'a>(
        &'a self,
        old_name: &'a str,
        new_name: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let account = self.get_account_by_name(old_name).await?;
            let Some(account_id) = account.id else {
                return Err(RedfishError::UserNotFound(format!(
                    "{old_name} has no ID field"
                )));
            };
            let url = format!("AccountService/Accounts/{account_id}");
            let mut data = HashMap::new();
            data.insert("UserName", new_name);
            self.client
                .patch(&url, &data)
                .await
                .map(|_status_code| Ok(()))?
        })
    }

    fn change_password<'a>(
        &'a self,
        user: &'a str,
        new_pass: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let account = self.get_account_by_name(user).await?;
            let Some(account_id) = account.id else {
                return Err(RedfishError::UserNotFound(format!(
                    "{user} has no ID field"
                )));
            };
            self.change_password_by_id(&account_id, new_pass).await
        })
    }

    fn change_password_by_id<'a>(
        &'a self,
        account_id: &'a str,
        new_pass: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("AccountService/Accounts/{}", account_id);
            let mut data = HashMap::new();
            data.insert("Password", new_pass);
            // Some BMCs reject authenticated ServiceRoot reads until the
            // factory password is changed, while still exposing it
            // anonymously. Prefer that path so password bootstrap can reach
            // the account PATCH. Fall back to the existing authenticated
            // lookup for BMCs that protect or omit vendor details there.
            let service_root = match self.client.get_anonymous::<ServiceRoot>("").await {
                Ok((_status, service_root))
                    if service_root
                        .vendor()
                        .is_some_and(|vendor| vendor != RedfishVendor::Unknown) =>
                {
                    service_root
                }
                _ => self.get_service_root().await?,
            };
            // AMI BMC requires If-Match header for PATCH requests
            if matches!(
                service_root.vendor(),
                Some(RedfishVendor::AMI | RedfishVendor::LenovoAMI | RedfishVendor::LenovoGB300)
            ) {
                self.client.patch_with_if_match(&url, &data).await
            } else {
                self.client
                    .patch(&url, &data)
                    .await
                    .map(|_status_code| Ok(()))?
            }
        })
    }

    fn get_accounts<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<ManagerAccount>, RedfishError>> {
        Box::pin(async move {
            let mut accounts: Vec<ManagerAccount> = self
                .get_collection(ODataId {
                    odata_id: "/redfish/v1/AccountService/Accounts".into(),
                })
                .await
                .and_then(|c| c.try_get::<ManagerAccount>())
                .into_iter()
                .flat_map(|rc| rc.members)
                .collect();

            accounts.sort();
            Ok(accounts)
        })
    }

    fn get_power_state<'a>(&'a self) -> crate::RedfishFuture<'a, Result<PowerState, RedfishError>> {
        Box::pin(async move {
            let system = self.get_system().await?;
            Ok(system.power_state)
        })
    }

    fn get_power_metrics<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Power, RedfishError>> {
        Box::pin(async move {
            let power = self.get_power_metrics().await?;
            Ok(power)
        })
    }

    fn power<'a>(
        &'a self,
        action: model::SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if action == model::SystemPowerControl::ACPowercycle {
                return Err(RedfishError::NotSupported(
                    "AC power cycle not supported on this platform".to_string(),
                ));
            }
            let url = format!("Systems/{}/Actions/ComputerSystem.Reset", self.system_id);
            let mut arg = HashMap::new();
            arg.insert("ResetType", action.to_string());
            // Lenovo: The expected HTTP response code is 204 No Content
            self.client.post(&url, arg).await.map(|_resp| Ok(()))?
        })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        false
    }

    fn bmc_reset<'a>(
        &'a self,
        reset_type: Option<ManagerResetType>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.reset_manager(
                reset_type.unwrap_or(ManagerResetType::GracefulRestart),
                None,
            )
            .await
        })
    }

    fn chassis_reset<'a>(
        &'a self,
        chassis_id: &'a str,
        reset_type: model::SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Chassis/{}/Actions/Chassis.Reset", chassis_id);
            let mut arg = HashMap::new();

            arg.insert("ResetType", reset_type.to_string());
            self.client.post(&url, arg).await.map(|_resp| Ok(()))?
        })
    }

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Thermal, RedfishError>> {
        Box::pin(async move {
            let thermal = self.get_thermal_metrics().await?;
            Ok(thermal)
        })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "No GPUs on this machine".to_string(),
            ))
        })
    }

    fn get_system_event_log<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("SEL".to_string())) })
    }

    fn get_bmc_event_log<'a>(
        &'a self,
        _from: Option<chrono::DateTime<chrono::Utc>>,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("BMC Event Log".to_string())) })
    }

    fn get_drives_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<Drives>, RedfishError>> {
        Box::pin(async move { self.get_drives_metrics().await })
    }

    fn bios<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios", self.system_id());
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn set_bios<'a>(
        &'a self,
        _values: HashMap<String, serde_json::Value>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "set_bios is vendor specific and not available on this platform".to_string(),
            ))
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "reset_bios is vendor specific and not available on this platform".to_string(),
            ))
        })
    }

    fn pending<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<HashMap<String, serde_json::Value>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/Settings", self.system_id());
            self.pending_with_url(&url).await
        })
    }

    fn clear_pending<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/Bios/Settings", self.system_id());
            self.clear_pending_with_url(&url).await
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
        Box::pin(async move { Err(RedfishError::NotSupported("machine_setup".to_string())) })
    }

    fn machine_setup_status<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "machine_setup_status".to_string(),
            ))
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
            self.client
                .patch("AccountService", body)
                .await
                .map(|_status_code| ())
        })
    }

    fn lockdown<'a>(
        &'a self,
        _target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("lockdown".to_string())) })
    }

    fn lockdown_status<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("lockdown_status".to_string())) })
    }

    fn setup_serial_console<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "setup_serial_console".to_string(),
            ))
        })
    }

    fn serial_console_status<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Status, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "setup_serial_console".to_string(),
            ))
        })
    }

    fn get_boot_options<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<BootOptions, RedfishError>> {
        Box::pin(async move { self.get_boot_options().await })
    }

    fn get_boot_option<'a>(
        &'a self,
        option_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<BootOption, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/BootOptions/{}", self.system_id(), option_id);
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn boot_once<'a>(&'a self, target: Boot) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.set_boot_override(BootOverride {
                target: target.into(),
                enabled: BootSourceOverrideEnabled::Once,
                mode: None,
                http_boot_uri: None,
            })
            .await?;
            Ok(())
        })
    }

    fn boot_first<'a>(
        &'a self,
        target: Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            self.set_boot_override(BootOverride {
                target: target.into(),
                enabled: BootSourceOverrideEnabled::Continuous,
                mode: None,
                http_boot_uri: None,
            })
            .await?;
            Ok(())
        })
    }

    fn set_boot_override<'a>(
        &'a self,
        settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let boot = boot::Boot {
                boot_source_override_target: Some(settings.target),
                boot_source_override_enabled: Some(settings.enabled),
                boot_source_override_mode: settings.mode,
                http_boot_uri: settings.http_boot_uri,
                ..Default::default()
            };
            let body = HashMap::from([("Boot", boot)]);
            let url = format!("Systems/{}", self.system_id());
            self.client.patch(&url, body).await?;
            Ok(None)
        })
    }

    fn clear_tpm<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("clear_tpm".to_string())) })
    }

    fn pcie_devices<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<PCIeDevice>, RedfishError>> {
        Box::pin(async move {
            let system = self.get_system().await?;
            let chassis = system
                .links
                .and_then(|l| l.chassis)
                .map(|chassis| {
                    chassis
                        .into_iter()
                        .filter_map(|odata_id| {
                            odata_id
                                .odata_id
                                .trim_matches('/')
                                .split('/')
                                .next_back()
                                .map(|v| v.to_string())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or(vec![self.system_id().into()]);
            self.pcie_devices_for_chassis(chassis).await
        })
    }

    fn get_firmware<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<SoftwareInventory, RedfishError>> {
        Box::pin(async move {
            let url = format!("UpdateService/FirmwareInventory/{}", id);
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn update_firmware<'a>(
        &'a self,
        firmware: tokio::fs::File,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            let (_status_code, body) = self.client.post_file("UpdateService", firmware).await?;
            Ok(body)
        })
    }

    fn update_firmware_multipart<'a>(
        &'a self,
        _filename: &'a Path,
        _reboot: bool,
        _timeout: Duration,
        _component_type: ComponentType,
    ) -> crate::RedfishFuture<'a, Result<String, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Multipart firmware updates not currently supported on this platform".to_string(),
            ))
        })
    }

    fn get_tasks<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.get_members("TaskService/Tasks/").await })
    }

    /// http://redfish.dmtf.org/schemas/v1/TaskCollection.json
    fn get_task<'a>(&'a self, id: &'a str) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            let url = format!("TaskService/Tasks/{}", id);
            let (_status_code, body) = self.client.get::<Task>(&url).await?;

            if let Some(msg) = body
                .messages
                .iter()
                .find(|x| x.message_id == "Update.1.0.OperationTransitionedToJob")
            {
                if let Some(message_arg) = msg.message_args.first() {
                    // The task is redirecting us to a JobService.  Look at that instead, and make a fake task from it.
                    let (_, job): (_, Job) = self
                        .client
                        .get(
                            message_arg
                                .strip_prefix("/redfish/v1/")
                                .unwrap_or("wrong_prefix"),
                        )
                        .await?;
                    return Ok(job.as_task());
                }
            }
            Ok(body)
        })
    }

    /// Vec of chassis id
    /// http://redfish.dmtf.org/schemas/v1/ChassisCollection.json
    fn get_chassis_all<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.get_members("Chassis/").await })
    }

    fn get_chassis<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Chassis, RedfishError>> {
        Box::pin(async move {
            let url = format!("Chassis/{}", id);
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn get_chassis_assembly<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Assembly, RedfishError>> {
        Box::pin(async move {
            let url = format!("Chassis/{}/Assembly", chassis_id);
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn get_chassis_network_adapters<'a>(
        &'a self,
        chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Chassis/{}/NetworkAdapters", chassis_id);
            self.get_members(&url).await
        })
    }

    fn get_base_network_adapters<'a>(
        &'a self,
        _system_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            // this is only implemented in iLO5, and will be removed in iLO6.
            Err(RedfishError::NotSupported(
                "BaseNetworkAdapter is only supported in iLO5".to_string(),
            ))
        })
    }

    fn get_base_network_adapter<'a>(
        &'a self,
        _system_id: &'a str,
        _id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        Box::pin(async move {
            // this is only implemented in iLO5, and will be removed in iLO6.
            Err(RedfishError::NotSupported(
                "BaseNetworkAdapter is only supported in iLO5".to_string(),
            ))
        })
    }

    fn get_chassis_network_adapter<'a>(
        &'a self,
        chassis_id: &'a str,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        Box::pin(async move {
            let url = format!("Chassis/{}/NetworkAdapters/{}", chassis_id, id);
            let (_, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    /// http://redfish.dmtf.org/schemas/v1/EthernetInterfaceCollection.json
    fn get_manager_ethernet_interfaces<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Managers/{}/EthernetInterfaces", self.manager_id);
            self.get_members(&url).await
        })
    }

    fn get_manager_ethernet_interface<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::EthernetInterface, RedfishError>> {
        Box::pin(async move {
            let url = format!("Managers/{}/EthernetInterfaces/{}", self.manager_id(), id);
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn get_system_ethernet_interfaces<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/EthernetInterfaces", self.system_id);
            self.get_members(&url).await
        })
    }

    fn get_system_ethernet_interface<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::EthernetInterface, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/EthernetInterfaces/{}", self.system_id(), id);
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    /// http://redfish.dmtf.org/schemas/v1/SoftwareInventoryCollection.json#/definitions/SoftwareInventoryCollection
    fn get_software_inventories<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { self.get_members("UpdateService/FirmwareInventory").await })
    }

    fn get_system<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<model::ComputerSystem, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/", self.system_id);
            let host: model::ComputerSystem = self.client.get(&url).await?.1;
            Ok(host)
        })
    }

    fn get_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<SecureBoot, RedfishError>> {
        Box::pin(async move {
            let url = format!("Systems/{}/SecureBoot", self.system_id());
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn enable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut data = HashMap::new();
            data.insert("SecureBootEnable", true);
            let url = format!("Systems/{}/SecureBoot", self.system_id());
            let _status_code = self.client.patch(&url, data).await?;
            Ok(())
        })
    }

    fn get_secure_boot_certificate<'a>(
        &'a self,
        database_id: &'a str,
        certificate_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Certificate, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Systems/{}/SecureBoot/SecureBootDatabases/{}/Certificates/{}",
                self.system_id(),
                database_id,
                certificate_id
            );
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn get_secure_boot_certificates<'a>(
        &'a self,
        database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Systems/{}/SecureBoot/SecureBootDatabases/{}/Certificates",
                self.system_id(),
                database_id
            );
            self.get_members(&url).await
        })
    }

    fn add_secure_boot_certificate<'a>(
        &'a self,
        pem_cert: &'a str,
        database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            let mut data = HashMap::new();
            data.insert("CertificateString", pem_cert);
            data.insert("CertificateType", "PEM");
            let url = format!(
                "Systems/{}/SecureBoot/SecureBootDatabases/{}/Certificates",
                self.system_id(),
                database_id
            );
            let (_status_code, resp_opt, _resp_headers) = self
                .client
                .req::<Task, _>(Method::POST, &url, Some(data), None, None, Vec::new())
                .await?;
            match resp_opt {
                Some(response_body) => Ok(response_body),
                None => Err(RedfishError::NoContent),
            }
        })
    }

    fn disable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let mut data = HashMap::new();
            data.insert("SecureBootEnable", false);
            let url = format!("Systems/{}/SecureBoot", self.system_id());
            let _status_code = self.client.patch(&url, data).await?;
            Ok(())
        })
    }

    fn get_network_device_functions<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "get_network_device_functions".to_string(),
            ))
        })
    }

    fn get_network_device_function<'a>(
        &'a self,
        _chassis_id: &'a str,
        _id: &'a str,
        _port: Option<&'a str>,
    ) -> crate::RedfishFuture<'a, Result<NetworkDeviceFunction, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "get_network_device_function".to_string(),
            ))
        })
    }

    fn get_ports<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Chassis/{}/NetworkAdapters/{}/Ports",
                chassis_id, network_adapter
            );
            self.get_members(&url).await
        })
    }

    fn get_port<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<NetworkPort, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Chassis/{}/NetworkAdapters/{}/Ports/{}",
                chassis_id, network_adapter, id
            );
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn change_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
        new_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            self.change_bios_password(UEFI_PASSWORD_NAME, current_uefi_password, new_uefi_password)
                .await
        })
    }

    fn change_boot_order<'a>(
        &'a self,
        _boot_array: Vec<String>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("change_boot_order".to_string())) })
    }

    fn get_service_root<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ServiceRoot, RedfishError>> {
        Box::pin(async move {
            let (_status_code, mut body): (StatusCode, ServiceRoot) = self.client.get("").await?;
            if body.vendor.is_none() && !self.client.is_anonymous() {
                // Power shelves don't advertise a vendor in the service root,
                // so fall back to the Manufacturer of the first chassis that
                // reports one. Lite-On exposes it under the "powershelf"
                // chassis, while Delta uses "chassis", so iterate rather than
                // hard-coding a single chassis id.
                let chassis_all = self.get_chassis_all().await?;
                for chassis_id in &chassis_all {
                    if let Ok(chassis) = self.get_chassis(chassis_id).await {
                        if let Some(x) = chassis.manufacturer {
                            body.vendor = Some(x);
                            break;
                        }
                    }
                }
            }
            Ok(body)
        })
    }

    fn get_systems<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let systems: Systems = match self.client.get("Systems/").await {
                Ok((_, systems)) => systems,
                // Power shelves (e.g. Delta) omit the Systems collection
                // entirely and return 404 rather than an empty collection.
                // Treat that the same as "no systems" and fall back to the
                // DMTF-suggested default id; power-shelf vendor clients never
                // touch /Systems anyway.
                Err(RedfishError::HTTPErrorCode { status_code, .. })
                    if status_code == StatusCode::NOT_FOUND =>
                {
                    return Ok(vec!["1".to_string()]);
                }
                Err(e) => return Err(e),
            };
            if systems.members.is_empty() {
                return Ok(vec!["1".to_string()]); // default to DMTF standard suggested
            }
            let v: Result<Vec<String>, RedfishError> = systems
                .members
                .into_iter()
                .map(|d| {
                    d.odata_id
                        .trim_matches('/')
                        .split('/')
                        .next_back()
                        .map(|s| s.to_string())
                        .ok_or_else(|| RedfishError::GenericError {
                            error: format!("Invalid odata_id format: {}", d.odata_id),
                        })
                })
                .collect();

            v
        })
    }

    fn get_manager<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Manager, RedfishError>> {
        Box::pin(async move {
            let (_, manager): (_, Manager) = self
                .client
                .get(&format!("Managers/{}", self.manager_id()))
                .await?;
            Ok(manager)
        })
    }

    fn get_managers<'a>(&'a self) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let (_, bmcs): (_, Managers) = self.client.get("Managers/").await?;
            if bmcs.members.is_empty() {
                return Ok(vec!["1".to_string()]);
            }
            let v: Result<Vec<String>, RedfishError> = bmcs
                .members
                .into_iter()
                .map(|d| {
                    d.odata_id
                        .trim_matches('/')
                        .split('/')
                        .next_back()
                        .map(|s| s.to_string())
                        .ok_or_else(|| RedfishError::GenericError {
                            error: format!("Invalid odata_id format: {}", d.odata_id),
                        })
                })
                .collect();
            v
        })
    }

    fn bmc_reset_to_defaults<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Managers/{}/Actions/Manager.ResetToDefaults",
                self.manager_id
            );
            let mut arg = HashMap::new();
            arg.insert("ResetType", "ResetAll".to_string());
            self.client.post(&url, arg).await.map(|_resp| Ok(()))?
        })
    }

    fn get_job_state<'a>(
        &'a self,
        _job_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<JobState, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("get_job_state".to_string())) })
    }

    fn get_resource<'a>(
        &'a self,
        id: ODataId,
    ) -> crate::RedfishFuture<'a, Result<Resource, RedfishError>> {
        Box::pin(async move {
            let url = id.odata_id.replace(&format!("/{REDFISH_ENDPOINT}/"), "");
            let (_, mut resource): (StatusCode, Resource) = self.client.get(url.as_str()).await?;

            resource.url = url;
            Ok(resource)
        })
    }

    // This function appends ?$expand=.($levels=1) to the URL, as defined by Redfish spec, to expand first level URIs.
    fn get_collection<'a>(
        &'a self,
        id: ODataId,
    ) -> crate::RedfishFuture<'a, Result<Collection, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "{}?$expand=.($levels=1)",
                id.odata_id.replace(&format!("/{REDFISH_ENDPOINT}/"), "")
            );
            let (_, body): (_, HashMap<String, serde_json::Value>) =
                self.client.get(url.as_str()).await?;
            Ok(Collection {
                url: url.clone(),
                body,
            })
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "set_boot_order_dpu_first".to_string(),
            ))
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn get_update_service<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<UpdateService, RedfishError>> {
        Box::pin(async move {
            let (_, update_service) = self.client.get(self.update_service().as_str()).await?;
            Ok(update_service)
        })
    }

    fn get_base_mac_address<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "get_base_mac_address".to_string(),
            ))
        })
    }

    fn lockdown_bmc<'a>(
        &'a self,
        _target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn is_ipmi_over_lan_enabled<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let network_protocol = self.get_manager_network_protocol().await?;
            match network_protocol.ipmi {
                Some(ipmi_status) => match ipmi_status.protocol_enabled {
                    Some(is_ipmi_enabled) => Ok(is_ipmi_enabled),
                    None => Err(RedfishError::GenericError {
                        error: format!(
                        "protocol_enabled is None in the server's ipmi status: {ipmi_status:#?}"
                    ),
                    }),
                },
                None => Err(RedfishError::GenericError {
                    error: format!(
                    "ipmi is None in the server's network service settings: {network_protocol:#?}"
                ),
                }),
            }
        })
    }

    fn enable_ipmi_over_lan<'a>(
        &'a self,
        target: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!("Managers/{}/NetworkProtocol", self.manager_id(),);
            let mut ipmi_data = HashMap::new();
            ipmi_data.insert("ProtocolEnabled", target.is_enabled());

            let mut data = HashMap::new();
            data.insert("IPMI", ipmi_data);

            self.client.patch(&url, data).await.map(|_status_code| ())
        })
    }

    fn update_firmware_simple_update<'a>(
        &'a self,
        image_uri: &'a str,
        targets: Vec<String>,
        transfer_protocol: TransferProtocolType,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            let data: HashMap<String, serde_json::Value> = HashMap::from([
                ("ImageURI".to_string(), json!(image_uri)),
                ("TransferProtocol".to_string(), json!(transfer_protocol)),
                ("Targets".to_string(), json!(targets)),
            ]);

            let (_status_code, resp_opt, _) = self
                .client
                .req::<Task, _>(
                    Method::POST,
                    "UpdateService/Actions/UpdateService.SimpleUpdate",
                    Some(data),
                    None,
                    None,
                    Vec::new(),
                )
                .await?;
            match resp_opt {
                Some(response_body) => Ok(response_body),
                None => Err(RedfishError::NoContent),
            }
        })
    }

    fn enable_rshim_bmc<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("enable_rshim_bmc".to_string())) })
    }

    fn clear_nvram<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("clear_nvram".to_string())) })
    }

    fn get_nic_mode<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<NicMode>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn set_nic_mode<'a>(
        &'a self,
        _mode: NicMode,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("set_nic_mode".to_string())) })
    }

    fn is_infinite_boot_enabled<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "enable_infinite_boot".to_string(),
            ))
        })
    }

    fn set_host_rshim<'a>(
        &'a self,
        _enabled: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("set_host_rshim".to_string())) })
    }

    fn get_host_rshim<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<EnabledDisabled>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn set_idrac_lockdown<'a>(
        &'a self,
        _enabled: EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("set_idrac_lockdown".to_string())) })
    }

    fn get_boss_controller<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { Ok(None) })
    }

    fn decommission_storage_controller<'a>(
        &'a self,
        _controller_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "decommission_storage_controller".to_string(),
            ))
        })
    }

    fn create_storage_volume<'a>(
        &'a self,
        _controller_id: &'a str,
        _volume_name: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "create_storage_volume".to_string(),
            ))
        })
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "is_boot_order_setup".to_string(),
            ))
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("is_bios_setup".to_string())) })
    }

    fn get_component_integrities<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        Box::pin(async move {
            let url = "ComponentIntegrity?$expand=.($levels=1)";
            let (_status_code, body) = self.client.get(url).await?;
            Ok(body)
        })
    }

    fn get_firmware_for_component<'a>(
        &'a self,
        _component_integrity_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<SoftwareInventory, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Not implemented for the given vendor.".to_string(),
            ))
        })
    }

    fn get_component_ca_certificate<'a>(
        &'a self,
        url: &'a str,
    ) -> crate::RedfishFuture<'a, Result<model::component_integrity::CaCertificate, RedfishError>>
    {
        Box::pin(async move {
            let url = url.replace("/redfish/v1/", "");
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn trigger_evidence_collection<'a>(
        &'a self,
        url: &'a str,
        nonce: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            let url = url.replace("/redfish/v1/", "");
            let mut arg = HashMap::new();
            arg.insert("Nonce", nonce.to_string());
            let (_status_code, resp_opt, _) = self
                .client
                .req::<Task, _>(Method::POST, &url, Some(arg), None, None, Vec::new())
                .await?;
            match resp_opt {
                Some(response_body) => Ok(response_body),
                None => Err(RedfishError::NoContent),
            }
        })
    }

    fn get_evidence<'a>(
        &'a self,
        url: &'a str,
    ) -> crate::RedfishFuture<'a, Result<model::component_integrity::Evidence, RedfishError>> {
        Box::pin(async move {
            let url = format!("{}/data", url.replace("/redfish/v1/", ""));
            let (_status_code, body) = self.client.get(&url).await?;
            Ok(body)
        })
    }

    fn set_host_privilege_level<'a>(
        &'a self,
        _level: HostPrivilegeLevel,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "set_host_privilege_level".to_string(),
            ))
        })
    }

    fn set_utc_timezone<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // No-op for non-Dell vendors
            Ok(())
        })
    }

    fn get_spx_nic_east_west_control_enabled<'a>(
        &'a self,
        _nic_index: u8,
    ) -> crate::RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        Box::pin(async move {
            // Not applicable for non-Vera-Rubin vendors
            Ok(None)
        })
    }

    fn set_spx_nic_east_west_control_enabled<'a>(
        &'a self,
        _nic_index: u8,
        _enabled: bool,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // No-op for non-Vera-Rubin vendors
            Ok(())
        })
    }

    fn get_spx_nic_mac_address<'a>(
        &'a self,
        _nic_index: u8,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            // Not applicable for non-Vera-Rubin vendors
            Ok(None)
        })
    }

    fn get_spx_nic_model_and_name<'a>(
        &'a self,
        _nic_index: u8,
    ) -> crate::RedfishFuture<'a, Result<Option<crate::SpxNicModelAndName>, RedfishError>> {
        Box::pin(async move {
            // Not applicable for non-Vera-Rubin vendors
            Ok(None)
        })
    }

    fn set_ntp_servers<'a>(
        &'a self,
        servers: &'a [String],
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.set_manager_ntp_servers(servers).await })
    }
}

impl RedfishStandard {
    //
    // PUBLIC
    //

    pub async fn get_members(&self, url: &str) -> Result<Vec<String>, RedfishError> {
        let (_, body): (_, HashMap<String, serde_json::Value>) = self.client.get(url).await?;
        self.parse_members(url, body)
    }

    pub async fn get_members_with_timout(
        &self,
        url: &str,
        timeout: Option<Duration>,
    ) -> Result<Vec<String>, RedfishError> {
        let (_, body): (_, HashMap<String, serde_json::Value>) =
            self.client.get_with_timeout(url, timeout).await?;
        self.parse_members(url, body)
    }

    fn parse_members(
        &self,
        url: &str,
        mut body: HashMap<String, serde_json::Value>,
    ) -> Result<Vec<String>, RedfishError> {
        let members: Vec<ODataId> = jsonmap::extract(&mut body, "Members", url)?;
        let member_ids: Vec<String> = members
            .into_iter()
            .filter_map(|d| d.odata_id_get().map(|id| id.to_string()).ok())
            .collect();
        Ok(member_ids)
    }
    /// Fetch root URL and record the vendor, if any
    pub async fn set_vendor(
        &mut self,
        vendor: RedfishVendor,
    ) -> Result<Box<dyn crate::Redfish>, RedfishError> {
        self.vendor = Some(vendor);
        debug!("BMC Vendor: {vendor}");
        match vendor {
            // nvidia dgx systems may have both ami and nvidia as vendor strings depending on hw
            // ami also ships its bmc fw for other system vendors.
            RedfishVendor::AMI => {
                if self.system_id == "DGX" && self.manager_id == "BMC" {
                    Ok(Box::new(crate::nvidia_viking::Bmc::new(self.clone())?))
                } else {
                    Ok(Box::new(crate::ami::Bmc::new(self.clone())?))
                }
            }
            RedfishVendor::Dell => Ok(Box::new(crate::dell::Bmc::new(self.clone())?)),
            RedfishVendor::Hpe => Ok(Box::new(crate::hpe::Bmc::new(self.clone())?)),
            RedfishVendor::Lenovo => Ok(Box::new(crate::lenovo::Bmc::new(self.clone())?)),
            RedfishVendor::LenovoAMI => Ok(Box::new(crate::ami::Bmc::new(self.clone())?)),
            RedfishVendor::LenovoGB300 => Ok(Box::new(crate::ami::Bmc::new(self.clone())?)),
            RedfishVendor::NvidiaDpu => Ok(Box::new(crate::nvidia_dpu::Bmc::new(self.clone())?)),
            RedfishVendor::NvidiaGBx00 => {
                Ok(Box::new(crate::nvidia_gbx00::Bmc::new(self.clone())?))
            }
            RedfishVendor::VeraRubin => {
                Ok(Box::new(crate::nvidia_vera_rubin::Bmc::new(self.clone())?))
            }
            RedfishVendor::NvidiaGBSwitch => {
                Ok(Box::new(crate::nvidia_gbswitch::Bmc::new(self.clone())?))
            }
            RedfishVendor::NvidiaGH200 => {
                Ok(Box::new(crate::nvidia_gh200::Bmc::new(self.clone())?))
            }
            RedfishVendor::Supermicro => Ok(Box::new(crate::supermicro::Bmc::new(self.clone())?)),
            RedfishVendor::LiteOnPowerShelf => {
                Ok(Box::new(crate::liteon_powershelf::Bmc::new(self.clone())?))
            }
            RedfishVendor::DeltaPowerShelf => {
                Ok(Box::new(crate::delta_powershelf::Bmc::new(self.clone())?))
            }
            RedfishVendor::Sushy => Ok(Box::new(crate::sushy::Bmc::new(self.clone())?)),
            _ => Ok(Box::new(self.clone())),
        }
    }

    /// Needed for all `Systems/{system_id}/...` calls
    pub fn set_system_id(&mut self, system_id: &str) -> Result<(), RedfishError> {
        self.system_id = system_id.to_string();
        Ok(())
    }

    /// Needed for all `Managers/{system_id}/...` calls
    pub fn set_manager_id(&mut self, manager_id: &str) -> Result<(), RedfishError> {
        self.manager_id = manager_id.to_string();
        Ok(())
    }

    /// Saves the service_root for later use
    pub fn set_service_root(&mut self, service_root: ServiceRoot) -> Result<(), RedfishError> {
        self.service_root = service_root;
        Ok(())
    }

    /// Create client object
    pub fn new(client: RedfishHttpClient) -> Self {
        Self {
            client,
            manager_id: "".to_string(),
            system_id: "".to_string(),
            vendor: None,
            service_root: default::Default::default(),
        }
    }

    pub fn system_id(&self) -> &str {
        &self.system_id
    }

    pub fn manager_id(&self) -> &str {
        &self.manager_id
    }

    /// Gets the location of the update service from the saved service root
    pub fn update_service(&self) -> String {
        self.service_root
            .update_service
            .clone()
            .unwrap_or_default()
            .get("@odata.id")
            .unwrap_or(&serde_json::Value::String(
                "/redfish/v1/UpdateService".to_string(), // Sane default
            ))
            .as_str()
            .unwrap_or_default()
            .replace("/redfish/v1/", "") // Remove starting /redfish/v1 as we add it elsewhere
            .to_string()
    }

    pub async fn get_boot_options(&self) -> Result<model::BootOptions, RedfishError> {
        let url = format!("Systems/{}/BootOptions", self.system_id());
        let (_status_code, body) = self.client.get(&url).await?;
        Ok(body)
    }

    pub async fn get_first_boot_option(&self) -> Result<BootOption, RedfishError> {
        let boot_options = self.get_boot_options().await?;
        let Some(member) = boot_options.members.first() else {
            return Err(RedfishError::NoContent);
        };
        let url = member
            .odata_id
            .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
        let b: BootOption = self.client.get(&url).await?.1;
        Ok(b)
    }

    pub fn get_manager_with_id<'a>(
        &'a self,
        manager_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Manager, RedfishError>> {
        Box::pin(async move {
            let (_, manager): (_, Manager) =
                self.client.get(&format!("Managers/{}", manager_id)).await?;
            Ok(manager)
        })
    }

    pub async fn fetch_bmc_event_log(
        &self,
        url: String,
        from: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<LogEntry>, RedfishError> {
        let url_with_filter = match from {
            Some(from) => {
                let filter_value = format!(
                    "Created ge '{}'",
                    from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                );
                let encoded_filter = urlencoding::encode(&filter_value).into_owned();
                format!("{}?$filter={}", url, encoded_filter)
            }
            None => url,
        };

        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.client.get(&url_with_filter).await?;
        Ok(log_entry_collection.members)
    }

    // The URL differs for Lenovo, but the rest is the same
    pub async fn pending_with_url(
        &self,
        pending_url: &str,
    ) -> Result<HashMap<String, serde_json::Value>, RedfishError> {
        let pending_attrs = self.pending_attributes(pending_url).await?;
        let current_attrs = self.bios_attributes().await?;
        Ok(attr_diff(&pending_attrs, &current_attrs))
    }

    // There's no standard Redfish way to clear pending BIOS settings, so we find the
    // pending changes and set them back to their existing values
    pub async fn clear_pending_with_url(&self, pending_url: &str) -> Result<(), RedfishError> {
        let pending_attrs = self.pending_attributes(pending_url).await?;
        let current_attrs = self.bios_attributes().await?;
        let diff = attr_diff(&pending_attrs, &current_attrs);

        let mut reset_attrs = HashMap::new();
        for k in diff.keys() {
            reset_attrs.insert(k, current_attrs.get(k));
        }
        let mut body = HashMap::new();
        body.insert("Attributes", reset_attrs);
        self.client
            .patch(pending_url, body)
            .await
            .map(|_status_code| ())
    }

    /// Get the first serial interface
    /// On Dell it has no useful content. On Lenovo and Supermicro it does,
    /// and on Supermicro it's part of setting up Serial-Over-LAN.
    pub async fn get_serial_interface(&self) -> Result<SerialInterface, RedfishError> {
        let interface_id = self.get_serial_interface_name().await?;
        let url = format!(
            "Managers/{}/SerialInterfaces/{}",
            self.manager_id(),
            interface_id
        );
        let (_status_code, body) = self.client.get(&url).await?;
        Ok(body)
    }

    /// The name of the first serial interface.
    /// I have not seen a box with any number except exactly one yet.
    pub async fn get_serial_interface_name(&self) -> Result<String, RedfishError> {
        let url = format!("Managers/{}/SerialInterfaces", self.manager_id());
        let mut members = self.get_members(&url).await?;
        let Some(member) = members.pop() else {
            return Err(RedfishError::InvalidValue {
                url: url.to_string(),
                field: "0".to_string(),
                err: InvalidValueError("Members array is empty, no SerialInterfaces".to_string()),
            });
        };
        Ok(member)
    }

    // pending_attributes returns BIOS attributes that will be applied on next restart.
    pub async fn pending_attributes(
        &self,
        pending_url: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, RedfishError> {
        let (_sc, mut body): (reqwest::StatusCode, HashMap<String, serde_json::Value>) =
            self.client.get(pending_url).await?;
        jsonmap::extract_object(&mut body, "Attributes", pending_url)
    }

    // bios_attributes returns the current BIOS attributes.
    pub async fn bios_attributes(&self) -> Result<serde_json::Value, RedfishError> {
        let url = format!("Systems/{}/Bios", self.system_id());
        let mut b = self.bios().await?;

        b.remove("Attributes")
            .ok_or_else(|| RedfishError::MissingKey {
                key: "Attributes".to_string(),
                url,
            })
    }

    pub async fn if_system_has_bios(&self, system_id: &str) -> Option<ComputerSystem> {
        self.client
            .get::<ComputerSystem>(&format!("Systems/{system_id}"))
            .await
            .map_or(None, |(_code, cs)| Some(cs))
            .and_then(|cs| if cs.bios.is_some() { Some(cs) } else { None })
    }

    pub async fn factory_reset_bios(&self) -> Result<(), RedfishError> {
        let url = format!("Systems/{}/Bios/Actions/Bios.ResetBios", self.system_id());
        self.client
            .post(&url, HashMap::<String, String>::new())
            .await
            .map(|_| ())
    }

    pub async fn get_account_by_id(
        &self,
        account_id: &str,
    ) -> Result<ManagerAccount, RedfishError> {
        let url = format!("AccountService/Accounts/{account_id}");
        let (_status_code, body) = self.client.get(&url).await?;
        Ok(body)
    }

    /// Iterates all accounts comparing the username. In practice I've never seen a BMC with more
    /// than about three accounts, so perf not a concern.
    /// Returns an error if the account does not exist.
    pub async fn get_account_by_name(
        &self,
        username: &str,
    ) -> Result<ManagerAccount, RedfishError> {
        let account_ids = self.get_members("AccountService/Accounts").await?;
        for id in account_ids {
            let account = self.get_account_by_id(&id).await?;
            if account.username == username {
                return Ok(account);
            }
        }
        Err(RedfishError::UserNotFound(username.to_string()))
    }

    /// Dell ships with all sixteen user accounts populated but disabled.
    /// To create an account we have to edit one of them.
    pub async fn edit_account(
        &self,
        account_id: u8,
        username: &str,
        password: &str,
        role_id: RoleId,
        enabled: bool,
    ) -> Result<(), RedfishError> {
        let url = format!("AccountService/Accounts/{account_id}");
        let account = ManagerAccount {
            id: None, // it's in the URL, must not be set here
            username: username.to_string(),
            password: Some(password.to_string()),
            enabled: Some(enabled),
            role_id: role_id.to_string(),
            ..Default::default()
        };
        self.client
            .patch(&url, &account)
            .await
            .map(|_status_code| Ok(()))?
    }

    //
    // PRIVATE
    //

    /// Query the power status from the server
    #[allow(dead_code)]
    pub async fn get_power_status(&self) -> Result<power::Power, RedfishError> {
        let url = format!("Chassis/{}/Power/", self.system_id());
        let (_status_code, body) = self.client.get(&url).await?;
        Ok(body)
    }

    /// Query the power supplies and voltages stats from the server
    pub async fn get_power_metrics(&self) -> Result<power::Power, RedfishError> {
        let url = format!("Chassis/{}/Power/", self.system_id());
        let (_status_code, body) = self.client.get(&url).await?;
        Ok(body)
    }

    /// Assemble power metrics by discovering the chassis that carries a
    /// `PowerSubsystem` link and following that chassis's own
    /// `PowerSubsystem`/`Sensors` links, rather than hard-coding a chassis id.
    /// This avoids vendor-specific assumptions (e.g. Lite-On names the chassis
    /// `powershelf`, Delta names it `chassis`).
    pub(crate) async fn get_power_metrics_from_power_subsystem(
        &self,
    ) -> Result<power::Power, RedfishError> {
        for chassis_id in self.get_chassis_all().await? {
            let chassis = self.get_chassis(&chassis_id).await?;
            if chassis.power_subsystem.is_some() {
                return chassis.get_power_metrics(&self.client).await;
            }
        }
        Err(RedfishError::GenericError {
            error: "No chassis with a PowerSubsystem found".to_string(),
        })
    }

    /// Assemble thermal metrics by discovering the chassis that carries a
    /// `ThermalSubsystem` link and following that chassis's
    /// `ThermalSubsystem`/`ThermalMetrics` links, rather than hard-coding a
    /// chassis id.
    pub(crate) async fn get_thermal_metrics_from_thermal_subsystem(
        &self,
    ) -> Result<thermal::Thermal, RedfishError> {
        for chassis_id in self.get_chassis_all().await? {
            let chassis = self.get_chassis(&chassis_id).await?;
            if chassis.thermal_subsystem.is_some() {
                return chassis.get_thermal_metrics(&self.client).await;
            }
        }
        Err(RedfishError::GenericError {
            error: "No chassis with a ThermalSubsystem found".to_string(),
        })
    }

    /// Query the thermal status from the server
    pub async fn get_thermal_metrics(&self) -> Result<thermal::Thermal, RedfishError> {
        let url = format!("Chassis/{}/Thermal/", self.system_id());
        let (_status_code, body) = self.client.get(&url).await?;
        Ok(body)
    }

    /// Query the drives status from the server
    pub async fn get_drives_metrics(&self) -> Result<Vec<Drives>, RedfishError> {
        let mut drives: Vec<Drives> = Vec::new();

        let storages: Vec<Storage> = self
            .get_collection(ODataId {
                odata_id: format!("/redfish/v1/Systems/{}/Storage/", self.system_id()),
            })
            .await
            .and_then(|c| c.try_get::<Storage>())
            .into_iter()
            .flat_map(|rc| rc.members)
            .collect();

        for storage in storages {
            if let Some(d) = storage.drives {
                for drive in d {
                    if drive.odata_id.contains("USB") {
                        continue;
                    }
                    let url = drive.odata_id.replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                    let (_, drive): (StatusCode, Drives) = self.client.get(&url).await?;

                    drives.push(drive);
                }
            }
        }
        Ok(drives)
    }

    pub async fn change_bios_password(
        &self,
        password_name: &str,
        current_bios_password: &str,
        new_bios_password: &str,
    ) -> Result<Option<String>, RedfishError> {
        let mut url = format!("Systems/{}/Bios/", self.system_id);

        match self.vendor {
            Some(RedfishVendor::Hpe) => {
                url = format!("{}Settings/Actions/Bios.ChangePasswords", url);
            }
            _ => {
                url = format!("{}Actions/Bios.ChangePassword", url);
            }
        }

        let mut arg = HashMap::new();
        arg.insert("PasswordName", password_name.to_string());
        arg.insert("OldPassword", current_bios_password.to_string());
        arg.insert("NewPassword", new_bios_password.to_string());
        self.client.post(&url, arg).await.map(|_resp| Ok(None))?
    }

    /// Query the network service settings for the server
    pub async fn get_manager_network_protocol(
        &self,
    ) -> Result<ManagerNetworkProtocol, RedfishError> {
        let url = format!("Managers/{}/NetworkProtocol", self.manager_id(),);
        let (_status_code, body) = self.client.get(&url).await?;
        Ok(body)
    }

    /// Set NTP servers via the standard ManagerNetworkProtocol resource.
    pub async fn set_manager_ntp_servers(&self, servers: &[String]) -> Result<(), RedfishError> {
        if servers.is_empty() {
            return Ok(());
        }

        let url = format!("Managers/{}/NetworkProtocol", self.manager_id());
        let ntp_servers = HashMap::from([(
            "NTP",
            json!({
                "NTPServers": servers,
                "ProtocolEnabled": true,
            }),
        )]);

        if matches!(
            self.vendor,
            Some(RedfishVendor::AMI | RedfishVendor::LenovoAMI | RedfishVendor::LenovoGB300)
        ) {
            match self.client.patch_with_if_match(&url, ntp_servers).await {
                Err(RedfishError::HTTPErrorCode {
                    status_code: StatusCode::ACCEPTED,
                    ..
                }) => Ok(()),
                result => result,
            }
        } else {
            self.client.patch(&url, ntp_servers).await.map(|_resp| ())
        }
    }

    pub async fn reset_manager(
        &self,
        reset_type: ManagerResetType,
        headers: Option<Vec<(HeaderName, String)>>,
    ) -> Result<(), RedfishError> {
        let url = format!("Managers/{}/Actions/Manager.Reset", self.manager_id);
        let mut arg = HashMap::new();
        // Dell only has GracefulRestart. The spec, and Lenovo, also have ForceRestart.
        // Response code 204 No Content is fine.
        arg.insert("ResetType", reset_type.to_string());
        self.client
            .post_with_headers(&url, arg, headers)
            .await
            .map(|_resp| Ok(()))?
    }

    pub async fn pcie_devices_for_chassis(
        &self,
        chassis_list: Vec<String>,
    ) -> Result<Vec<PCIeDevice>, RedfishError> {
        let mut devices = Vec::new();
        for chassis in chassis_list {
            let chassis_devices: Vec<PCIeDevice> = self
                .get_collection(ODataId {
                    odata_id: format!("/redfish/v1/Chassis/{}/PCIeDevices/", chassis),
                })
                .await
                .and_then(|c| c.try_get::<PCIeDevice>())
                .into_iter()
                .flat_map(|rc| rc.members)
                .filter(|d: &PCIeDevice| {
                    d.id.is_some()
                        && d.manufacturer.is_some()
                        && d.status.as_ref().is_some_and(|s| {
                            s.state
                                .as_ref()
                                .is_some_and(|s| s.to_ascii_lowercase().contains("enabled"))
                        })
                })
                .collect();
            devices.extend(chassis_devices);
        }

        devices.sort_unstable_by(|a, b| a.manufacturer.cmp(&b.manufacturer));
        Ok(devices)
    }
}

// Key/value pairs that different between these two sets of attributes
// The left needs to be a full map, but the right side only needs to support `get`.
fn attr_diff(
    l: &serde_json::Map<String, serde_json::Value>,
    r: &serde_json::Value,
) -> HashMap<String, serde_json::Value> {
    l.iter()
        .filter(|(k, v)| r.get(k) != Some(v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
