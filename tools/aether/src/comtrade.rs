//! IEC/IEEE COMTRADE file inspection and conversion tooling.
//!
//! COMTRADE is a paired file format rather than a live device protocol, so it
//! belongs in the offline CLI instead of the production IO channel registry.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use serde::Serialize;

#[derive(Debug, Subcommand)]
pub(crate) enum ComtradeCommands {
    /// Validate and summarize a COMTRADE CFG file and optional DAT file
    Inspect {
        /// COMTRADE configuration file
        #[arg(long)]
        cfg: PathBuf,
        /// COMTRADE data file; defaults to the CFG path with a .dat suffix
        #[arg(long)]
        dat: Option<PathBuf>,
    },
    /// Convert COMTRADE samples to a normalized CSV file
    ExportCsv {
        /// COMTRADE configuration file
        #[arg(long)]
        cfg: PathBuf,
        /// COMTRADE data file; defaults to the CFG path with a .dat suffix
        #[arg(long)]
        dat: Option<PathBuf>,
        /// Destination CSV path
        #[arg(long)]
        output: PathBuf,
    },
}

pub(crate) fn run(command: ComtradeCommands, json: bool) -> Result<()> {
    match command {
        ComtradeCommands::Inspect { cfg, dat } => {
            let config = ComtradeConfig::read(&cfg)?;
            let dat = dat.unwrap_or_else(|| cfg.with_extension("dat"));
            let sample_count = if dat.exists() {
                Some(config.read_samples(&dat)?.len())
            } else {
                None
            };
            let summary = ComtradeSummary::new(&config, &cfg, &dat, sample_count);
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!("Station: {}", summary.station);
                println!("Recorder: {}", summary.recorder);
                println!("Revision: {}", summary.revision);
                println!("Format: {}", summary.data_format);
                println!("Analog channels: {}", summary.analog_channels);
                println!("Digital channels: {}", summary.digital_channels);
                println!(
                    "Sample rates: {}",
                    summary
                        .sample_rates
                        .iter()
                        .map(|rate| format!(
                            "{} Hz through sample {}",
                            rate.samples_per_second, rate.final_sample
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                match summary.decoded_samples {
                    Some(count) => println!("Decoded samples: {count}"),
                    None => println!("Decoded samples: DAT file not found"),
                }
            }
        },
        ComtradeCommands::ExportCsv { cfg, dat, output } => {
            let config = ComtradeConfig::read(&cfg)?;
            let dat = dat.unwrap_or_else(|| cfg.with_extension("dat"));
            let samples = config.read_samples(&dat)?;
            config.write_csv(&output, &samples)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "cfg": cfg,
                        "dat": dat,
                        "output": output,
                        "sample_count": samples.len()
                    })
                );
            } else {
                println!(
                    "Exported {} COMTRADE samples to {}",
                    samples.len(),
                    output.display()
                );
            }
        },
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum DataFormat {
    Ascii,
    Binary,
    Binary32,
    Float32,
}

impl DataFormat {
    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "ASCII" => Ok(Self::Ascii),
            "BINARY" => Ok(Self::Binary),
            "BINARY32" => Ok(Self::Binary32),
            "FLOAT32" => Ok(Self::Float32),
            value => bail!("unsupported COMTRADE DAT format '{value}'"),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Ascii => "ASCII",
            Self::Binary => "BINARY",
            Self::Binary32 => "BINARY32",
            Self::Float32 => "FLOAT32",
        }
    }
}

#[derive(Debug, Clone)]
struct AnalogChannel {
    index: usize,
    name: String,
    phase: String,
    unit: String,
    a: f64,
    b: f64,
}

#[derive(Debug, Clone)]
struct DigitalChannel {
    index: usize,
    name: String,
    phase: String,
}

#[derive(Debug, Clone, Serialize)]
struct SampleRate {
    samples_per_second: f64,
    final_sample: u64,
}

#[derive(Debug, Clone)]
struct ComtradeConfig {
    station: String,
    recorder: String,
    revision: String,
    analog: Vec<AnalogChannel>,
    digital: Vec<DigitalChannel>,
    frequency_hz: f64,
    sample_rates: Vec<SampleRate>,
    start_timestamp: String,
    trigger_timestamp: String,
    data_format: DataFormat,
    time_multiplier: f64,
}

#[derive(Debug, Clone)]
struct Sample {
    number: u32,
    timestamp_raw: u32,
    timestamp_seconds: f64,
    analog_raw: Vec<f64>,
    analog_engineering: Vec<f64>,
    digital: Vec<bool>,
}

#[derive(Debug, Serialize)]
struct ComtradeSummary<'a> {
    cfg: &'a Path,
    dat: &'a Path,
    station: &'a str,
    recorder: &'a str,
    revision: &'a str,
    data_format: &'a str,
    analog_channels: usize,
    digital_channels: usize,
    frequency_hz: f64,
    sample_rates: &'a [SampleRate],
    start_timestamp: &'a str,
    trigger_timestamp: &'a str,
    decoded_samples: Option<usize>,
}

impl<'a> ComtradeSummary<'a> {
    fn new(
        config: &'a ComtradeConfig,
        cfg: &'a Path,
        dat: &'a Path,
        decoded_samples: Option<usize>,
    ) -> Self {
        Self {
            cfg,
            dat,
            station: &config.station,
            recorder: &config.recorder,
            revision: &config.revision,
            data_format: config.data_format.label(),
            analog_channels: config.analog.len(),
            digital_channels: config.digital.len(),
            frequency_hz: config.frequency_hz,
            sample_rates: &config.sample_rates,
            start_timestamp: &config.start_timestamp,
            trigger_timestamp: &config.trigger_timestamp,
            decoded_samples,
        }
    }
}

impl ComtradeConfig {
    fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read COMTRADE CFG {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid COMTRADE CFG {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        let station_line = parse_csv_line(next_line(&mut lines, "station header")?)?;
        if station_line.len() < 2 {
            bail!("COMTRADE station header requires station and recorder names");
        }
        let station = station_line[0].trim().to_owned();
        let recorder = station_line[1].trim().to_owned();
        let revision = station_line
            .get(2)
            .map_or("1991", |value| value.trim())
            .to_owned();

        let count_line = parse_csv_line(next_line(&mut lines, "channel counts")?)?;
        if count_line.len() < 3 {
            bail!("COMTRADE channel count line requires total, analog, and digital counts");
        }
        let total = parse_suffixed_count(&count_line[0], None)?;
        let analog_count = parse_suffixed_count(&count_line[1], Some('A'))?;
        let digital_count = parse_suffixed_count(&count_line[2], Some('D'))?;
        if total != analog_count + digital_count {
            bail!("COMTRADE total channel count does not match analog plus digital counts");
        }

        let mut analog = Vec::with_capacity(analog_count);
        for _ in 0..analog_count {
            let fields = parse_csv_line(next_line(&mut lines, "analog channel")?)?;
            if fields.len() < 7 {
                bail!("COMTRADE analog channel requires at least seven fields");
            }
            analog.push(AnalogChannel {
                index: parse_value(&fields[0], "analog channel index")?,
                name: fields[1].trim().to_owned(),
                phase: fields.get(2).map_or("", String::as_str).trim().to_owned(),
                unit: fields.get(4).map_or("", String::as_str).trim().to_owned(),
                a: parse_value(&fields[5], "analog multiplier a")?,
                b: parse_value(&fields[6], "analog offset b")?,
            });
        }
        validate_channel_indexes(analog.iter().map(|channel| channel.index), "analog")?;

        let mut digital = Vec::with_capacity(digital_count);
        for _ in 0..digital_count {
            let fields = parse_csv_line(next_line(&mut lines, "digital channel")?)?;
            if fields.len() < 2 {
                bail!("COMTRADE digital channel requires at least two fields");
            }
            digital.push(DigitalChannel {
                index: parse_value(&fields[0], "digital channel index")?,
                name: fields[1].trim().to_owned(),
                phase: fields.get(2).map_or("", String::as_str).trim().to_owned(),
            });
        }
        validate_channel_indexes(digital.iter().map(|channel| channel.index), "digital")?;

        let frequency_hz = parse_value(next_line(&mut lines, "line frequency")?, "frequency")?;
        let rate_count: usize =
            parse_value(next_line(&mut lines, "sample-rate count")?, "rate count")?;
        if rate_count == 0 {
            bail!("COMTRADE requires at least one sample rate");
        }
        let mut sample_rates = Vec::with_capacity(rate_count);
        for _ in 0..rate_count {
            let fields = parse_csv_line(next_line(&mut lines, "sample rate")?)?;
            if fields.len() < 2 {
                bail!("COMTRADE sample rate requires frequency and final sample");
            }
            sample_rates.push(SampleRate {
                samples_per_second: parse_value(&fields[0], "sample rate")?,
                final_sample: parse_value(&fields[1], "final sample")?,
            });
        }
        if !sample_rates
            .windows(2)
            .all(|window| window[0].final_sample < window[1].final_sample)
        {
            bail!("COMTRADE sample-rate final sample numbers must increase");
        }
        let start_timestamp = next_line(&mut lines, "start timestamp")?.trim().to_owned();
        let trigger_timestamp = next_line(&mut lines, "trigger timestamp")?
            .trim()
            .to_owned();
        let data_format = DataFormat::parse(next_line(&mut lines, "data format")?)?;
        let time_multiplier: f64 = lines
            .next()
            .map(|value| parse_value(value, "time multiplier"))
            .transpose()?
            .unwrap_or(1.0);
        if !time_multiplier.is_finite() || time_multiplier <= 0.0 {
            bail!("COMTRADE time multiplier must be finite and positive");
        }

        Ok(Self {
            station,
            recorder,
            revision,
            analog,
            digital,
            frequency_hz,
            sample_rates,
            start_timestamp,
            trigger_timestamp,
            data_format,
            time_multiplier,
        })
    }

    fn read_samples(&self, path: &Path) -> Result<Vec<Sample>> {
        let file = File::open(path)
            .with_context(|| format!("failed to open COMTRADE DAT {}", path.display()))?;
        match self.data_format {
            DataFormat::Ascii => self.read_ascii(file),
            DataFormat::Binary | DataFormat::Binary32 | DataFormat::Float32 => {
                self.read_binary(file)
            },
        }
        .with_context(|| format!("invalid COMTRADE DAT {}", path.display()))
    }

    fn read_ascii(&self, file: File) -> Result<Vec<Sample>> {
        let mut samples = Vec::new();
        for (line_index, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let fields = parse_csv_line(&line)?;
            let required = 2 + self.analog.len() + self.digital.len();
            if fields.len() < required {
                bail!(
                    "ASCII sample line {} has {} fields, expected at least {required}",
                    line_index + 1,
                    fields.len()
                );
            }
            let number = parse_value(&fields[0], "sample number")?;
            let timestamp_raw = parse_value(&fields[1], "sample timestamp")?;
            let analog_raw = fields[2..2 + self.analog.len()]
                .iter()
                .map(|value| parse_value(value, "analog sample"))
                .collect::<Result<Vec<f64>>>()?;
            let digital = fields[2 + self.analog.len()..required]
                .iter()
                .map(|value| match value.trim() {
                    "0" => Ok(false),
                    "1" => Ok(true),
                    value => Err(anyhow!("invalid COMTRADE digital sample '{value}'")),
                })
                .collect::<Result<Vec<bool>>>()?;
            samples.push(self.sample(number, timestamp_raw, analog_raw, digital)?);
        }
        self.validate_samples(&samples)?;
        Ok(samples)
    }

    fn read_binary(&self, mut file: File) -> Result<Vec<Sample>> {
        let analog_width = match self.data_format {
            DataFormat::Binary => 2,
            DataFormat::Binary32 | DataFormat::Float32 => 4,
            DataFormat::Ascii => unreachable!(),
        };
        let digital_words = self.digital.len().div_ceil(16);
        let record_size = 8 + self.analog.len() * analog_width + digital_words * 2;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes.len() % record_size != 0 {
            bail!(
                "binary DAT length {} is not a multiple of record size {record_size}",
                bytes.len()
            );
        }
        let mut samples = Vec::with_capacity(bytes.len() / record_size);
        for record in bytes.chunks_exact(record_size) {
            let number = le_u32(&record[0..4]);
            let timestamp_raw = le_u32(&record[4..8]);
            let mut cursor = 8;
            let mut analog_raw = Vec::with_capacity(self.analog.len());
            for _ in &self.analog {
                let value = match self.data_format {
                    DataFormat::Binary => {
                        let value = i16::from_le_bytes([record[cursor], record[cursor + 1]]);
                        cursor += 2;
                        f64::from(value)
                    },
                    DataFormat::Binary32 => {
                        let value = i32::from_le_bytes([
                            record[cursor],
                            record[cursor + 1],
                            record[cursor + 2],
                            record[cursor + 3],
                        ]);
                        cursor += 4;
                        f64::from(value)
                    },
                    DataFormat::Float32 => {
                        let value = f32::from_le_bytes([
                            record[cursor],
                            record[cursor + 1],
                            record[cursor + 2],
                            record[cursor + 3],
                        ]);
                        cursor += 4;
                        f64::from(value)
                    },
                    DataFormat::Ascii => unreachable!(),
                };
                analog_raw.push(value);
            }
            let mut digital = Vec::with_capacity(self.digital.len());
            for word_index in 0..digital_words {
                let word = u16::from_le_bytes([record[cursor], record[cursor + 1]]);
                cursor += 2;
                for bit in 0..16 {
                    if word_index * 16 + bit < self.digital.len() {
                        digital.push(word & (1 << bit) != 0);
                    }
                }
            }
            samples.push(self.sample(number, timestamp_raw, analog_raw, digital)?);
        }
        self.validate_samples(&samples)?;
        Ok(samples)
    }

    fn sample(
        &self,
        number: u32,
        timestamp_raw: u32,
        analog_raw: Vec<f64>,
        digital: Vec<bool>,
    ) -> Result<Sample> {
        let analog_engineering = analog_raw
            .iter()
            .zip(&self.analog)
            .map(|(raw, channel)| raw.mul_add(channel.a, channel.b))
            .collect::<Vec<_>>();
        if analog_engineering.iter().any(|value| !value.is_finite()) {
            bail!("COMTRADE analog scaling produced a non-finite value");
        }
        Ok(Sample {
            number,
            timestamp_raw,
            timestamp_seconds: f64::from(timestamp_raw) * self.time_multiplier / 1_000_000.0,
            analog_raw,
            analog_engineering,
            digital,
        })
    }

    fn validate_samples(&self, samples: &[Sample]) -> Result<()> {
        if !samples
            .windows(2)
            .all(|window| window[0].number < window[1].number)
        {
            bail!("COMTRADE sample numbers must increase strictly");
        }
        if let Some(expected) = self.sample_rates.last().map(|rate| rate.final_sample)
            && samples
                .last()
                .is_some_and(|sample| u64::from(sample.number) > expected)
        {
            bail!("COMTRADE DAT contains a sample number beyond the CFG final sample");
        }
        Ok(())
    }

    fn write_csv(&self, path: &Path, samples: &[Sample]) -> Result<()> {
        let mut writer = csv::Writer::from_path(path)
            .with_context(|| format!("failed to create CSV {}", path.display()))?;
        let mut header = vec![
            "sample_number".to_owned(),
            "timestamp_raw".to_owned(),
            "timestamp_seconds".to_owned(),
        ];
        for channel in &self.analog {
            let label = channel_label(&channel.name, &channel.phase);
            header.push(format!("{label}_raw"));
            header.push(if channel.unit.is_empty() {
                label
            } else {
                format!("{label}_{}", channel.unit)
            });
        }
        header.extend(
            self.digital
                .iter()
                .map(|channel| channel_label(&channel.name, &channel.phase)),
        );
        writer.write_record(header)?;
        for sample in samples {
            let mut record = vec![
                sample.number.to_string(),
                sample.timestamp_raw.to_string(),
                sample.timestamp_seconds.to_string(),
            ];
            for (raw, engineering) in sample.analog_raw.iter().zip(&sample.analog_engineering) {
                record.push(raw.to_string());
                record.push(engineering.to_string());
            }
            record.extend(sample.digital.iter().map(bool::to_string));
            writer.write_record(record)?;
        }
        writer.flush()?;
        Ok(())
    }
}

fn next_line<'a>(lines: &mut impl Iterator<Item = &'a str>, name: &str) -> Result<&'a str> {
    lines
        .next()
        .ok_or_else(|| anyhow!("missing COMTRADE {name}"))
}

fn parse_csv_line(line: &str) -> Result<Vec<String>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(line.as_bytes());
    let record = reader
        .records()
        .next()
        .ok_or_else(|| anyhow!("empty COMTRADE CSV line"))??;
    Ok(record.iter().map(ToOwned::to_owned).collect())
}

fn parse_suffixed_count(value: &str, suffix: Option<char>) -> Result<usize> {
    let trimmed = value.trim();
    let digits = if let Some(suffix) = suffix {
        trimmed
            .strip_suffix(suffix)
            .or_else(|| trimmed.strip_suffix(suffix.to_ascii_lowercase()))
            .ok_or_else(|| anyhow!("COMTRADE count '{trimmed}' is missing suffix {suffix}"))?
    } else {
        trimmed
    };
    parse_value(digits, "channel count")
}

fn parse_value<T>(value: &str, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .trim()
        .parse::<T>()
        .map_err(|error| anyhow!("invalid COMTRADE {name} '{}': {error}", value.trim()))
}

fn validate_channel_indexes(indexes: impl Iterator<Item = usize>, kind: &str) -> Result<()> {
    for (expected, actual) in indexes.enumerate() {
        if actual != expected + 1 {
            bail!("COMTRADE {kind} channel indexes must be contiguous and one-based");
        }
    }
    Ok(())
}

fn channel_label(name: &str, phase: &str) -> String {
    if phase.is_empty() {
        name.to_owned()
    } else {
        format!("{name}_{phase}")
    }
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = "STATION,RECORDER,2013\n\
2,1A,1D\n\
1,VA,A,,V,0.1,1,0,-32768,32767,1,1,P\n\
1,TRIP,,,0\n\
50\n\
1\n\
1000,2\n\
01/01/2026,00:00:00.000000\n\
01/01/2026,00:00:00.001000\n\
BINARY\n\
1\n";

    #[test]
    fn cfg_parser_preserves_channels_scaling_and_format() {
        let config = ComtradeConfig::parse(CFG).expect("config");
        assert_eq!(config.station, "STATION");
        assert_eq!(config.analog.len(), 1);
        assert_eq!(config.digital.len(), 1);
        assert_eq!(config.data_format, DataFormat::Binary);
        assert_eq!(config.analog[0].a, 0.1);
        assert_eq!(config.analog[0].b, 1.0);
    }

    #[test]
    fn binary_record_decodes_analog_and_digital_values() {
        let config = ComtradeConfig::parse(CFG).expect("config");
        let mut record = Vec::new();
        record.extend_from_slice(&1_u32.to_le_bytes());
        record.extend_from_slice(&1_000_u32.to_le_bytes());
        record.extend_from_slice(&100_i16.to_le_bytes());
        record.extend_from_slice(&1_u16.to_le_bytes());
        let mut cursor = std::io::Cursor::new(record);
        let mut bytes = Vec::new();
        cursor.read_to_end(&mut bytes).expect("read");
        let record_size = 12;
        assert_eq!(bytes.len(), record_size);

        let number = le_u32(&bytes[0..4]);
        let timestamp = le_u32(&bytes[4..8]);
        let raw = f64::from(i16::from_le_bytes([bytes[8], bytes[9]]));
        let digital = u16::from_le_bytes([bytes[10], bytes[11]]) & 1 != 0;
        let sample = config
            .sample(number, timestamp, vec![raw], vec![digital])
            .expect("sample");
        assert_eq!(sample.timestamp_seconds, 0.001);
        assert_eq!(sample.analog_engineering, [11.0]);
        assert_eq!(sample.digital, [true]);
    }

    #[test]
    fn ascii_sample_rejects_invalid_digital_values() {
        let mut config = ComtradeConfig::parse(&CFG.replace("BINARY", "ASCII")).expect("config");
        config.data_format = DataFormat::Ascii;
        let path = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(path.path(), "1,1000,100,2\n").expect("write");
        assert!(config.read_samples(path.path()).is_err());
    }
}
