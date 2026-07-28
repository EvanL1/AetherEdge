use aether_example_energy_gateway::EnergyGateway;

fn main() {
    match EnergyGateway::bundled() {
        Ok(gateway) => {
            let summary = gateway.pack_summary();
            println!(
                "AetherEMS ready: pack={}, capabilities={}, example_channels={}, commissioned=0",
                summary.id,
                summary.capabilities.len(),
                summary.example_channel_count
            );
        },
        Err(error) => {
            eprintln!("cannot compose AetherEMS: {error}");
            std::process::exit(1);
        },
    }
}
