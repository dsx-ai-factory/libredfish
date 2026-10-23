// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: MIT

use std::{env, error::Error, fs};

use libredfish::{Endpoint, Redfish, RedfishClientPool};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let endpoint = Endpoint {
        host: env::var("REDFISH_HOST")?,
        port: env::var("REDFISH_PORT")
            .ok()
            .map(|port| port.parse::<u16>())
            .transpose()?,
        user: Some(env::var("REDFISH_USER")?),
        password: Some(env::var("REDFISH_PASSWORD")?),
    };

    // Server certificate and hostname verification remain enabled.
    let mut builder = RedfishClientPool::builder();
    if let Some(path) = env::var_os("REDFISH_CA_CERT") {
        builder = builder.add_root_certificates(fs::read(path)?);
    }
    let pool = builder.build()?;
    let client = pool.create_standard_client(endpoint)?;
    let root = client.get_service_root().await?;
    println!("Redfish version: {}", root.redfish_version);

    Ok(())
}
