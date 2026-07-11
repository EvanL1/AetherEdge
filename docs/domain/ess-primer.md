---
title: Energy Storage Primer
description: How ESS concepts - PCS, BMS, SOC, grid interface - map onto Aether products, instances, and points
updated: 2026-07-10
---

# Energy Storage Primer

This page maps energy-storage industry concepts onto Aether's data model. Every device type, point name, and unit cited here comes from the product template library in `libs/aether-model/src/products/` — the same JSON files the system loads at runtime. If you are an AI agent answering questions about an Aether site, these templates are the vocabulary.

## What an energy storage site looks like

Aether models a site as a tree of products. The root is the **Station**; everything else hangs off it:

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

Each node is one JSON template. The parent is recorded in the child's `pName` field (for example, `Battery.json` has `"pName": "ESS"`). Two nodes — `ESS` (energy storage system) and `Generator` — are pure grouping levels: their `P`, `M`, and `A` lists are empty, and they exist only to structure the hierarchy.

The `README.md` in `libs/aether-model/src/products/` omits four of these templates — **PVInverter** (parent `Generator`, an AC-coupled solar inverter) and the three load variants with parent `Station`: **EVChargingLoad**, **HVACLoad**, and **Load_Three_Phase** — but the diagram above is the complete derivation from the templates' `pName` fields.

The Station itself carries site-level facts: static properties such as `Rated Capacity`, `Longitude`/`Latitude`/`Altitude`, and a `Station Type` restricted to `residential` / `commercial` / `industrial` / `datacenter`, plus two measurements, `Status` and `Saving Billing` ($).

## The device roles

**PCS (power conversion system).** The PCS converts power between the DC battery bus and the AC grid. Its template (`PCS.json`) measures both sides: `DC Power` and `DC Voltage` on the battery side; `Total Power` and the three-phase split `Power A`/`Power B`/`Power C`, `Voltage A`/`Voltage B`/`Voltage C`, and `Current A`/`Current B`/`Current C` on the AC side, along with `AC Frequency`, `Grid Status`, and `Direction`. Its actions are `Start`, `Stop`, `Power Set` (kW), `Off On Grid`, and `Clear Error` — `Power Set` is the power setpoint, and `Off On Grid` switches between off-grid and on-grid operation.

**Battery (BMS view).** The Battery template (`Battery.json`) is the battery management system's view of a battery: state of charge (`SOC`, %) and state of health (`SOH`, %), pack totals (`Total Voltage`, `Total Current`, `Charge Power`, `Discharge Power`), and cell-level statistics — `Max Cell Voltage`, `Min Cell Voltage`, `Avg Cell Voltage`, `Cell Voltage Difference`, plus `Cell Voltage Array` and `Cell Temperature Array` for per-cell data. Pack temperatures come as `Max Battery Pack Temperature` / `Min Battery Pack Temperature`. Energy is tracked cumulatively (`Charge Energy`, `Discharge Energy`) and as rolling windows (`Daily` / `Weekly` / `Monthly` / `Quarterly` charge and discharge energy). Static properties record the physical build (`Battery Pack Count`, `Cell Count`) and the operating envelope (`Min SOC`, `Max SOC`, `Charge Efficiency`, `Discharge Efficiency`).

**Diesel generator.** The backup AC source (`Diesel.json`). It reports per-phase power, voltage, and current (`Diesel Power A/B/C`, `Diesel Voltage A/B/C`, `Diesel Current A/B/C`), fuel level as `Diesel Oil` (%), and `Diesel Temperature`. Its properties describe dispatch behavior: `Max Fuel`, `Fuel Consumption Rate` (L/kWh), `Startup Time`, `Min Runtime`, and `Response Time` (all in minutes). Actions mirror the PCS: `Start`, `Stop`, `Power Set`, `Off On Grid`, `Clear Error`.

**PV (solar).** Two flavors exist. **PV DCDC** (`PV_DCDC.json`) is a DC-coupled converter feeding the battery bus, with array measurements (`PV Power (Array)`, `PV Voltage (Array)`, `PV Current (Array)`), `Energy Today`, and efficiency statistics; its properties include `String Count`, `Tilt angle`, and `Azimuth angle`. **PVInverter** (`PVInverter.json`) is the AC-coupled variant with DC-side and three-phase AC-side measurements plus `MPPT Count` and `Phase Count` properties. Both accept `Start`, `Stop`, `Power Set`, and `Clear Error`.

**Loads.** The base `Load.json` measures consumption: `Load Power`, `Energy Used`, `Power Factor`, `Reactive Power` (kVar), and `Apparent Power` (kVa). Its properties describe demand-response flexibility: `Load Flexibility` (`rigid` or `flexible`), `Adjustable Power Range`, `Response Time`, `Max Interrupt Time`, and `Shiftable Time Window`. Variants extend this: **EVChargingLoad** adds `Charging Status` (`charging`/`idle`/`completed`) and charging-specific actions `Target SOC` (0–100%) and `Deadline Time`; **HVACLoad** adds `Indoor Temperature`, `Outdoor Temperature`, `Setpoint Temperature`, and `Operating Mode` (`cooling`/`heating`/`off`), with actions to set `Setpoint Temperature` (16–30 °C) and `Temperature Tolerance`; **Load_Three_Phase** adds per-phase electrical points and demand metering (`Max Demand This Month`, `Current Demand`).

**Env (site environment).** `Env.json` covers site safety sensors: `Water Leakage`, `Lightning Protection`, `Temperature`, `Humidity`, `Fire Protection`, and `Emergency Stop` as measurements, plus `Emergency Stop` and `Fire Protection` as actions.

## Key quantities

Only quantities that actually appear in the product templates:

| Quantity | Unit | Where it appears |
|---|---|---|
| State of charge (`SOC`) | % | `Battery.json` M points; `Min SOC`/`Max SOC` bounds in Battery P; `Target SOC` action in `EVChargingLoad.json` |
| State of health (`SOH`) | % | `Battery.json` |
| Active power, total and per phase | kW | `Total Power` + `Power A/B/C` (PCS, PVInverter); `Load Power` (loads); `Diesel Power A/B/C` |
| Reactive / apparent power | kVar / kVa | `Load.json` and its variants |
| Voltage and current, per phase | V / A | `Voltage A/B/C`, `Current A/B/C` (PCS, PVInverter); `A/B/C Phase Voltage`, `A/B/C Phase Current` (`Load_Three_Phase.json`) |
| DC-side quantities | kW / V / A | `DC Power`, `DC Voltage` (PCS); `DC Current` (PVInverter); `Total Voltage`, `Total Current` (Battery) |
| Frequency | Hz | `AC Frequency` (PCS, PVInverter); `Frequency` (Diesel, loads) |
| Temperature | ℃ / °C | Device temperature (PCS, Diesel); cell and pack temperatures (Battery); ambient (`Env.json`, HVACLoad). HVACLoad's templates write `°C`; the others write `℃` |
| Energy counters | kWh | Cumulative, today, and rolling windows (Battery, Diesel, PV, loads) |

**Naming convention:** a point's canonical name is the `name` field in its product JSON, and points are addressed by `id` within their list (`P`, `M`, or `A`). There is no separate registry — the JSON is the source of truth. Note that unit strings are recorded as written in the templates (`kw`/`kW`, `kwh`/`kWh`, and `℃`/`°C` all occur).

A few point names are not self-explanatory: `Battery System` (%, `Battery.json`), `Diesel Generator` (%, `Diesel.json`), `Solar Panels` and `PV System` (%, `PV_DCDC.json`), and `Sub PVI` (kW, `PV_DCDC.json`). These are the literal template names; the templates do not define their semantics further, so do not guess — check the deployment's channel mapping to see what a given device writes into them.

## How this maps to Aether

| Industry concept | Aether concept |
|---|---|
| Device type (PCS, battery, meter…) | Product (JSON template with P/M/A point definitions) |
| A physical device on site | Instance (created from a product) |
| Live telemetry (power, SOC…) | Measurement points (M), written by io |
| Commands and setpoints | Action points (A), written by automation |
| Nameplate data (rated power…) | Properties (P), static |
| Field protocol (Modbus, IEC 104…) | Channel (one per device connection) |

A product is a type; an instance is a device. Creating an instance from the `Battery` product gives it every P, M, and A point the template defines. Field data flows in through a channel (one per device connection) handled by io, which writes M points; control flows out through automation, which writes A points. The split is enforced by the architecture — see [Data Model](../concepts/data-model.md) for how instances and points are stored and addressed.

## Standard information models

**Field protocols.** io speaks 14 protocols: Modbus TCP/RTU, IEC 60870-5-104, IEC 61850 (MMS), OPC UA, MQTT, HTTP, DL/T 645, CAN/J1939, GPIO, BLE, Zigbee, Matter, Aether-485, and Virtual. Which of these are compiled into a given binary is controlled by Cargo feature flags on io.

**SunSpec.** The `aether-model` crate embeds SunSpec model definitions at compile time and exposes them through its `sunspec` module: `load_model(model_id)` parses an embedded model, `list_model_ids()` enumerates what is available, and `model_exists(model_id)` checks for one. `expand_model` walks a model's group tree and produces Modbus-ready point definitions (`ExpandedPoint` with signal name, register address, data type, unit, scale, and offset), so a SunSpec-compliant inverter or meter can be mapped to channel points without hand-writing a register table. An `ExpandFilter` controls whether static/nameplate points, scale-factor registers, and optional points are included.

## Where to go next

- [Product Models](product-models.md) — the full product template reference and how to define your own
- [Control Strategies](control-strategies.md) — how rules drive A points to implement peak shaving, demand response, and other strategies
- [Safe Operations](safe-operations.md) — operating limits and the guardrails around writes
- [Data Model](../concepts/data-model.md) — how products, instances, and points are stored and addressed at runtime
