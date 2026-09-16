use crate::Assembly;
use std::{collections::HashMap, path::Path, time::Duration};
use tokio::fs::File;

use crate::model::boot::BootOverride;
use crate::model::certificate::Certificate;
use crate::model::component_integrity::ComponentIntegrities;
use crate::model::power::Power;
use crate::model::sensor::GPUSensors;
use crate::model::service_root::RedfishVendor;
use crate::model::task::Task;
use crate::model::update_service::ComponentType;
use crate::MachineSetupStatus;
use crate::{
    model::{
        chassis::NetworkAdapter,
        sel::{LogEntry, LogEntryCollection},
        BootOption, ComputerSystem,
    },
    standard::RedfishStandard,
    BiosProfileType, NetworkDeviceFunction, Redfish, RedfishError,
};

const UEFI_PASSWORD_NAME: &str = "AdminPassword";

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
    fn get_power_state<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::PowerState, RedfishError>> {
        Box::pin(async move {
            let system = self.get_system().await?;
            Ok(system.power_state)
        })
    }

    fn get_power_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Power, RedfishError>> {
        // Discover the chassis carrying the PowerSubsystem rather than
        // hard-coding the Lite-On-specific id.
        Box::pin(async move { self.s.get_power_metrics_from_power_subsystem().await })
    }

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Thermal, RedfishError>> {
        Box::pin(async move {
            let url = "Chassis/powershelf/Thermal/".to_string();
            let (_status_code, body) = self.s.client.get(&url).await?;
            Ok(body)
        })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("no gpus".to_string())) })
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
            // we don't do any changes for powershelves
            Ok(None)
        })
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
                // Never lock
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

    fn bmc_reset_to_defaults<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Managers/{}/Actions/Manager.ResetToDefaults",
                self.s.manager_id()
            );
            // The Lite-On PMC follows the DMTF Manager.ResetToDefaults schema and
            // requires the parameter named ResetToDefaultsType. The standard
            // implementation sends ResetType (which several other vendors name
            // their parameter), so the PMC rejects it with HTTP 400
            // PropertyUnknown/ActionParameterMissing.
            let mut arg = HashMap::new();
            arg.insert("ResetToDefaultsType", "ResetAll".to_string());
            self.s.client.post(&url, arg).await.map(|_resp| Ok(()))?
        })
    }

    fn get_boot_options<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::BootOptions, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn get_boot_option<'a>(
        &'a self,
        _option_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<BootOption, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn boot_once<'a>(
        &'a self,
        _target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn boot_first<'a>(
        &'a self,
        _target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn set_boot_override<'a>(
        &'a self,
        _settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf does not support boot source overrides".to_string(),
            ))
        })
    }

    fn pcie_devices<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<crate::PCIeDevice>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf doesn't have PCIeDevices tree".to_string(),
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

    fn get_system<'a>(&'a self) -> crate::RedfishFuture<'a, Result<ComputerSystem, RedfishError>> {
        Box::pin(async move {
            let mut system = self.s.get_system().await?;
            let power = self.get_power_metrics().await;
            system.power_state = power_state_from_psus(&power);
            Ok(system)
        })
    }

    fn add_secure_boot_certificate<'a>(
        &'a self,
        _pem_cert: &'a str,
        _database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf secure boot unsupported".to_string(),
            ))
        })
    }

    fn get_chassis_network_adapters<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf doesn't have NetworkAdapters tree".to_string(),
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
                "Lite-on powershelf  doesn't have NetworkAdapters tree".to_string(),
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
                "Lite-on powershelf doesn't have Systems EthernetInterface".to_string(),
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
                "Lite-on powershelf doesn't have NetworkAdapters tree".to_string(),
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
                "Lite-on powershelf doesn't have NetworkAdapters tree".to_string(),
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
                "Lite-on powershelf doesn't have NetworkAdapters tree".to_string(),
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
                "Lite-on powershelf doesn't have NetworkAdapters tree".to_string(),
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
        _boot_array: Vec<String>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Lite-on powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "set_dpu_first_boot_order".to_string(),
            ))
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn get_secure_boot_certificate<'a>(
        &'a self,
        _database_id: &'a str,
        _certificate_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Certificate, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_secure_boot_certificates<'a>(
        &'a self,
        _database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
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
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_component_integrities<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }
}

impl Bmc {
    async fn get_system_event_log(&self) -> Result<Vec<LogEntry>, RedfishError> {
        // there's an EventLog too, but its always returning Not found!
        let url = format!(
            "Managers/{}/LogServices/EventLog/Entries",
            self.s.manager_id()
        );
        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.s.client.get(&url).await?;
        let log_entries = log_entry_collection.members;
        Ok(log_entries)
    }
}

/// Derives the overall powershelf `PowerState` from individual PSU states.
/// All On → On, all Off → Off, mixed or no PSUs or fetch failure → Unknown.
fn power_state_from_psus(power: &Result<Power, RedfishError>) -> crate::PowerState {
    let supplies = match power {
        Ok(p) => p.power_supplies.as_deref().unwrap_or_default(),
        Err(_) => return crate::PowerState::Unknown,
    };

    if supplies.is_empty() {
        return crate::PowerState::Unknown;
    }

    let all_on = supplies
        .iter()
        .all(|ps| ps.power_state == Some(crate::PowerState::On));
    let all_off = supplies
        .iter()
        .all(|ps| ps.power_state == Some(crate::PowerState::Off));

    if all_on {
        crate::PowerState::On
    } else if all_off {
        crate::PowerState::Off
    } else {
        crate::PowerState::Unknown
    }
}
