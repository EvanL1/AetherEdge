//! Model management module
//!
//! Provides functionality to manage products and instances via HTTP API

use anyhow::Result;
use clap::Subcommand;
use serde_json::Value;
use std::collections::HashMap;
use tracing::info;

pub mod client;

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
    /// List products selected by active Packs and site configuration
    #[command(about = "Show products selected by aether-automation")]
    List,

    /// Get product details
    #[command(about = "Show detailed information about a selected product")]
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
        product_name: String,
        /// Instance name
        instance_name: String,
        /// Optional explicit instance ID; omit to allocate one
        #[arg(long)]
        instance_id: Option<u32>,
        /// Properties as key=<JSON literal>; JSON strings must be quoted
        #[arg(short, long, value_parser = parse_property)]
        props: Vec<(String, Value)>,
        /// Current instances revision from GET /api/instances/revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this desired-state mutation
        #[arg(long)]
        confirmed: bool,
    },

    /// Get instance details
    #[command(about = "Show detailed information about an instance")]
    Get {
        /// Instance ID
        instance_id: u32,
    },

    /// Update an instance
    #[command(about = "Rename an instance and/or replace its properties")]
    Update {
        /// Instance ID
        instance_id: u32,
        /// New instance name
        #[arg(long)]
        instance_name: Option<String>,
        /// Replacement properties as key=<JSON literal>; JSON strings must be quoted
        #[arg(short, long, value_parser = parse_property)]
        props: Vec<(String, Value)>,
        /// Current instances revision from GET /api/instances/revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this desired-state mutation
        #[arg(long)]
        confirmed: bool,
    },

    /// Delete an instance
    #[command(about = "Delete a device instance")]
    Delete {
        /// Instance ID
        instance_id: u32,
        /// Skip the interactive prompt; --confirmed is still required
        #[arg(short, long)]
        force: bool,
        /// Current instances revision from GET /api/instances/revision
        #[arg(long)]
        expected_revision: u64,
        /// Explicitly confirm this desired-state mutation
        #[arg(long)]
        confirmed: bool,
    },

    /// Get instance runtime data
    #[command(about = "Get realtime measurement and action values from SHM")]
    Data {
        /// Instance ID
        instance_id: u32,
        /// Point type filter (measurement or action; both if omitted)
        #[arg(short = 't', long, value_enum)]
        point_type: Option<InstanceDataType>,
    },

    /// Execute a control action on an instance
    #[command(about = "Submit a confirmed control action to the local command plane")]
    Action {
        /// Instance ID
        instance_id: u32,
        /// Numeric action point ID encoded as a string (for example, "1")
        #[arg(long)]
        point_id: String,
        /// Value to write
        #[arg(long)]
        value: f64,
        /// Explicitly confirm this high-risk device command
        #[arg(long)]
        confirmed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InstanceDataType {
    Measurement,
    Action,
}

fn parse_property(s: &str) -> Result<(String, Value), String> {
    let (key, raw_value) = s
        .split_once('=')
        .ok_or_else(|| format!("Invalid property format: '{s}'. Expected key=<JSON literal>"))?;
    if key.is_empty() {
        return Err("Property key must not be empty".to_string());
    }
    let value = serde_json::from_str(raw_value).map_err(|error| {
        format!("Invalid JSON literal for property '{key}': {error}. JSON strings must be quoted")
    })?;
    Ok((key.to_string(), value))
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
            product_name,
            instance_name,
            instance_id,
            props,
            expected_revision,
            confirmed,
        } => {
            let properties: HashMap<String, Value> = props.into_iter().collect();
            client
                .create_instance(
                    instance_id,
                    &instance_name,
                    &product_name,
                    properties,
                    expected_revision,
                    confirmed,
                )
                .await?;
            if json {
                crate::output::print_ok();
            } else {
                info!("Instance '{}' created", instance_name);
            }
        },
        InstanceCommands::Get { instance_id } => {
            let instance = client.get_instance(instance_id).await?;
            if json {
                crate::output::print_success(&instance);
            } else {
                println!(
                    "Instance '{}': {}",
                    instance_id,
                    serde_json::to_string_pretty(&instance)?
                );
            }
        },
        InstanceCommands::Update {
            instance_id,
            instance_name,
            props,
            expected_revision,
            confirmed,
        } => {
            let properties = (!props.is_empty()).then(|| props.into_iter().collect());
            client
                .update_instance(
                    instance_id,
                    instance_name.as_deref(),
                    properties,
                    expected_revision,
                    confirmed,
                )
                .await?;
            if json {
                crate::output::print_ok();
            } else {
                info!("Instance '{}' updated", instance_id);
            }
        },
        InstanceCommands::Delete {
            instance_id,
            force,
            expected_revision,
            confirmed,
        } => {
            // In json mode, skip interactive confirmation (agents can't prompt)
            if !force && !json {
                println!("Delete instance '{}'? [y/N]", instance_id);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
            }

            client
                .delete_instance(instance_id, expected_revision, confirmed)
                .await?;
            if json {
                crate::output::print_ok();
            } else {
                info!("Instance '{}' deleted", instance_id);
            }
        },
        InstanceCommands::Data {
            instance_id,
            point_type,
        } => {
            let data_type = match point_type {
                None => None,
                Some(InstanceDataType::Measurement) => Some("measurement"),
                Some(InstanceDataType::Action) => Some("action"),
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
            confirmed,
        } => {
            let data = client
                .execute_action(instance_id, &point_id, value, confirmed)
                .await?;
            crate::output::print_action(
                &data,
                &format!(
                    "Local command plane accepted instance {instance_id} point {point_id}: {value}"
                ),
                json,
            );
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{InstanceCommands, InstanceDataType, ModelCommands, ProductCommands};

    #[derive(Parser)]
    struct ModelsCli {
        #[command(subcommand)]
        command: ModelCommands,
    }

    #[test]
    fn products_exposes_only_automation_backed_commands() {
        assert!(ModelsCli::try_parse_from(["models", "products", "available"]).is_err());
        let parsed = ModelsCli::try_parse_from(["models", "products", "list"])
            .expect("canonical product list command");
        assert!(matches!(
            parsed.command,
            ModelCommands::Products {
                command: ProductCommands::List
            }
        ));
    }

    #[test]
    fn instance_data_point_type_accepts_only_canonical_words() {
        for (value, expected) in [
            ("measurement", InstanceDataType::Measurement),
            ("action", InstanceDataType::Action),
        ] {
            let parsed = ModelsCli::try_parse_from([
                "models",
                "instances",
                "data",
                "9",
                "--point-type",
                value,
            ])
            .expect("canonical point type");
            let ModelCommands::Instances {
                command:
                    InstanceCommands::Data {
                        point_type: Some(actual),
                        ..
                    },
            } = parsed.command
            else {
                panic!("expected instance data command")
            };
            assert_eq!(actual, expected);
        }

        for retired in ["M", "m", "A", "a"] {
            assert!(
                ModelsCli::try_parse_from([
                    "models",
                    "instances",
                    "data",
                    "9",
                    "--point-type",
                    retired,
                ])
                .is_err(),
                "retired point type {retired} was accepted"
            );
        }
    }

    #[test]
    fn instance_create_parses_canonical_identity_governance_and_json_properties() {
        let parsed = ModelsCli::try_parse_from([
            "models",
            "instances",
            "create",
            "pump",
            "pump-1",
            "--instance-id",
            "9",
            "--props",
            "capacity=100",
            "--props",
            r#"owner="ops""#,
            "--expected-revision",
            "7",
            "--confirmed",
        ])
        .expect("canonical create command");
        let ModelCommands::Instances {
            command:
                InstanceCommands::Create {
                    product_name,
                    instance_name,
                    instance_id,
                    props,
                    expected_revision,
                    confirmed,
                },
        } = parsed.command
        else {
            panic!("expected create command")
        };
        assert_eq!(product_name, "pump");
        assert_eq!(instance_name, "pump-1");
        assert_eq!(instance_id, Some(9));
        assert_eq!(props[0].1, serde_json::json!(100));
        assert_eq!(props[1].1, serde_json::json!("ops"));
        assert_eq!(expected_revision, 7);
        assert!(confirmed);
    }

    #[test]
    fn properties_reject_unquoted_string_values() {
        let error = ModelsCli::try_parse_from([
            "models",
            "instances",
            "create",
            "pump",
            "pump-1",
            "--props",
            "owner=ops",
            "--expected-revision",
            "7",
            "--confirmed",
        ])
        .err()
        .expect("unquoted JSON string must fail closed");
        assert!(error.to_string().contains("JSON strings must be quoted"));
    }

    #[test]
    fn instance_identity_commands_reject_retired_name_paths() {
        for args in [
            vec!["models", "instances", "get", "pump-1"],
            vec![
                "models",
                "instances",
                "update",
                "pump-1",
                "--props",
                "capacity=100",
                "--expected-revision",
                "7",
                "--confirmed",
            ],
            vec![
                "models",
                "instances",
                "delete",
                "pump-1",
                "--expected-revision",
                "7",
                "--confirmed",
            ],
        ] {
            assert!(ModelsCli::try_parse_from(args).is_err());
        }
    }
}
