"""Example transform script for solar inverter data.

This script demonstrates how to handle complex JSON payloads
that JSONPath alone cannot express (array iteration, conditional
quality flags, calculated fields).

Input payload example:
{
    "timestamp": 1700000000,
    "site_id": "solar-farm-01",
    "inverters": [
        {"id": 1, "dc_power": 5000, "ac_power": 4800, "status": "online", "temp": 45.2},
        {"id": 2, "dc_power": 4500, "ac_power": 4300, "status": "offline", "temp": 80.1}
    ],
    "grid": {"frequency": 50.01, "voltage": 400.5}
}

Output: list of standardized data point dicts.
"""


def transform(payload: dict) -> list[dict]:
    """Transform solar inverter JSON into AetherEMS data points."""
    points = []

    # Per-inverter metrics
    for inv in payload.get("inverters", []):
        inv_id = inv["id"]
        base_id = inv_id * 100  # point_id namespace per inverter

        # DC power (W -> kW)
        points.append({
            "point_id": base_id + 1,
            "point_type": "T",
            "value": inv["dc_power"] * 0.001,
            "quality": "good" if inv.get("status") == "online" else "bad",
        })

        # AC power (W -> kW)
        points.append({
            "point_id": base_id + 2,
            "point_type": "T",
            "value": inv["ac_power"] * 0.001,
            "quality": "good" if inv.get("status") == "online" else "bad",
        })

        # Efficiency (calculated field - not directly in JSON)
        dc = inv.get("dc_power", 0)
        ac = inv.get("ac_power", 0)
        efficiency = (ac / dc * 100) if dc > 0 else 0.0
        points.append({
            "point_id": base_id + 3,
            "point_type": "T",
            "value": round(efficiency, 2),
        })

        # Temperature with conditional quality
        temp = inv.get("temp", 0)
        quality = "good"
        if temp > 75:
            quality = "uncertain"
        if temp > 90:
            quality = "bad"
        points.append({
            "point_id": base_id + 4,
            "point_type": "T",
            "value": temp,
            "quality": quality,
        })

        # Online status (signal point)
        points.append({
            "point_id": base_id + 10,
            "point_type": "S",
            "value": inv.get("status") == "online",
        })

    # Grid-level metrics
    grid = payload.get("grid", {})
    if "frequency" in grid:
        points.append({
            "point_id": 10001,
            "point_type": "T",
            "value": grid["frequency"],
        })
    if "voltage" in grid:
        points.append({
            "point_id": 10002,
            "point_type": "T",
            "value": grid["voltage"],
        })

    return points
