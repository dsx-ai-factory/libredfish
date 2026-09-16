use std::{collections::HashMap, path::Path, time::Duration};

use tokio::fs::File;

use crate::model::certificate::Certificate;
use crate::model::component_integrity::ComponentIntegrities;
use crate::model::sensor::GPUSensors;
use crate::model::service_root::RedfishVendor;
use crate::model::task::Task;
use crate::model::update_service::ComponentType;
use crate::Boot::UefiHttp;
use crate::{
    model::{
        boot::{self, BootOverride, BootSourceOverrideEnabled, BootSourceOverrideTarget},
        chassis::{Assembly, NetworkAdapter},
        sel::{LogEntry, LogEntryCollection},
        BootOption, ComputerSystem,
    },
    standard::RedfishStandard,
    BiosProfileType, NetworkDeviceFunction, Redfish, RedfishError,
};
use crate::{MachineSetupDiff, MachineSetupStatus};

const UEFI_PASSWORD_NAME: &str = "AdminPassword";

fn secure_boot_diffs(secure_boot_enabled: bool) -> Vec<MachineSetupDiff> {
    let mut diffs = Vec::new();

    if secure_boot_enabled {
        diffs.push(MachineSetupDiff {
            key: "SecureBoot".to_string(),
            expected: "false".to_string(),
            actual: "true".to_string(),
        });
    }

    diffs
}

fn dpu_http_boot_option_name(boot_interface_mac: &str) -> String {
    let mac = boot_interface_mac.replace(':', "").to_uppercase();
    format!("{} (MAC:{mac})", BootOptionName::Http.to_string())
}

fn find_dpu_http_boot_option<'a>(
    boot_options: &'a [BootOption],
    boot_interface_mac: &str,
) -> Option<&'a BootOption> {
    let expected = dpu_http_boot_option_name(boot_interface_mac);
    boot_options
        .iter()
        .find(|option| option.display_name.eq_ignore_ascii_case(&expected))
}

fn compare_boot_order(
    boot_order: &[String],
    boot_options: &[BootOption],
    boot_interface_mac: &str,
) -> Vec<MachineSetupDiff> {
    let target = find_dpu_http_boot_option(boot_options, boot_interface_mac);
    let actual_first_reference = boot_order
        .first()
        .map(|entry| boot::boot_order_entry_reference(entry));
    if target
        .is_some_and(|option| actual_first_reference == Some(option.boot_option_reference.as_str()))
    {
        return Vec::new();
    }

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
    vec![MachineSetupDiff {
        key: "boot_first".to_string(),
        expected,
        actual,
    }]
}

pub struct Bmc {
    s: RedfishStandard,
}

impl Bmc {
    pub fn new(s: RedfishStandard) -> Result<Bmc, RedfishError> {
        Ok(Bmc { s })
    }
}

#[derive(Copy, Clone)]
pub enum BootOptionName {
    Http,
    Pxe,
    UefiHd,
}

impl BootOptionName {
    fn to_string(self) -> &'static str {
        match self {
            BootOptionName::Http => "UEFI HTTPv4",
            BootOptionName::Pxe => "UEFI PXEv4",
            BootOptionName::UefiHd => "HD(",
        }
    }
}

enum BootOptionMatchField {
    DisplayName,
    UefiDevicePath,
}
impl Redfish for Bmc {
    fn std_redfish(&self) -> &RedfishStandard {
        &self.s
    }
    fn get_power_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Power, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 PowerSubsystem not populated".to_string(),
            ))
        })
    }

    fn power<'a>(
        &'a self,
        action: crate::SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if action == crate::SystemPowerControl::ACPowercycle {
                let args: HashMap<String, String> =
                    HashMap::from([("ResetType".to_string(), "AuxPowerCycle".to_string())]);
                return self
                    .s
                    .client
                    .post(
                        "Chassis/BMC_0/Actions/Oem/NvidiaChassis.AuxPowerReset",
                        args,
                    )
                    .await
                    .map(|_status_code| ());
            }

            self.s.power(action).await
        })
    }

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Thermal, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 Thermal not populated".to_string(),
            ))
        })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "get_gpu_sensors not implemented".to_string(),
            ))
        })
    }

    fn get_system_event_log<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move { self.get_system_event_log().await })
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
            self.disable_secure_boot().await?;
            self.boot_once(UefiHttp).await?;
            Ok(None)
        })
    }

    fn machine_setup_status<'a>(
        &'a self,
        boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            let sb = self.get_secure_boot().await?;
            let mut diffs = secure_boot_diffs(sb.secure_boot_enable.unwrap_or(false));
            if let Some(boot_interface) = boot_interface {
                let mac = crate::resolve_boot_interface_mac(self, boot_interface).await?;
                diffs.extend(self.boot_order_diffs(&mac).await?);
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
            // These are also the defaults
            let body = HashMap::from([
                // Never lock
                ("AccountLockoutThreshold", Number(0.into())),
                // 600 is the smallest value it will accept. 10 minutes, in seconds.
                ("AccountLockoutDuration", Number(600.into())),
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
        _target: crate::EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // OpenBMC does not provide a lockdown
            // carbide calls this so don't return an error, otherwise GH200 would need special handling
            Ok(())
        })
    }

    fn boot_once<'a>(
        &'a self,
        target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // UefiHttp isn't in the GH200's list of AllowableValues, but it seems to work.
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
                crate::Boot::Pxe => self.set_boot_order(BootOptionName::Pxe).await,
                crate::Boot::HardDisk => {
                    // We're looking for a UefiDevicePath like this:
                    // HD(1,GPT,A04D0F1E-E02F-4725-9434-0699B52D8FF2,0x800,0x100000)/\\EFI\\ubuntu\\shimaa64.efi
                    // The DisplayName will be something like "ubuntu".
                    let boot_array = self
                        .get_boot_options_ids_with_first(
                            BootOptionName::UefiHd,
                            BootOptionMatchField::UefiDevicePath,
                        )
                        .await?;
                    self.change_boot_order(boot_array).await
                }
                crate::Boot::UefiHttp => self.set_boot_order(BootOptionName::Http).await,
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
            if let Some(mode) = settings.mode {
                boot_data.insert(
                    "BootSourceOverrideMode".to_string(),
                    mode.to_string().into(),
                );
            }
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

    fn pcie_devices<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<crate::PCIeDevice>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 doesn't have PCIeDevices tree".to_string(),
            ))
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

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.factory_reset_bios().await })
    }

    fn get_chassis_network_adapters<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 doesn't have NetworkAdapters tree".to_string(),
            ))
        })
    }

    fn get_chassis_network_adapter<'a>(
        &'a self,
        _chassis_id: &'a str,
        _id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 doesn't have NetworkAdapters tree".to_string(),
            ))
        })
    }

    fn get_system_ethernet_interfaces<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Ok(vec![]) })
    }

    fn get_system_ethernet_interface<'a>(
        &'a self,
        _id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::EthernetInterface, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 doesn't have Systems EthernetInterface".to_string(),
            ))
        })
    }

    fn get_ports<'a>(
        &'a self,
        _chassis_id: &'a str,
        _network_adapter: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 doesn't have NetworkAdapters tree".to_string(),
            ))
        })
    }

    fn get_port<'a>(
        &'a self,
        _chassis_id: &'a str,
        _network_adapter: &'a str,
        _id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::NetworkPort, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 doesn't have NetworkAdapters tree".to_string(),
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
                "GH200 doesn't have NetworkAdapters tree".to_string(),
            ))
        })
    }

    /// http://redfish.dmtf.org/schemas/v1/NetworkDeviceFunctionCollection.json
    fn get_network_device_functions<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GH200 doesn't have NetworkAdapters tree".to_string(),
            ))
        })
    }

    // Set current_uefi_password to "" if there isn't one yet. By default there isn't a password.
    /// Set new_uefi_password to "" to disable it.
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
            let url = format!("Systems/{}/Settings", self.s.system_id());
            self.s.client.patch(&url, body).await?;
            Ok(())
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mac = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            let (system, boot_options) = self.get_system_and_boot_options().await?;
            let target = find_dpu_http_boot_option(&boot_options, &mac).ok_or_else(|| {
                RedfishError::MissingBootOption(format!(
                    "No boot option matching {}",
                    dpu_http_boot_option_name(&mac)
                ))
            })?;
            let mut boot_order = system.boot.boot_order;
            let target_reference = &target.boot_option_reference;
            if boot_order
                .first()
                .is_some_and(|entry| boot::boot_order_entry_reference(entry) == target_reference)
            {
                return Ok(None);
            }
            if !boot::promote_boot_order_entry_first(&mut boot_order, target_reference) {
                boot_order.insert(0, target_reference.clone());
            }
            self.change_boot_order(boot_order).await?;
            Ok(None)
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        false
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let mac = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            Ok(self.boot_order_diffs(&mac).await?.is_empty())
        })
    }

    fn get_component_integrities<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }

    fn get_firmware_for_component<'a>(
        &'a self,
        _component_integrity_id: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::software_inventory::SoftwareInventory, RedfishError>,
    > {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }

    fn get_component_ca_certificate<'a>(
        &'a self,
        _url: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::component_integrity::CaCertificate, RedfishError>,
    > {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }

    fn trigger_evidence_collection<'a>(
        &'a self,
        _url: &'a str,
        _nonce: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }

    fn get_evidence<'a>(
        &'a self,
        _url: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::model::component_integrity::Evidence, RedfishError>>
    {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }

    fn get_chassis_assembly<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Assembly, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }

    fn get_secure_boot_certificate<'a>(
        &'a self,
        _database_id: &'a str,
        _certificate_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Certificate, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }

    fn get_secure_boot_certificates<'a>(
        &'a self,
        _database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let secure_boot = self.get_secure_boot().await?;
            Ok(secure_boot_diffs(secure_boot.secure_boot_enable.unwrap_or(false)).is_empty())
        })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GH200".to_string(),
            ))
        })
    }
}

impl Bmc {
    async fn get_system_and_boot_options(
        &self,
    ) -> Result<(ComputerSystem, Vec<BootOption>), RedfishError> {
        let system = self.s.get_system().await?;
        let boot_options_id =
            system
                .boot
                .boot_options
                .clone()
                .ok_or_else(|| RedfishError::MissingKey {
                    key: "boot.boot_options".to_string(),
                    url: system.odata.odata_id.clone(),
                })?;
        let boot_options = self
            .get_collection(boot_options_id)
            .await
            .and_then(|collection| collection.try_get::<BootOption>())?
            .members;
        Ok((system, boot_options))
    }

    async fn boot_order_diffs(
        &self,
        boot_interface_mac: &str,
    ) -> Result<Vec<MachineSetupDiff>, RedfishError> {
        let (system, boot_options) = self.get_system_and_boot_options().await?;
        Ok(compare_boot_order(
            &system.boot.boot_order,
            &boot_options,
            boot_interface_mac,
        ))
    }

    // name: The name of the device you want to make the first boot choice.
    async fn set_boot_order(&self, name: BootOptionName) -> Result<(), RedfishError> {
        let boot_array = self
            .get_boot_options_ids_with_first(name, BootOptionMatchField::DisplayName)
            .await?;
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
        with_name: BootOptionName,
        match_field: BootOptionMatchField,
    ) -> Result<Vec<String>, RedfishError> {
        let with_name_str = with_name.to_string();
        let mut ordered = Vec::new(); // the final boot options
        let boot_options = self.s.get_system().await?.boot.boot_order;
        for member in boot_options {
            let b: BootOption = self.s.get_boot_option(member.as_str()).await?;
            let is_match = match match_field {
                BootOptionMatchField::DisplayName => b.display_name.starts_with(with_name_str),
                BootOptionMatchField::UefiDevicePath => {
                    matches!(b.uefi_device_path, Some(x) if x.starts_with(with_name_str))
                }
            };
            if is_match {
                ordered.insert(0, b.id);
            } else {
                ordered.push(b.id);
            }
        }
        Ok(ordered)
    }

    async fn get_system_event_log(&self) -> Result<Vec<LogEntry>, RedfishError> {
        let url = format!("Systems/{}/LogServices/SEL/Entries", self.s.system_id());
        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.s.client.get(&url).await?;
        let log_entries = log_entry_collection.members;
        Ok(log_entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_without_boot_target_preserves_secure_boot_status() {
        assert!(secure_boot_diffs(false).is_empty());

        let diffs = secure_boot_diffs(true);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].key, "SecureBoot");
    }

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
    fn exact_http_boot_option_is_verified_by_mac_and_reference() {
        let target_name = "UEFI HTTPv4 (MAC:58A2E1BBB10F)";
        let options = vec![
            boot_option("Boot0001", "UEFI Hard Drive"),
            boot_option("Boot0020", target_name),
        ];

        let diffs = compare_boot_order(
            &["Boot0020: Selected network adapter".to_string()],
            &options,
            "58:a2:e1:bb:b1:0f",
        );

        assert!(diffs.is_empty());
    }

    #[test]
    fn different_first_boot_option_is_reported() {
        let target_name = "UEFI HTTPv4 (MAC:58A2E1BBB10F)";
        let options = vec![
            boot_option("Boot0001", "UEFI Hard Drive"),
            boot_option("Boot0020", target_name),
        ];

        let diffs = compare_boot_order(
            &["Boot0001".to_string(), "Boot0020".to_string()],
            &options,
            "58:A2:E1:BB:B1:0F",
        );

        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].key, "boot_first");
        assert_eq!(diffs[0].expected, target_name);
        assert_eq!(diffs[0].actual, "UEFI Hard Drive");
    }
}
