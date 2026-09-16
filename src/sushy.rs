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
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR
 * OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
 * ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
 * OTHER DEALINGS IN THE SOFTWARE.
 */

//! Sushy/redvirt Redfish emulator vendor implementation.
//!
//! Sushy (sushy-tools) is the OpenStack Redfish emulator backed by libvirt.
//! This vendor provides no-op overrides for BMC operations that Sushy doesn't
//! support (BIOS configuration, password management, boot order, etc.),
//! allowing Carbide to manage Sushy-emulated machines for development and testing.
//!
//! Sushy reports `"Vendor": "Sushy"` in the Redfish ServiceRoot.
//! The aliases "Contoso" and "Redvirt" are also accepted.

use crate::Assembly;
use std::{collections::HashMap, path::Path, time::Duration};
use tokio::fs::File;
use tracing::info;

use crate::model::account_service::ManagerAccount;
use crate::model::component_integrity::ComponentIntegrities;
use crate::model::sensor::GPUSensors;
use crate::model::service_root::RedfishVendor;
use crate::model::task::Task;
use crate::model::update_service::ComponentType;
use crate::{
    standard::RedfishStandard, BiosProfileType, MachineSetupStatus, Redfish, RedfishError, RoleId,
};

pub struct Bmc {
    standard: RedfishStandard,
}

impl Bmc {
    pub fn new(standard: RedfishStandard) -> Result<Self, RedfishError> {
        Ok(Self { standard })
    }
}

impl Redfish for Bmc {
    fn std_redfish(&self) -> &RedfishStandard {
        &self.standard
    }

    fn create_user<'a>(
        &'a self,
        _username: &'a str,
        _password: &'a str,
        _role_id: RoleId,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping create_user (emulator has no AccountService)");
            Ok(())
        })
    }

    fn delete_user<'a>(
        &'a self,
        _username: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping delete_user (emulator has no AccountService)");
            Ok(())
        })
    }

    fn change_username<'a>(
        &'a self,
        _old_name: &'a str,
        _new_name: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping change_username (emulator has no AccountService)");
            Ok(())
        })
    }

    fn change_password<'a>(
        &'a self,
        _user: &'a str,
        _new: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping change_password (emulator)");
            Ok(())
        })
    }

    fn change_password_by_id<'a>(
        &'a self,
        _account_id: &'a str,
        _new_pass: &'a str,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping change_password_by_id (emulator)");
            Ok(())
        })
    }

    fn get_accounts<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<ManagerAccount>, RedfishError>> {
        Box::pin(async move {
            info!("Sushy: returning empty accounts (emulator has no AccountService)");
            Ok(vec![])
        })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("no gpus".to_string())) })
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
            info!("Sushy: skipping machine_setup (emulator)");
            Ok(None)
        })
    }

    fn machine_setup_status<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            Ok(MachineSetupStatus {
                is_done: true,
                diffs: vec![],
            })
        })
    }

    fn set_machine_password_policy<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping set_machine_password_policy (emulator)");
            Ok(())
        })
    }

    fn lockdown<'a>(
        &'a self,
        _target: crate::EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Ok(()) })
    }

    fn lockdown_status<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Status, RedfishError>> {
        Box::pin(async move {
            info!("Sushy: reporting lockdown as enabled (emulator)");
            Ok(crate::Status {
                status: crate::StatusInternal::Enabled,
                message: "Sushy emulator".to_string(),
            })
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

            let update_service = self.standard.get_update_service().await?;

            if update_service.multipart_http_push_uri.is_empty() {
                return Err(RedfishError::NotSupported(
                    "Host BMC does not support HTTP multipart push".to_string(),
                ));
            }

            let parameters = "{}".to_string();

            let (_status_code, _loc, body) = self
                .standard
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

    fn set_bios<'a>(
        &'a self,
        _values: HashMap<String, serde_json::Value>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping set_bios (emulator has no BIOS)");
            Ok(())
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping reset_bios (emulator has no BIOS)");
            Ok(())
        })
    }

    fn change_uefi_password<'a>(
        &'a self,
        _current_uefi_password: &'a str,
        _new_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping change_uefi_password (emulator)");
            Ok(None)
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping set_boot_order_dpu_first (emulator)");
            Ok(None)
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn add_secure_boot_certificate<'a>(
        &'a self,
        _pem_cert: &'a str,
        _database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Sushy emulator secure boot unsupported".to_string(),
            ))
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { Ok(true) })
    }

    fn is_infinite_boot_enabled<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        Box::pin(async move { Ok(Some(true)) })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            info!("Sushy: skipping enable_infinite_boot (emulator)");
            Ok(())
        })
    }

    fn trigger_evidence_collection<'a>(
        &'a self,
        _url: &'a str,
        _nonce: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_evidence<'a>(
        &'a self,
        _url: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::model::component_integrity::Evidence, RedfishError>>
    {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_firmware_for_component<'a>(
        &'a self,
        _component_integrity_id: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::software_inventory::SoftwareInventory, RedfishError>,
    > {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_component_ca_certificate<'a>(
        &'a self,
        _url: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::component_integrity::CaCertificate, RedfishError>,
    > {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_chassis_assembly<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Assembly, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        false
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { Ok(true) })
    }

    fn get_component_integrities<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }
}
