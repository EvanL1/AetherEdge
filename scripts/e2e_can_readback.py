#!/usr/bin/env python3
"""E2E CAN/J1939 readback verification.

Reads CAN frames from vcan interfaces and verifies the simulator is sending
correct LYNK / J1939 frames with expected values based on device state.

Requires: Linux with python3-can (`pip install python-can`) or manual socket.
Uses raw SocketCAN for zero-dependency approach (Linux only).

Usage:
    python3 scripts/e2e_can_readback.py --type lynk --interface vcan0
    python3 scripts/e2e_can_readback.py --type j1939 --interface vcan1
"""

import argparse
import socket
import struct
import sys
import time

# Colors
GREEN = "\033[0;32m"
RED = "\033[0;31m"
YELLOW = "\033[1;33m"
NC = "\033[0m"
LINE_V = "\u2502"

# ── Raw SocketCAN primitives ─────────────────────────────────────────

CAN_RAW = 1
CAN_EFF_FLAG = 0x80000000
CAN_EFF_MASK = 0x1FFFFFFF
CAN_SFF_MASK = 0x000007FF
SOL_CAN_RAW = 101
CAN_RAW_FILTER = 1

# struct can_frame: <IB3x8s (id, len, pad, data)
CAN_FRAME_FMT = "<IB3x8s"
CAN_FRAME_SIZE = struct.calcsize(CAN_FRAME_FMT)


def open_can_socket(interface):
    """Open a raw SocketCAN socket bound to the given interface."""
    s = socket.socket(socket.AF_CAN, socket.SOCK_RAW, CAN_RAW)
    s.bind((interface,))
    s.settimeout(5.0)
    return s


def read_can_frame(sock):
    """Read a single CAN frame. Returns (can_id, is_extended, data_bytes)."""
    raw = sock.recv(CAN_FRAME_SIZE)
    can_id_raw, dlc, data = struct.unpack(CAN_FRAME_FMT, raw)
    is_extended = bool(can_id_raw & CAN_EFF_FLAG)
    if is_extended:
        can_id = can_id_raw & CAN_EFF_MASK
    else:
        can_id = can_id_raw & CAN_SFF_MASK
    return can_id, is_extended, data[:dlc]


# ── LYNK frame verification ─────────────────────────────────────────

LYNK_IDS = {0x351, 0x355, 0x356}


def collect_lynk_frames(sock, timeout=3.0):
    """Collect one frame per LYNK ID within timeout."""
    frames = {}
    deadline = time.time() + timeout
    while len(frames) < len(LYNK_IDS) and time.time() < deadline:
        try:
            can_id, is_ext, data = read_can_frame(sock)
            if not is_ext and can_id in LYNK_IDS:
                frames[can_id] = data
        except socket.timeout:
            break
    return frames


def verify_lynk(interface):
    """Verify LYNK frames on vcan interface."""
    print(f"{LINE_V}")
    print(f"{LINE_V} CAN LYNK readback on {interface}...")

    sock = open_can_socket(interface)
    frames = collect_lynk_frames(sock)
    sock.close()

    passed = 0
    failed = 0

    # 0x351 BatteryLimits
    if 0x351 in frames:
        data = frames[0x351]
        charge_v = struct.unpack_from("<h", data, 0)[0]
        charge_i = struct.unpack_from("<h", data, 2)[0]
        discharge_i = struct.unpack_from("<h", data, 4)[0]
        discharge_v = struct.unpack_from("<h", data, 6)[0]
        ok = charge_v == 560 and charge_i == 200 and discharge_i == 200 and discharge_v == 480
        status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
        detail = f"0x351 charge_v={charge_v} charge_i={charge_i} discharge_i={discharge_i} discharge_v={discharge_v}"
        print(f"{LINE_V}   {status} {detail}")
        passed += 1 if ok else 0
        failed += 0 if ok else 1
    else:
        print(f"{LINE_V}   {RED}✗{NC} 0x351 BatteryLimits: no frame received")
        failed += 1

    # 0x355 BatteryStatus
    if 0x355 in frames:
        data = frames[0x355]
        soc = struct.unpack_from("<H", data, 0)[0]
        soh = struct.unpack_from("<H", data, 2)[0]
        ok = soh == 98 and soc in (50, 75, 10)  # depends on state
        status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
        detail = f"0x355 soc={soc} soh={soh}"
        print(f"{LINE_V}   {status} {detail}")
        passed += 1 if ok else 0
        failed += 0 if ok else 1
    else:
        print(f"{LINE_V}   {RED}✗{NC} 0x355 BatteryStatus: no frame received")
        failed += 1

    # 0x356 BatteryMeasurements
    if 0x356 in frames:
        data = frames[0x356]
        voltage = struct.unpack_from("<h", data, 0)[0]
        current = struct.unpack_from("<h", data, 2)[0]
        temp = struct.unpack_from("<h", data, 4)[0]
        ok = voltage == 520 and temp == 250
        status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
        detail = f"0x356 voltage={voltage} current={current} temp={temp}"
        print(f"{LINE_V}   {status} {detail}")
        passed += 1 if ok else 0
        failed += 0 if ok else 1
    else:
        print(f"{LINE_V}   {RED}✗{NC} 0x356 BatteryMeasurements: no frame received")
        failed += 1

    return passed, failed


# ── J1939 frame verification ────────────────────────────────────────

# PGN extraction from 29-bit CAN ID: bits 8-25 contain PGN
PGN_EEC1 = 61444  # 0xF004
PGN_ET1 = 65262   # 0xFEEE


def extract_pgn(can_id):
    """Extract PGN from 29-bit J1939 CAN ID."""
    pf = (can_id >> 16) & 0xFF
    ps = (can_id >> 8) & 0xFF
    if pf >= 240:
        return (pf << 8) | ps
    else:
        return pf << 8


def collect_j1939_frames(sock, timeout=3.0):
    """Collect one EEC1 and one ET1 frame within timeout."""
    frames = {}
    target_pgns = {PGN_EEC1, PGN_ET1}
    deadline = time.time() + timeout
    while len(frames) < len(target_pgns) and time.time() < deadline:
        try:
            can_id, is_ext, data = read_can_frame(sock)
            if is_ext:
                pgn = extract_pgn(can_id)
                if pgn in target_pgns:
                    frames[pgn] = data
        except socket.timeout:
            break
    return frames


def verify_j1939(interface):
    """Verify J1939 frames on vcan interface."""
    print(f"{LINE_V}")
    print(f"{LINE_V} J1939 readback on {interface}...")

    sock = open_can_socket(interface)
    frames = collect_j1939_frames(sock)
    sock.close()

    passed = 0
    failed = 0

    # EEC1 — SPN 190 Engine Speed at bytes 3-4
    if PGN_EEC1 in frames:
        data = frames[PGN_EEC1]
        raw_speed = struct.unpack_from("<H", data, 3)[0]
        rpm = raw_speed * 0.125
        # Accept 0 (standby) or 1500 (running)
        ok = raw_speed in (0, 12000)
        status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
        detail = f"EEC1 SPN190 raw={raw_speed} ({rpm:.1f} RPM)"
        print(f"{LINE_V}   {status} {detail}")
        passed += 1 if ok else 0
        failed += 0 if ok else 1
    else:
        print(f"{LINE_V}   {RED}✗{NC} EEC1 (PGN {PGN_EEC1}): no frame received")
        failed += 1

    # ET1 — SPN 110 Coolant Temp at byte 0
    if PGN_ET1 in frames:
        data = frames[PGN_ET1]
        raw_temp = data[0]
        temp_c = raw_temp - 40
        ok = raw_temp in (65, 125, 160)  # 25°C, 85°C, 120°C
        status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
        detail = f"ET1 SPN110 raw={raw_temp} ({temp_c}°C)"
        print(f"{LINE_V}   {status} {detail}")
        passed += 1 if ok else 0
        failed += 0 if ok else 1
    else:
        print(f"{LINE_V}   {RED}✗{NC} ET1 (PGN {PGN_ET1}): no frame received")
        failed += 1

    return passed, failed


# ── Main ─────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="E2E CAN/J1939 readback verification")
    parser.add_argument(
        "--type",
        choices=["lynk", "j1939"],
        required=True,
        help="Protocol type to verify",
    )
    parser.add_argument(
        "--interface",
        required=True,
        help="vcan interface name (e.g., vcan0)",
    )
    args = parser.parse_args()

    if sys.platform != "linux":
        print(f"{LINE_V} {YELLOW}i{NC} CAN readback requires Linux — skipping")
        sys.exit(0)

    if args.type == "lynk":
        passed, failed = verify_lynk(args.interface)
    else:
        passed, failed = verify_j1939(args.interface)

    total = passed + failed
    print(f"{LINE_V}")
    if failed == 0:
        print(f"{LINE_V} {GREEN}✓ CAN readback: {passed}/{total} verified{NC}")
    else:
        print(f"{LINE_V} {RED}✗ CAN readback: {failed}/{total} failed{NC}")
    sys.exit(0 if failed == 0 else 1)


if __name__ == "__main__":
    main()
