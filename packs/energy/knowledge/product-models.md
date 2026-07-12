---
title: Energy Pack Product Models
description: The 13 Energy Pack product models, their hierarchy, and the meaning of every measurement and action point
updated: 2026-07-12
---

# Energy Pack Product Models

The Energy Pack owns 13 products — JSON templates that define the structure of energy device instances: configuration properties (P), measurement points (M), and action points (A). They live under `packs/energy/models/` and enter automation only through the validated active-Pack configuration. The kernel embeds none of them. Point ids below are the same ids used in `inst:{id}:M` / `inst:{id}:A` keys, rules, and the HTTP API.

This page transcribes the product JSONs verbatim. The point names and units *are* the documentation; where a name is ambiguous, it is reproduced literally rather than interpreted.

## Hierarchy

Each product's `pName` field names its parent. Deriving the tree from the actual JSON files gives:

```
Station                      (root — no pName field)
├── ESS                      (grouping node, empty P/M/A)
│   ├── Battery
│   └── PCS
├── Generator                (grouping node, empty P/M/A)
│   ├── Diesel
│   ├── PV DCDC
│   └── PVInverter
├── Env
├── Load
├── Load_Three_Phase
├── EVChargingLoad
└── HVACLoad
```

Two naming notes matter when consuming the Pack:

- The four leaf models `PVInverter`, `Load_Three_Phase`, `EVChargingLoad`, and `HVACLoad` are included in the 13-model hierarchy above.
- **The product named `PV DCDC` contains a space**, while its file is `PV_DCDC.json`. Lookups and explicit site overrides match on the JSON `name` field, not the filename, so the correct key is `"PV DCDC"`.

`ProductLibrary::children(parent_name)` in `libs/aether-model/src/product_lib.rs` walks the selected hierarchy at runtime.

## Reading a product definition

Every product JSON has the same shape:

```json
{
  "name": "Battery",
  "pName": "ESS",
  "P": [{"id": 1, "name": "Max Power", "unit": "kw", "type": "number"}],
  "M": [{"id": 7, "name": "SOC", "unit": "%", "type": "number"}],
  "A": [{"id": 1, "name": "Start", "unit": "", "type": "string"}]
}
```

- `name` — the product's unique identifier.
- `pName` — parent product name; absent on the root (`Station`). Deserialized as `Option<String>` (`parent_name` in `BuiltinProduct`).
- P / M / A — arrays of point definitions: **P**roperties (static configuration set per instance), **M**easurements (live telemetry), **A**ctions (writable control points).
- Each point has `id`, `name`, `unit`, `type` (mapped to `PointDef { id: u32, name, unit, value_type }` in `product_lib.rs`; `unit` and `type` default to `""` when absent).

Rules to keep in mind when reading the tables below:

- **`id` is the point's identity within its type.** The three arrays have independent id spaces — Battery has an id 1 in P, M, and A, and they are unrelated points. A point is addressed as (instance, point type, id).
- **Ids need not be contiguous.** Load's measurements skip id 9; Battery's jump from 21 to 101.
- **Unit strings are literal.** The JSONs mix casings (`kw` and `kW`, `kwh` and `kWh`) and both `℃` and `°C`; the tables reproduce them exactly.
- **Some points carry extra JSON keys** — `options` (enumerated values), `min`/`max` (numeric bounds), `default`. These are noted under each table. The Rust `PointDef` struct only deserializes `id`/`name`/`unit`/`type`, so the extra keys exist in the raw JSON but are not exposed through the Rust API.

## Station

The root product of the hierarchy, carrying the station's nameplate and location properties, plus two site-wide measurements. It has no action points.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Rated Capacity | | string |
| 2 | Longitude | | string |
| 3 | Latitude | | string |
| 4 | Altitude | m | number |
| 5 | Station Type | | string |
| 6 | Scenario Parameters | | string |

`Station Type` (P5) has `options`: `residential`, `commercial`, `industrial`, `datacenter`.

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Status | | number |
| 2 | Saving Billing | $ | string |

Actions: none (A is an empty array).

## ESS

The energy storage system grouping node under Station, parenting Battery and PCS. Its P, M, and A arrays are all empty — it defines structure only. See the [ESS Primer](ess-primer.md) for the domain background.

## Battery

The battery bank inside an ESS: pack-level electrical totals, cell statistics, state of charge/health, and charge/discharge energy counters over several time windows.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Power | kw | number |
| 2 | Max Voltage | V | number |
| 3 | Max Current | A | number |
| 4 | Max Capacity | kwh | number |
| 5 | Battery Pack Count | | number |
| 6 | Cell Count | | number |
| 7 | Temperature Count | | number |
| 8 | Min Power | kw | number |
| 9 | Min SOC | % | number |
| 10 | Max SOC | % | number |
| 11 | Charge Efficiency | % | number |
| 12 | Discharge Efficiency | % | number |

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Total Voltage | V | number |
| 2 | Total Current | A | number |
| 3 | Max Battery Pack Temperature | ℃ | number |
| 4 | Min Battery Pack Temperature | ℃ | number |
| 5 | Charge Power | kw | number |
| 6 | Discharge Power | kw | number |
| 7 | SOC | % | number |
| 8 | SOH | % | number |
| 9 | Charge Energy | kwh | number |
| 10 | Discharge Energy | kwh | number |
| 11 | Charge Discharge Status | | number |
| 12 | Max Cell Voltage | V | number |
| 13 | Min Cell Voltage | V | number |
| 14 | Avg Cell Voltage | V | number |
| 15 | Cell Voltage Difference | V | number |
| 16 | Avg Cell Temperature | ℃ | number |
| 17 | Cell Voltage Array | V | number |
| 18 | Cell Temperature Array | ℃ | number |
| 19 | Battery System | % | number |
| 20 | Charge Energy Today | kwh | number |
| 21 | Discharge Energy Today | kwh | number |
| 101 | Daily Charge Energy | kWh | number |
| 102 | Daily Discharge Energy | kWh | number |
| 103 | Weekly Charge Energy | kWh | number |
| 104 | Weekly Discharge Energy | kWh | number |
| 105 | Monthly Charge Energy | kWh | number |
| 106 | Monthly Discharge Energy | kWh | number |
| 107 | Quarterly Charge Energy | kWh | number |
| 108 | Quarterly Discharge Energy | kWh | number |

Actions:

| id | name | unit | type |
|----|------|------|------|
| 1 | Start | | string |
| 2 | Stop | | string |
| 3 | Clear Error | | string |

## PCS

The power conversion system (bidirectional inverter) inside an ESS, converting between the DC battery bus and the three-phase AC grid.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Power | kw | number |
| 2 | Max Voltage | V | number |
| 3 | Max Current AC | A | number |
| 4 | Max Current DC | A | number |
| 5 | Rated Frequency | Hz | number |
| 6 | Conversion Efficiency | % | number |

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Total Power | kw | number |
| 2 | DC Power | kw | number |
| 3 | Power A | kw | number |
| 4 | Power B | kw | number |
| 5 | Power C | kw | number |
| 6 | DC Voltage | V | number |
| 7 | Voltage A | V | number |
| 8 | Voltage B | V | number |
| 9 | Voltage C | V | number |
| 10 | Current A | A | number |
| 11 | Current B | A | number |
| 12 | Current C | A | number |
| 13 | Temperature | ℃ | number |
| 14 | Start Stop Status | | number |
| 15 | Grid Status | | number |
| 16 | Direction | | number |
| 17 | AC Frequency | Hz | number |

Actions:

| id | name | unit | type |
|----|------|------|------|
| 1 | Start | | string |
| 2 | Stop | | string |
| 3 | Power Set | kw | number |
| 4 | Off On Grid | | string |
| 5 | Clear Error | | string |

## Generator

The generation grouping node under Station, parenting Diesel, PV DCDC, and PVInverter. Its P, M, and A arrays are all empty — it defines structure only.

## Diesel

A diesel generator set: per-phase electrical measurements, fuel level, and dispatch-relevant properties such as startup and response times.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Power | kw | number |
| 2 | Max Voltage | V | number |
| 3 | Max Current | A | number |
| 4 | Max Fuel | L | number |
| 5 | Rated Frequency | Hz | number |
| 6 | Min Power | kw | number |
| 7 | Fuel Consumption Rate | L/kWh | number |
| 8 | Startup Time | min | number |
| 9 | Min Runtime | min | number |
| 10 | Response Time | min | number |

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Diesel Power | kw | number |
| 2 | Diesel Energy | kwh | number |
| 3 | Diesel Voltage | V | number |
| 4 | Diesel Current A | A | number |
| 5 | Diesel Current B | A | number |
| 6 | Diesel Current C | A | number |
| 7 | Diesel Voltage A | V | number |
| 8 | Diesel Voltage B | V | number |
| 9 | Diesel Voltage C | V | number |
| 10 | Diesel Power A | kw | number |
| 11 | Diesel Power B | kw | number |
| 12 | Diesel Power C | kw | number |
| 13 | Diesel Oil | % | number |
| 14 | Diesel Temperature | ℃ | number |
| 15 | Start Stop Status | | number |
| 16 | Frequency | Hz | number |
| 17 | Diesel Generator | % | number |
| 18 | Diesel Energy Today | kwh | number |

Actions:

| id | name | unit | type |
|----|------|------|------|
| 1 | Start | | string |
| 2 | Stop | | string |
| 3 | Power Set | kw | number |
| 4 | Off On Grid | | string |
| 5 | Clear Error | | string |

## PV DCDC

A DC-coupled photovoltaic converter (note the space in the product name; the file is `PV_DCDC.json`): PV array electrical measurements, efficiency statistics, and array-geometry properties.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Power | kw | number |
| 2 | Max Voltage | V | number |
| 3 | Max Current | A | number |
| 4 | Station | | string |
| 5 | String Count | | number |
| 6 | Cell type | | string |
| 7 | Tracking method | | string |
| 8 | Tilt angle | | number |
| 9 | Azimuth angle | | number |

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | PV Power (Array) | kw | number |
| 2 | PV Voltage (Array) | V | number |
| 3 | PV Current (Array) | A | number |
| 4 | Sub PVI | kw | number |
| 5 | Energy Today | kwh | number |
| 6 | Start Stop Status | | number |
| 7 | PV Power | kw | number |
| 8 | PV Voltage | V | number |
| 9 | PV Current | A | number |
| 10 | Peak Efficiency | % | number |
| 11 | Average Efficiency | % | number |
| 12 | Minimum Efficiency | % | number |
| 13 | Solar Panels | % | number |
| 14 | PV System | % | number |
| 15 | Energy Total | kwh | number |

Actions:

| id | name | unit | type |
|----|------|------|------|
| 1 | Start | | string |
| 2 | Stop | | string |
| 3 | Power Set | kw | number |
| 4 | Clear Error | | string |

## PVInverter

A grid-tied photovoltaic inverter: DC input and three-phase AC output measurements, plus string/MPPT topology properties. Not listed in the products README tree, but its `pName` places it under Generator.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Power | kw | number |
| 2 | Max DC Voltage | V | number |
| 3 | Max DC Current | A | number |
| 4 | Rated AC Voltage | V | number |
| 5 | Rated Frequency | Hz | number |
| 6 | String Count | | number |
| 7 | MPPT Count | | number |
| 8 | Phase Count | | number |

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Total Power | kw | number |
| 2 | DC Power | kw | number |
| 3 | DC Voltage | V | number |
| 4 | DC Current | A | number |
| 5 | Power A | kw | number |
| 6 | Power B | kw | number |
| 7 | Power C | kw | number |
| 8 | Voltage A | V | number |
| 9 | Voltage B | V | number |
| 10 | Voltage C | V | number |
| 11 | Current A | A | number |
| 12 | Current B | A | number |
| 13 | Current C | A | number |
| 14 | AC Frequency | Hz | number |
| 15 | Temperature | ℃ | number |
| 16 | Efficiency | % | number |
| 17 | Energy Today | kwh | number |
| 18 | Start Stop Status | | number |
| 19 | Grid Status | | number |

Actions:

| id | name | unit | type |
|----|------|------|------|
| 1 | Start | | string |
| 2 | Stop | | string |
| 3 | Power Set | kw | number |
| 4 | Clear Error | | string |

## Load

A generic single-feed electrical load with demand-response properties (flexibility, adjustable range, interrupt limits). It has no action points.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Voltage | V | number |
| 2 | Max Current | A | number |
| 3 | Rated Frequency | Hz | number |
| 4 | Max Power | kw | number |
| 5 | Load Flexibility | | string |
| 6 | Adjustable Power Range | kw | number |
| 7 | Response Time | min | number |
| 8 | Min Runtime | min | number |
| 9 | Max Interrupt Time | min | number |
| 10 | Shiftable Time Window | min | number |

`Load Flexibility` (P5) has `options`: `rigid`, `flexible`.

Measurements (note: there is no id 9):

| id | name | unit | type |
|----|------|------|------|
| 1 | Load Power | kw | number |
| 2 | Energy Used | kwh | number |
| 3 | Voltage | V | number |
| 4 | Current | A | number |
| 5 | Frequency | Hz | number |
| 6 | Power Factor | | number |
| 7 | Reactive Power | kVar | number |
| 8 | Apparent Power | kVa | number |
| 10 | Energy Used Today | kwh | number |

Actions: none (A is an empty array).

## Load_Three_Phase

A three-phase metered load with per-phase voltage/current/power, energy counters, and demand tracking. Same demand-response properties as Load; no action points. Not listed in the products README tree, but its `pName` places it under Station.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Voltage | V | number |
| 2 | Max Current | A | number |
| 3 | Rated Frequency | Hz | number |
| 4 | Max Power | kw | number |
| 5 | Load Flexibility | | string |
| 6 | Adjustable Power Range | kw | number |
| 7 | Response Time | min | number |
| 8 | Min Runtime | min | number |
| 9 | Max Interrupt Time | min | number |
| 10 | Shiftable Time Window | min | number |

`Load Flexibility` (P5) has `options`: `rigid`, `flexible`.

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Total Active Power | kW | number |
| 2 | Total Reactive Power | kVar | number |
| 3 | Power Factor | | number |
| 4 | Frequency | Hz | number |
| 5 | A Phase Voltage | V | number |
| 6 | B Phase Voltage | V | number |
| 7 | C Phase Voltage | V | number |
| 8 | A Phase Current | A | number |
| 9 | B Phase Current | A | number |
| 10 | C Phase Current | A | number |
| 11 | A Phase Active Power | kW | number |
| 12 | B Phase Active Power | kW | number |
| 13 | C Phase Active Power | kW | number |
| 14 | Total Energy | kWh | number |
| 15 | Energy Today | kWh | number |
| 16 | Energy This Month | kWh | number |
| 17 | Max Demand This Month | kW | number |
| 18 | Current Demand | kW | number |

`Power Factor` (M3) has no `unit` key in the JSON; it deserializes to an empty string.

Actions: none (A is an empty array).

## EVChargingLoad

An electric-vehicle charging load: the generic load points plus charger-specific properties, a charging status, and charge-scheduling actions. Not listed in the products README tree, but its `pName` places it under Station.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Voltage | V | number |
| 2 | Max Current | A | number |
| 3 | Rated Frequency | Hz | number |
| 4 | Max Power | kw | number |
| 5 | Load Flexibility | | string |
| 6 | Adjustable Power Range | kw | number |
| 7 | Response Time | min | number |
| 8 | Min Runtime | min | number |
| 9 | Max Interrupt Time | min | number |
| 10 | Shiftable Time Window | min | number |
| 11 | Charger Type | | string |
| 12 | Battery Capacity | kWh | number |
| 13 | Charging Efficiency | % | number |

Extra keys: `Load Flexibility` (P5) has `options` `rigid`/`flexible`; `Charger Type` (P11) has `options` `Level1`/`Level2`/`DC_Fast`; `Charging Efficiency` (P13) has `default` 90.

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Load Power | kw | number |
| 2 | Energy Used | kwh | number |
| 3 | Voltage | V | number |
| 4 | Current | A | number |
| 5 | Frequency | Hz | number |
| 6 | Power Factor | | number |
| 7 | Reactive Power | kVar | number |
| 8 | Apparent Power | kVa | number |
| 9 | Charging Status | | string |

`Charging Status` (M9) has `options`: `charging`, `idle`, `completed`.

Actions:

| id | name | unit | type |
|----|------|------|------|
| 1 | Start | | string |
| 2 | Stop | | string |
| 3 | Power Set | kw | number |
| 4 | Target SOC | % | number |
| 5 | Deadline Time | HH:mm | string |

`Target SOC` (A4) has `min` 0 and `max` 100.

## HVACLoad

A heating/ventilation/air-conditioning load: the generic load points plus thermal properties (capacity, thermal mass, heat transfer) and temperature-setpoint control. Not listed in the products README tree, but its `pName` places it under Station.

Properties:

| id | name | unit | type |
|----|------|------|------|
| 1 | Max Voltage | V | number |
| 2 | Max Current | A | number |
| 3 | Rated Frequency | Hz | number |
| 4 | Max Power | kw | number |
| 5 | Load Flexibility | | string |
| 6 | Adjustable Power Range | kw | number |
| 7 | Response Time | min | number |
| 8 | Min Runtime | min | number |
| 9 | Max Interrupt Time | min | number |
| 10 | Shiftable Time Window | min | number |
| 11 | HVAC Type | | string |
| 12 | Rated Cooling Capacity | kW | number |
| 13 | Rated Heating Capacity | kW | number |
| 14 | Thermal Mass | kJ/°C | number |
| 15 | Heat Transfer Coefficient | W/°C | number |
| 16 | Controlled Area | m² | number |

Extra keys: `Load Flexibility` (P5) has `options` `rigid`/`flexible`; `HVAC Type` (P11) has `options` `air_conditioner`/`heat_pump`/`furnace`.

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Load Power | kw | number |
| 2 | Energy Used | kwh | number |
| 3 | Voltage | V | number |
| 4 | Current | A | number |
| 5 | Frequency | Hz | number |
| 6 | Power Factor | | number |
| 7 | Reactive Power | kVar | number |
| 8 | Apparent Power | kVa | number |
| 9 | Indoor Temperature | °C | number |
| 10 | Outdoor Temperature | °C | number |
| 11 | Setpoint Temperature | °C | number |
| 12 | Operating Mode | | string |

`Operating Mode` (M12) has `options`: `cooling`, `heating`, `off`.

Actions:

| id | name | unit | type |
|----|------|------|------|
| 1 | Setpoint Temperature | °C | number |
| 2 | Temperature Tolerance | °C | number |

`Setpoint Temperature` (A1) has `min` 16 / `max` 30; `Temperature Tolerance` (A2) has `min` 0.5 / `max` 5.

## Env

Environmental and safety monitoring for the station: leak, lightning, fire, and emergency-stop signals plus ambient temperature and humidity. It has no properties.

Properties: none (P is an empty array).

Measurements:

| id | name | unit | type |
|----|------|------|------|
| 1 | Water Leakage | | number |
| 2 | Lightning Protection | | number |
| 3 | Temperature | ℃ | number |
| 4 | Humidity | % | number |
| 5 | Fire Protection | | number |
| 6 | Emergency Stop | | number |

Actions:

| id | name | unit | type |
|----|------|------|------|
| 1 | Emergency Stop | | string |
| 2 | Fire Protection | | string |

## Custom products and overrides

Products remain extensible without recompiling. Automation loads model directories from validated active Packs in configured order, then loads the explicit site-owned `products_path` last. A later directory may deliberately replace an earlier product with the same `name`; duplicate names inside one directory fail closed. The kernel default is empty.

`aether sync` and runtime loading share the same candidate rules: JSON entries must be UTF-8 named, regular non-symlink files confined to the selected directory and below the size limit, and must deserialize to a non-empty product identity. A site `Battery.json` can therefore override the active Energy Pack's Battery deliberately, while no Pack activation means no Energy model is available.

## SunSpec expansion

For devices that speak the standard SunSpec register map (PV inverters, meters, storage), `libs/aether-model/src/sunspec/expand.rs` bridges the standard models to concrete point sets: `expand_model(model, config)` walks a `SunSpecModel`'s group tree and emits a `Vec<ExpandedPoint>` — Modbus telemetry point definitions (signal name, register address, data type, unit, scale/offset, protocol mappings) ready for SQLite insertion as channel points. `ExpandConfig` supplies the discovered model's id, start register, slave id, and function code, while `ExpandFilter` controls whether static/nameplate points, scale-factor registers (`sunssf`), and optional points are included. This turns a standard inverter or meter model into a ready-made register table feeding instances of products like PVInverter, instead of hand-authoring the Modbus mapping. See [Connect Devices](../guides/connect-devices.md) for the workflow.
