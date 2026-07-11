use aether_example_minimal_gateway::MinimalGateway;
use aether_sdk::application::capability_catalog;

fn main() {
    match MinimalGateway::new() {
        Ok(_) => {
            println!(
                "Aether minimal gateway ready: {} capabilities, no external services",
                capability_catalog().len()
            );
        },
        Err(error) => {
            eprintln!("cannot compose minimal gateway: {error}");
            std::process::exit(1);
        },
    }
}
