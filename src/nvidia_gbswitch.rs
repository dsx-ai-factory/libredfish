use reqwest::StatusCode;
use std::{collections::HashMap, path::Path, time::Duration};

use crate::model::certificate::Certificate;
use crate::model::component_integrity::ComponentIntegrities;
use crate::model::sensor::{GPUSensors, Sensors};
use crate::model::service_root::RedfishVendor;
use crate::model::task::Task;
use crate::model::thermal::{LeakDetector, Temperature, TemperaturesOemNvidia, Thermal};
use crate::model::update_service::ComponentType;
use crate::model::PCIeDevices;
use crate::REDFISH_ENDPOINT;
use crate::{
    model::{
        boot::{BootOverride, BootSourceOverrideEnabled, BootSourceOverrideTarget},
        chassis::{Assembly, NetworkAdapter},
        sel::{LogEntry, LogEntryCollection},
        BootOption,
    },
    standard::RedfishStandard,
    BiosProfileType, Chassis, NetworkDeviceFunction, Redfish, RedfishError,
};
use crate::{MachineSetupStatus, PCIeDevice};

const UEFI_PASSWORD_NAME: &str = "AdminPassword";

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

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Thermal, RedfishError>> {
        Box::pin(async move {
            let mut temperatures = Vec::new();
            let fans = Vec::new();
            let mut leak_detectors = Vec::new();

            // gb200 bianca has temperature sensors in several chassis items
            let chassis_all = self.s.get_chassis_all().await?;
            for chassis_id in chassis_all {
                if chassis_id != "MGX_NVSwitch_0" {
                    continue;
                }
                let mut url = format!("Chassis/{}", chassis_id);
                let (_status_code, chassis): (StatusCode, Chassis) =
                    self.s.client.get(&url).await?;
                if chassis.thermal_subsystem.is_some() {
                    url = format!("Chassis/{}/ThermalSubsystem/ThermalMetrics", chassis_id);
                    let (_status_code, temps): (StatusCode, TemperaturesOemNvidia) =
                        self.s.client.get(&url).await?;
                    if let Some(temp) = temps.temperature_readings_celsius {
                        for t in temp {
                            let sensor: Temperature = Temperature::from(t);
                            temperatures.push(sensor);
                        }
                    }
                    // walk through leak detection sensors and add those
                    url = format!(
                        "Chassis/{}/ThermalSubsystem/LeakDetection/LeakDetectors",
                        chassis_id
                    );

                    let res: Result<(StatusCode, Sensors), RedfishError> =
                        self.s.client.get(&url).await;

                    if let Ok((_, sensors)) = res {
                        for sensor in sensors.members {
                            url = sensor
                                .odata_id
                                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                            let (_status_code, l): (StatusCode, LeakDetector) =
                                self.s.client.get(&url).await?;
                            leak_detectors.push(l);
                        }
                    }
                }
            }
            let thermals = Thermal {
                temperatures,
                fans,
                leak_detectors: Some(leak_detectors),
                ..Default::default()
            };
            Ok(thermals)
        })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "No GPUs on the switch".to_string(),
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
        Box::pin(async move { Ok(None) })
    }

    fn machine_setup_status<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            let diffs = vec![];

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
                // 10 attempts before lockout. This is the default on GB Switch.
                ("AccountLockoutThreshold", Number(10.into())),
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
    ) -> crate::RedfishFuture<'a, Result<Vec<PCIeDevice>, RedfishError>> {
        Box::pin(async move {
            let mut out = Vec::new();

            // gb200 has pcie devices on several chassis items
            let chassis_all = self.s.get_chassis_all().await?;
            for chassis_id in chassis_all {
                if chassis_id.contains("BMC") {
                    continue;
                }

                let chassis = self.get_chassis(&chassis_id).await?;

                if let Some(member) = chassis.pcie_devices {
                    let mut url = member
                        .odata_id
                        .replace(&format!("/{REDFISH_ENDPOINT}/"), "");

                    let devices: PCIeDevices = match self.s.client.get(&url).await {
                        Ok((_status, x)) => x,
                        Err(_e) => {
                            continue;
                        }
                    };
                    for id in devices.members {
                        url = id.odata_id.replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                        let p: PCIeDevice = self.s.client.get(&url).await?.1;
                        // To be considered enabled, the PCIE device needs to have
                        // an ID, a status, and the status needs to be enabled.
                        let is_device_enabled = p.id.is_some()
                            && p.status.as_ref().is_some_and(|s| {
                                s.state
                                    .as_ref()
                                    .is_some_and(|state| state.to_lowercase().contains("enabled"))
                            });
                        if !is_device_enabled {
                            continue;
                        }
                        out.push(p);
                    }
                }
            }

            out.sort_unstable_by(|a, b| a.manufacturer.cmp(&b.manufacturer));
            Ok(out)
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
                "GB Switch firmware update unsupported".to_string(),
            ))
        })
    }

    fn bios<'a>(
        &'a self,
    ) -> crate::RedfishFuture<
        'a,
        Result<std::collections::HashMap<String, serde_json::Value>, RedfishError>,
    > {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch Bios unsupported".to_string(),
            ))
        })
    }

    fn set_bios<'a>(
        &'a self,
        _values: HashMap<String, serde_json::Value>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch Bios unsupported".to_string(),
            ))
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch Bios unsupported".to_string(),
            ))
        })
    }

    /// gb switch bios attributes?
    fn pending<'a>(
        &'a self,
    ) -> crate::RedfishFuture<
        'a,
        Result<std::collections::HashMap<String, serde_json::Value>, RedfishError>,
    > {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch Bios unsupported".to_string(),
            ))
        })
    }

    /// gh200 has no bios attributes
    fn clear_pending<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch Bios unsupported".to_string(),
            ))
        })
    }

    fn get_secure_boot<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::model::secure_boot::SecureBoot, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch secure boot unsupported".to_string(),
            ))
        })
    }

    fn enable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch secure boot unsupported".to_string(),
            ))
        })
    }

    fn disable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch secure boot unsupported".to_string(),
            ))
        })
    }

    fn add_secure_boot_certificate<'a>(
        &'a self,
        _pem_cert: &'a str,
        _database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch secure boot unsupported".to_string(),
            ))
        })
    }

    fn get_chassis_network_adapters<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB Switch doesn't have NetworkAdapters tree".to_string(),
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
                "GB Switch doesn't have NetworkAdapters tree".to_string(),
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
                "GB Switch doesn't have Systems EthernetInterface".to_string(),
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
                "GB Switch doesn't have NetworkAdapters tree".to_string(),
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
                "GB Switch doesn't have NetworkAdapters tree".to_string(),
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
                "GB Switch doesn't have NetworkAdapters tree".to_string(),
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
                "GB Switch doesn't have NetworkAdapters tree".to_string(),
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
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Not applicable to NVSwitch".to_string(),
            ))
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
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GBSwitch".to_string(),
            ))
        })
    }

    fn get_component_integrities<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GBSwitch".to_string(),
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
                "not populated for GBSwitch".to_string(),
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
                "not populated for GBSwitch".to_string(),
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
                "not populated for GBSwitch".to_string(),
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
                "not populated for GBSwitch".to_string(),
            ))
        })
    }

    fn get_chassis_assembly<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Assembly, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GBSwitch".to_string(),
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
                "not populated for GBSwitch".to_string(),
            ))
        })
    }

    fn get_secure_boot_certificates<'a>(
        &'a self,
        _database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GBSwitch".to_string(),
            ))
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GBSwitch".to_string(),
            ))
        })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "not populated for GBSwitch".to_string(),
            ))
        })
    }
}

impl Bmc {
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
