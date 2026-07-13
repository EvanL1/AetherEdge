use std::env;
use std::error::Error;
use std::path::PathBuf;

use aether_runtime_catalog::{
    KernelRuntimeManifest, default_io_features, load_runtime_manifest, load_runtime_manifest_file,
    normalize_io_protocol_features,
};

fn main() {
    if let Err(error) = run(env::args().skip(1)) {
        eprintln!("runtime manifest error: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: impl IntoIterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let mut arguments = arguments.into_iter();
    let command = arguments.next().ok_or("missing command")?;
    let remaining = arguments.collect::<Vec<_>>();
    match command.as_str() {
        "default-io-features" => {
            reject_arguments(&remaining)?;
            println!("{}", default_io_features().join(","));
        },
        "print-default-features" => {
            reject_arguments(&remaining)?;
            println!(
                "{}",
                default_io_features()
                    .iter()
                    .map(|feature| format!("aether-io/{feature}"))
                    .collect::<Vec<_>>()
                    .join(",")
            );
        },
        "normalize-io-features" => {
            let features = required_option(&remaining, "--io-features")?;
            reject_unknown_options(&remaining, &["--io-features"])?;
            println!(
                "{}",
                normalize_io_protocol_features(split_features(&features))?.join(",")
            );
        },
        "generate" => {
            if remaining
                .first()
                .is_some_and(|argument| !argument.starts_with('-'))
            {
                generate_positional(&remaining)?;
                return Ok(());
            }
            let output = PathBuf::from(required_option(&remaining, "--output")?);
            let target = required_option(&remaining, "--target")?;
            let io_features = option_value(&remaining, "--io-features")?.map_or_else(
                || {
                    default_io_features()
                        .iter()
                        .map(ToString::to_string)
                        .collect()
                },
                |features| split_features(&features),
            );
            reject_unknown_options(&remaining, &["--output", "--target", "--io-features"])?;
            let manifest = KernelRuntimeManifest::from_io_features(
                env!("CARGO_PKG_VERSION"),
                target,
                io_features,
            )?;
            manifest.write_to_file(&output)?;
            println!("{}", output.display());
        },
        "verify" => {
            let path = PathBuf::from(required_option(&remaining, "--path")?);
            let expected_version = match option_value(&remaining, "--aether-version")? {
                Some(version) => version,
                None => env!("CARGO_PKG_VERSION").to_string(),
            };
            reject_unknown_options(&remaining, &["--path", "--aether-version"])?;
            let manifest = load_runtime_manifest_file(&path, &expected_version)?;
            println!("{}", manifest.to_pretty_json()?);
        },
        "validate" => {
            if remaining.len() != 2 {
                return Err("validate requires CONFIG_DIRECTORY AETHER_VERSION".into());
            }
            let manifest = load_runtime_manifest(&remaining[0], &remaining[1])?;
            println!("{}", manifest.to_pretty_json()?);
        },
        _ => return Err(format!("unknown command {command:?}").into()),
    }
    Ok(())
}

fn generate_positional(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    if !(2..=3).contains(&arguments.len()) {
        return Err("generate requires TARGET OUTPUT_DIRECTORY [IO_PROTOCOL_FEATURES]".into());
    }
    let target = &arguments[0];
    let output =
        PathBuf::from(&arguments[1]).join(aether_runtime_catalog::RUNTIME_MANIFEST_FILE_NAME);
    let io_features = arguments.get(2).map_or_else(
        || {
            default_io_features()
                .iter()
                .map(ToString::to_string)
                .collect()
        },
        |features| split_features(features),
    );
    let manifest =
        KernelRuntimeManifest::from_io_features(env!("CARGO_PKG_VERSION"), target, io_features)?;
    manifest.write_to_file(&output)?;
    println!("{}", output.display());
    Ok(())
}

fn split_features(features: &str) -> Vec<String> {
    features
        .split(',')
        .map(str::trim)
        .filter(|feature| !feature.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn required_option(arguments: &[String], name: &str) -> Result<String, Box<dyn Error>> {
    option_value(arguments, name)?.ok_or_else(|| format!("missing required option {name}").into())
}

fn option_value(arguments: &[String], name: &str) -> Result<Option<String>, Box<dyn Error>> {
    let mut found = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == name {
            let value = arguments
                .get(index + 1)
                .ok_or_else(|| format!("missing value for {name}"))?;
            if found.replace(value.clone()).is_some() {
                return Err(format!("option {name} was supplied more than once").into());
            }
            index += 2;
        } else if let Some(value) = arguments[index].strip_prefix(&format!("{name}=")) {
            if found.replace(value.to_string()).is_some() {
                return Err(format!("option {name} was supplied more than once").into());
            }
            index += 1;
        } else {
            index += 1;
        }
    }
    Ok(found)
}

fn reject_unknown_options(arguments: &[String], accepted: &[&str]) -> Result<(), Box<dyn Error>> {
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let name = argument
            .split_once('=')
            .map_or(argument.as_str(), |(name, _)| name);
        if !accepted.contains(&name) {
            return Err(format!("unknown option {argument:?}").into());
        }
        index += usize::from(!argument.contains('=')) + 1;
    }
    Ok(())
}

fn reject_arguments(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(format!("unexpected arguments: {arguments:?}").into())
    }
}
