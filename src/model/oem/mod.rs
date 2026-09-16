use serde::{Deserialize, Serialize};

pub mod dell;
pub mod hpe;
pub mod lenovo;
pub mod nvidia_dpu;
pub mod nvidia_openbmc;
pub mod nvidia_viking;
pub mod supermicro;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ManagerExtensions {
    pub dell: Option<dell::Manager>,
    pub lenovo: Option<lenovo::Manager>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SystemExtensions {
    pub dell: Option<dell::SystemWrapper>,
    pub lenovo: Option<lenovo::System>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ChassisExtensions {
    pub nvidia: Option<nvidia_openbmc::ChassisExtensions>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PortExtensions {
    pub lenovo: Option<lenovo::Port>,
}
