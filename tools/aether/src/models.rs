//! Model management module
//!
//! Provides functionality to manage products and instances via HTTP API

use anyhow::Result;
use clap::Subcommand;
use tracing::info;

pub mod client;
pub mod csv_loader;

#[derive(Subcommand)]
pub enum ModelCommands {
    /// Manage products (device type templates)
    #[command(about = "Manage product definitions and templates")]
    Products {
        #[command(subcommand)]
        command: ProductCommands,
    },

    /// Manage instances (device configurations)
    #[command(about = "Manage device instances based on product templates")]
    Instances {
        #[command(subcommand)]
        command: InstanceCommands,
    },
}

#[derive(Subcommand)]
pub enum ProductCommands {
    /// List all built-in products
    #[command(about = "Show all built-in products from aether-model")]
    List,

    /// Show available products in products/ directory (for development)
    #[command(about = "List product definitions in the products/ directory")]
    Available,

    /// Get product details
    #[command(about = "Show detailed information about a built-in product")]
    Get {
        /// Product name
        name: String,
    },
}

#[derive(Subcommand)]
pub enum InstanceCommands {
    /// List all instances
    #[command(about = "Show all device instances")]
    List {
        /// Filter by product type
        #[arg(short, long)]
        product: Option<String>,
    },

    /// Create a new instance
    #[command(about = "Create a new device instance from a product template")]
    Create {
        /// Product name
        product: String,
        /// Instance name
        name: String,
        /// Properties in key=value format
        #[arg(short, long, value_parser = parse_property)]
        props: Vec<(String, String)>,
    },

    /// Get instance details
    #[command(about = "Show detailed information about an instance")]
    Get {
        /// Instance name
        name: String,
    },

    /// Update an instance
    #[command(about = "Update instance properties")]
    Update {
        /// Instance name
        name: String,
        /// Properties to update in key=value format
        #[arg(short, long, value_parser = parse_property)]
        props: Vec<(String, String)>,
    },

    /// Delete an instance
    #[command(about = "Delete a device instance")]
    Delete {
        /// Instance name
        name: String,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Get instance runtime data
    #[command(about = "Get realtime measurement and action values from SHM")]
    Data {
        /// Instance ID
        instance_id: u32,
        /// Point type filter (M for measurements, A for actions, both if not specified)
        #[arg(short = 't', long)]
        point_type: Option<String>,
    },

    /// Execute a control action on an instance
    #[command(about = "Execute a control action on an instance (writes to the device)")]
    Action {
        /// Instance ID
        instance_id: u32,
        /// Numeric action point ID encoded as a string (for example, "1")
        #[arg(long)]
        point_id: String,
        /// Value to write
        #[arg(long)]
        value: f64,
    },

    /// Set a measurement value on an instance
    #[command(about = "Set a measurement value on an instance")]
    Measurement {
        /// Instance ID
        instance_id: u32,
        /// Point ID: numeric ("101") or semantic name
        #[arg(long)]
        point_id: String,
        /// Value to set
        #[arg(long)]
        value: f64,
    },
}

fn parse_property(s: &str) -> Result<(String, String), String> {
    let parts: Vec<&str> = s.splitn(2, '=').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid property format: '{}'. Expected key=value",
            s
        ));
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

pub async fn handle_command(cmd: ModelCommands, base_url: &str, json: bool) -> Result<()> {
    match cmd {
        ModelCommands::Products { command } => {
            handle_product_command(command, base_url, json).await
        },
        ModelCommands::Instances { command } => {
            handle_instance_command(command, base_url, json).await
        },
    }
}

async fn handle_product_command(cmd: ProductCommands, base_url: &str, json: bool) -> Result<()> {
    match cmd {
        ProductCommands::Available => {
            if json {
                eprintln!("warning: --json is not fully supported for 'products available'");
            }
            csv_loader::list_available_products()?;
        },
        ProductCommands::List => {
            let client = client::ModelClient::new(base_url)?;
            let products = client.list_products().await?;
            if json {
                crate::output::print_success(&products);
            } else {
                println!("Products: {}", serde_json::to_string_pretty(&products)?);
            }
        },
        ProductCommands::Get { name } => {
            let client = client::ModelClient::new(base_url)?;
            let product = client.get_product(&name).await?;
            if json {
                crate::output::print_success(&product);
            } else {
                println!(
                    "Product '{}': {}",
                    name,
                    serde_json::to_string_pretty(&product)?
                );
            }
        },
    }
    Ok(())
}

async fn handle_instance_command(cmd: InstanceCommands, base_url: &str, json: bool) -> Result<()> {
    let client = client::ModelClient::new(base_url)?;

    match cmd {
        InstanceCommands::List { product } => {
            let instances = client.list_instances(product.as_deref()).await?;
            if json {
                crate::output::print_success(&instances);
            } else {
                println!("Instances: {}", serde_json::to_string_pretty(&instances)?);
            }
        },
        InstanceCommands::Create {
            product,
            name,
            props,
        } => {
            let props_map: std::collections::HashMap<String, String> = props.into_iter().collect();
            client.create_instance(&product, &name, props_map).await?;
            if json {
                crate::output::print_ok();
            } else {
                info!("Instance '{}' created", name);
            }
        },
        InstanceCommands::Get { name } => {
            let instance = client.get_instance(&name).await?;
            if json {
                crate::output::print_success(&instance);
            } else {
                println!(
                    "Instance '{}': {}",
                    name,
                    serde_json::to_string_pretty(&instance)?
                );
            }
        },
        InstanceCommands::Update { name, props } => {
            let props_map: std::collections::HashMap<String, String> = props.into_iter().collect();
            client.update_instance(&name, props_map).await?;
            if json {
                crate::output::print_ok();
            } else {
                info!("Instance '{}' updated", name);
            }
        },
        InstanceCommands::Delete { name, force } => {
            // In json mode, skip interactive confirmation (agents can't prompt)
            if !force && !json {
                println!("Delete instance '{}'? [y/N]", name);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
            }

            client.delete_instance(&name).await?;
            if json {
                crate::output::print_ok();
            } else {
                info!("Instance '{}' deleted", name);
            }
        },
        InstanceCommands::Data {
            instance_id,
            point_type,
        } => {
            let data_type = match point_type.as_deref() {
                None => None,
                Some("M" | "m" | "measurement") => Some("measurement"),
                Some("A" | "a" | "action") => Some("action"),
                Some(other) => {
                    anyhow::bail!("invalid point type '{other}'; use M/measurement or A/action")
                },
            };
            let data = client.get_instance_data(instance_id, data_type).await?;
            if json {
                crate::output::print_success(&data);
            } else {
                println!("{}", serde_json::to_string_pretty(&data)?);
            }
        },
        InstanceCommands::Action {
            instance_id,
            point_id,
            value,
        } => {
            let data = client.execute_action(instance_id, &point_id, value).await?;
            crate::output::print_action(
                &data,
                &format!("Action executed on instance {instance_id} point {point_id}: {value}"),
                json,
            );
        },
        InstanceCommands::Measurement {
            instance_id,
            point_id,
            value,
        } => {
            let data = client
                .set_measurement(instance_id, &point_id, value)
                .await?;
            crate::output::print_action(
                &data,
                &format!("Measurement set on instance {instance_id} point {point_id}: {value}"),
                json,
            );
        },
    }
    Ok(())
}
