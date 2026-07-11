#!/usr/bin/env python3
"""
E2E 测试配置生成器

生成 4 通道 × ~1000 点的完整测试配置：
- 光 (PV_DCDC): 通道 1001, 端口 5020
- 储 (Battery): 通道 1002, 端口 5021
- 柴 (Diesel):  通道 1003, 端口 5022
- 荷 (Load):    通道 1004, 端口 5023

点位分布 (每通道):
- T (Telemetry): 800 点 - 覆盖所有数据类型和字节序
- S (Signal):    100 点 - 线圈和离散输入
- C (Control):    50 点 - 写线圈 (FC05)
- A (Adjustment): 50 点 - 写寄存器 (FC06)
"""

import os
import csv
import yaml
from pathlib import Path
from dataclasses import dataclass
from typing import Optional

# ==== 配置常量 ====

CHANNELS = {
    1001: {"name": "PV", "full_name": "E2E_PV", "port": 5020, "has_control": True},
    1002: {"name": "Battery", "full_name": "E2E_Battery", "port": 5021, "has_control": True},
    1003: {"name": "Diesel", "full_name": "E2E_Diesel", "port": 5022, "has_control": True},
    1004: {"name": "Load", "full_name": "E2E_Load", "port": 5023, "has_control": False},
}

# 遥测点位规格：(data_type, count, start_reg, byte_order, registers_per_point)
TELEMETRY_SPEC = [
    ("uint16", 300, 0, "AB", 1),       # reg 0-299
    ("int16", 200, 300, "AB", 1),      # reg 300-499
    ("uint32", 100, 500, "ABCD", 2),   # reg 500-699 (每点2寄存器)
    ("float32", 100, 700, "ABCD", 2),  # reg 700-899 (Big-Endian)
    ("float32", 30, 900, "DCBA", 2),   # reg 900-959 (Little-Endian)
    ("float32", 30, 960, "BADC", 2),   # reg 960-1019 (Word-Swap)
    ("float32", 30, 1020, "CDAB", 2),  # reg 1020-1079 (Word-Swap LE)
    ("bool", 10, 1080, None, 1),       # reg 1080-1089 (bit_position 0-9)
]

# 信号点位规格：(function_code, count, start_addr)
SIGNAL_SPEC = [
    (1, 50, 0),    # FC01 线圈 coil 0-49
    (2, 50, 100),  # FC02 离散输入 DI 100-149
]

# 控制点位 (FC05 写线圈)
CONTROL_SPEC = (5, 50, 200)  # coil 200-249

# 调节点位 (FC06 写寄存器)
ADJUSTMENT_SPEC = (6, 50, 2000)  # reg 2000-2049


@dataclass
class PointDef:
    """点位定义"""
    point_id: int
    signal_name: str
    scale: float
    offset: float
    unit: str
    reverse: int
    data_type: str


@dataclass
class MappingDef:
    """映射定义"""
    point_id: int
    slave_id: int
    function_code: int
    register_address: int
    data_type: str
    byte_order: str
    bit_position: Optional[int]


def get_unit_for_type(data_type: str, idx: int) -> str:
    """根据数据类型返回单位"""
    if data_type in ("uint16", "int16"):
        return ["kW", "kVar", "V", "A", "Hz", "°C", "%", "kWh"][idx % 8]
    elif data_type in ("uint32", "float32"):
        return ["kWh", "kVarh", "MW", "MVar", "V", "A"][idx % 6]
    return ""


def get_scale_for_type(data_type: str, idx: int) -> float:
    """根据数据类型返回缩放因子"""
    if data_type == "uint16":
        return [1, 0.1, 0.01, 0.001][idx % 4]
    elif data_type == "int16":
        return [1, 0.1, 0.01][idx % 3]
    elif data_type in ("uint32", "float32"):
        return 1.0
    return 1.0


def generate_telemetry(ch_id: int, ch_name: str) -> tuple[list[PointDef], list[MappingDef]]:
    """生成遥测点位"""
    points = []
    mappings = []
    point_id = 1

    for data_type, count, start_reg, byte_order, regs_per_point in TELEMETRY_SPEC:
        for i in range(count):
            reg_addr = start_reg + i * regs_per_point

            # 点位定义
            name = f"{ch_name}_T_{data_type}_{point_id}"
            scale = get_scale_for_type(data_type, i)
            unit = get_unit_for_type(data_type, i)

            points.append(PointDef(
                point_id=point_id,
                signal_name=name,
                scale=scale,
                offset=0,
                unit=unit,
                reverse=0,
                data_type=data_type
            ))

            # 映射定义
            bit_pos = i % 16 if data_type == "bool" else None
            mappings.append(MappingDef(
                point_id=point_id,
                slave_id=1,
                function_code=3,  # FC03 Read Holding Registers
                register_address=reg_addr,
                data_type=data_type,
                byte_order=byte_order or "AB",
                bit_position=bit_pos
            ))

            point_id += 1

    return points, mappings


def generate_signal(ch_id: int, ch_name: str) -> tuple[list[PointDef], list[MappingDef]]:
    """生成信号点位 (S)"""
    points = []
    mappings = []
    point_id = 1

    for fc, count, start_addr in SIGNAL_SPEC:
        for i in range(count):
            name = f"{ch_name}_S_FC{fc:02d}_{point_id}"

            points.append(PointDef(
                point_id=point_id,
                signal_name=name,
                scale=1,
                offset=0,
                unit="",
                reverse=0,
                data_type="bool"
            ))

            mappings.append(MappingDef(
                point_id=point_id,
                slave_id=1,
                function_code=fc,
                register_address=start_addr + i,
                data_type="bool",
                byte_order="AB",
                bit_position=None
            ))

            point_id += 1

    return points, mappings


def generate_control(ch_id: int, ch_name: str) -> tuple[list[PointDef], list[MappingDef]]:
    """生成控制点位 (C)"""
    fc, count, start_addr = CONTROL_SPEC
    points = []
    mappings = []

    for i in range(count):
        point_id = i + 1
        name = f"{ch_name}_C_{point_id}"

        points.append(PointDef(
            point_id=point_id,
            signal_name=name,
            scale=1,
            offset=0,
            unit="",
            reverse=0,
            data_type="bool"
        ))

        mappings.append(MappingDef(
            point_id=point_id,
            slave_id=1,
            function_code=fc,
            register_address=start_addr + i,
            data_type="bool",
            byte_order="AB",
            bit_position=None
        ))

    return points, mappings


def generate_adjustment(ch_id: int, ch_name: str) -> tuple[list[PointDef], list[MappingDef]]:
    """生成调节点位 (A)"""
    fc, count, start_addr = ADJUSTMENT_SPEC
    points = []
    mappings = []

    for i in range(count):
        point_id = i + 1
        name = f"{ch_name}_A_{point_id}"

        points.append(PointDef(
            point_id=point_id,
            signal_name=name,
            scale=1,
            offset=0,
            unit="",
            reverse=0,
            data_type="uint16"
        ))

        mappings.append(MappingDef(
            point_id=point_id,
            slave_id=1,
            function_code=fc,
            register_address=start_addr + i,
            data_type="uint16",
            byte_order="AB",
            bit_position=None
        ))

    return points, mappings


def write_point_csv(path: Path, points: list[PointDef]):
    """写入点位 CSV"""
    with open(path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["point_id", "signal_name", "scale", "offset", "unit", "reverse", "data_type"])
        for p in points:
            writer.writerow([p.point_id, p.signal_name, p.scale, p.offset, p.unit, p.reverse, p.data_type])


def write_mapping_csv(path: Path, mappings: list[MappingDef]):
    """写入映射 CSV"""
    with open(path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["point_id", "slave_id", "function_code", "register_address", "data_type", "byte_order", "bit_position"])
        for m in mappings:
            bit_pos = m.bit_position if m.bit_position is not None else ""
            writer.writerow([m.point_id, m.slave_id, m.function_code, m.register_address, m.data_type, m.byte_order, bit_pos])


def generate_io_yaml(base_path: Path):
    """生成 io.yaml"""
    config = {
        "channels": []
    }

    for ch_id, ch_cfg in CHANNELS.items():
        channel = {
            "id": ch_id,
            "name": ch_cfg["full_name"],
            "description": f"E2E Test - {ch_cfg['name']} Simulator",
            "protocol": "modbus_tcp",
            "enabled": True,
            "parameters": {
                "host": "127.0.0.1",
                "port": ch_cfg["port"],
                "connect_timeout_ms": 5000,
                "read_timeout_ms": 3000,
            },
            "logging": {
                "enabled": True,
                "level": "debug"
            }
        }
        config["channels"].append(channel)

    yaml_path = base_path / "io.yaml"
    with open(yaml_path, "w") as f:
        f.write("# E2E Test Configuration for io\n")
        f.write("# 4 channels: PV(5020), Battery(5021), Diesel(5022), Load(5023)\n\n")
        yaml.dump(config, f, default_flow_style=False, allow_unicode=True, sort_keys=False)

    print(f"  Generated: {yaml_path}")


def generate_channel_config(base_path: Path, ch_id: int, ch_cfg: dict):
    """生成单个通道的配置"""
    ch_name = ch_cfg["name"]
    ch_path = base_path / str(ch_id)
    mapping_path = ch_path / "mapping"

    # 创建目录
    ch_path.mkdir(parents=True, exist_ok=True)
    mapping_path.mkdir(parents=True, exist_ok=True)

    # 生成遥测 (T)
    t_points, t_mappings = generate_telemetry(ch_id, ch_name)
    write_point_csv(ch_path / "telemetry.csv", t_points)
    write_mapping_csv(mapping_path / "telemetry_mapping.csv", t_mappings)

    # 生成信号 (S)
    s_points, s_mappings = generate_signal(ch_id, ch_name)
    write_point_csv(ch_path / "signal.csv", s_points)
    write_mapping_csv(mapping_path / "signal_mapping.csv", s_mappings)

    # 生成控制 (C) - 仅有控制的设备
    if ch_cfg["has_control"]:
        c_points, c_mappings = generate_control(ch_id, ch_name)
        write_point_csv(ch_path / "control.csv", c_points)
        write_mapping_csv(mapping_path / "control_mapping.csv", c_mappings)
    else:
        # 空文件 (仅表头)
        write_point_csv(ch_path / "control.csv", [])
        write_mapping_csv(mapping_path / "control_mapping.csv", [])

    # 生成调节 (A) - 仅有控制的设备
    if ch_cfg["has_control"]:
        a_points, a_mappings = generate_adjustment(ch_id, ch_name)
        write_point_csv(ch_path / "adjustment.csv", a_points)
        write_mapping_csv(mapping_path / "adjustment_mapping.csv", a_mappings)
    else:
        write_point_csv(ch_path / "adjustment.csv", [])
        write_mapping_csv(mapping_path / "adjustment_mapping.csv", [])

    # 统计
    t_count = len(t_points)
    s_count = len(s_points)
    c_count = len([p for p in (c_points if ch_cfg["has_control"] else [])]) if ch_cfg["has_control"] else 0
    a_count = len([p for p in (a_points if ch_cfg["has_control"] else [])]) if ch_cfg["has_control"] else 0
    total = t_count + s_count + c_count + a_count

    print(f"  Channel {ch_id} ({ch_name}): T={t_count} S={s_count} C={c_count} A={a_count} -> {total} points")
    return t_count, s_count, c_count, a_count


def generate_simulator_scenario(base_path: Path):
    """为每个设备生成独立的 Simulator 场景文件"""
    scenarios_path = base_path / "tools" / "simulator" / "scenarios"
    scenarios_path.mkdir(parents=True, exist_ok=True)

    for ch_id, ch_cfg in CHANNELS.items():
        scenario_file = scenarios_path / f"e2e_{ch_cfg['name'].lower()}.yaml"

        # 构建场景 - 与 simulator 兼容的格式
        scenario = {
            "name": f"E2E {ch_cfg['name']} Scenario",
            "devices": [{
                "type": ch_cfg["name"].lower(),
                "unit_id": 1,
                "registers": []
            }],
            "faults": {"enabled": False}
        }

        registers = scenario["devices"][0]["registers"]

        # 保持寄存器 (FC03) - 遥测数据
        # uint16: 0-299
        for i in range(300):
            registers.append({
                "address": i,
                "name": f"T_uint16_{i+1}",
                "generator": {
                    "type": "random_drift",
                    "center": 30000.0 + i * 100,
                    "max_delta": 1000.0,
                    "smoothness": 0.9
                }
            })

        # int16: 300-499
        for i in range(200):
            registers.append({
                "address": 300 + i,
                "name": f"T_int16_{i+1}",
                "generator": {
                    "type": "sine",
                    "frequency": 0.001 + i * 0.0001,
                    "amplitude": 10000.0,
                    "offset": 0.0
                }
            })

        # uint32: 500-699 (每点2寄存器) - 只需要低位寄存器
        for i in range(100):
            addr = 500 + i * 2
            registers.append({
                "address": addr,
                "name": f"T_uint32_lo_{i+1}",
                "generator": {
                    "type": "random_drift",
                    "center": 30000.0,
                    "max_delta": 5000.0,
                    "smoothness": 0.85
                }
            })
            registers.append({
                "address": addr + 1,
                "name": f"T_uint32_hi_{i+1}",
                "generator": {
                    "type": "random_drift",
                    "center": 100.0,
                    "max_delta": 50.0,
                    "smoothness": 0.85
                }
            })

        # float32: 700-1079 (每点2寄存器)
        for i in range(190):  # 100 ABCD + 30 DCBA + 30 BADC + 30 CDAB
            addr = 700 + i * 2
            registers.append({
                "address": addr,
                "name": f"T_float32_lo_{i+1}",
                "generator": {
                    "type": "sine",
                    "frequency": 0.002,
                    "amplitude": 30000.0,
                    "offset": 32000.0
                }
            })
            registers.append({
                "address": addr + 1,
                "name": f"T_float32_hi_{i+1}",
                "generator": {
                    "type": "constant",
                    "value": 16800.0  # ~= 0x4190 for float 18.x
                }
            })

        # bool (bit): 1080-1089
        for i in range(10):
            registers.append({
                "address": 1080 + i,
                "name": f"T_bool_{i+1}",
                "generator": {
                    "type": "random_drift",
                    "center": 32768.0,
                    "max_delta": 32767.0,
                    "smoothness": 0.5
                }
            })

        # 调节寄存器 (FC06): 2000-2049
        for i in range(50):
            registers.append({
                "address": 2000 + i,
                "name": f"A_{i+1}",
                "generator": {
                    "type": "constant",
                    "value": 0.0
                }
            })

        # 写入场景文件
        with open(scenario_file, "w") as f:
            f.write(f"# E2E {ch_cfg['name']} Test Scenario\n")
            f.write(f"# Generated by scripts/generate-e2e-config.py\n")
            f.write(f"# Port: {ch_cfg['port']}, Channel: {ch_id}\n\n")
            yaml.dump(scenario, f, default_flow_style=False, allow_unicode=True, sort_keys=False)

        print(f"  Generated: {scenario_file}")


def main():
    print("=" * 60)
    print("  AetherEMS E2E Configuration Generator")
    print("  4 Devices × 1000+ Points per Channel")
    print("=" * 60)
    print()

    # 项目根目录
    script_dir = Path(__file__).parent
    project_root = script_dir.parent
    config_path = project_root / "config.e2e" / "io"

    print("[1/3] Generating io.yaml (4 channels)...")
    generate_io_yaml(config_path)
    print()

    print("[2/3] Generating channel configurations...")
    total_t, total_s, total_c, total_a = 0, 0, 0, 0

    for ch_id, ch_cfg in CHANNELS.items():
        t, s, c, a = generate_channel_config(config_path, ch_id, ch_cfg)
        total_t += t
        total_s += s
        total_c += c
        total_a += a

    print()
    print(f"  Total: T={total_t} S={total_s} C={total_c} A={total_a}")
    print(f"  Grand Total: {total_t + total_s + total_c + total_a} points")
    print()

    print("[3/3] Generating simulator scenario...")
    generate_simulator_scenario(project_root)
    print()

    print("=" * 60)
    print("  Configuration generation complete!")
    print("=" * 60)


if __name__ == "__main__":
    main()
