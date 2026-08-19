use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};

fn main() -> Result<()> {
    let host = cpal::default_host();
    println!("== input devices ==");
    for d in host.input_devices()? {
        println!("{}", d.name()?);
    }
    println!("== output devices ==");
    for d in host.output_devices()? {
        println!("{}", d.name()?);
    }
    Ok(())
}
