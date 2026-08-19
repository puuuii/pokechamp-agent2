// src/bin/list_devices.rs
use nokhwa::{query, utils::ApiBackend};

fn main() -> anyhow::Result<()> {
    let devices = query(ApiBackend::Auto)?;
    for dev in devices {
        println!("{:?}", dev);
    }
    Ok(())
}
