#!/usr/bin/env python3
"""E2E Modbus readback verification.

After io API or automation action writes, connect directly to simulators
via Modbus TCP and read back values to confirm writes reached the device.

No third-party dependencies required (stdlib only: socket, struct, argparse).

Usage:
    python3 scripts/e2e_modbus_readback.py --phase 7   # verify Phase 7 C/A writes
    python3 scripts/e2e_modbus_readback.py --phase 9   # verify Phase 9 M2C actions
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

# ── Modbus TCP primitives ──────────────────────────────────────────────

_TX_COUNTER = 0


def modbus_tcp_request(sock, unit_id, fc, data):
    """Send a Modbus TCP request and return the raw response."""
    global _TX_COUNTER
    _TX_COUNTER += 1
    pdu = bytes([fc]) + data
    length = len(pdu) + 1
    mbap = struct.pack(">HHHB", _TX_COUNTER, 0, length, unit_id)
    sock.send(mbap + pdu)
    resp = sock.recv(512)
    return resp


def read_coils(sock, unit_id, addr, count):
    """FC01: read coils, returns list of bool."""
    data = struct.pack(">HH", addr, count)
    resp = modbus_tcp_request(sock, unit_id, 0x01, data)
    if len(resp) < 9:
        return None
    byte_count = resp[8]
    coil_bytes = resp[9 : 9 + byte_count]
    coils = []
    for i in range(count):
        byte_idx = i // 8
        bit_idx = i % 8
        if byte_idx < len(coil_bytes):
            coils.append((coil_bytes[byte_idx] >> bit_idx) & 1 == 1)
    return coils


def read_registers(sock, unit_id, addr, count):
    """FC03: read holding registers, returns tuple of uint16."""
    data = struct.pack(">HH", addr, count)
    resp = modbus_tcp_request(sock, unit_id, 0x03, data)
    if len(resp) < 9 + count * 2:
        return None
    values = struct.unpack(">" + "H" * count, resp[9 : 9 + count * 2])
    return values


# ── Readback verification ──────────────────────────────────────────────


def write_coil(sim_port, unit_id, addr, value, retries=3):
    """FC05: write single coil to simulator. Used to reset Phase 7 contamination."""
    for attempt in range(retries):
        sock = None
        try:
            sock = socket.socket()
            sock.settimeout(5)
            sock.connect(("127.0.0.1", sim_port))
            coil_val = 0xFF00 if value else 0x0000
            data = struct.pack(">HH", addr, coil_val)
            resp = modbus_tcp_request(sock, unit_id, 0x05, data)
            if len(resp) >= 9:
                return True, f"coil[{addr}]={'ON' if value else 'OFF'}"
            if attempt < retries - 1:
                time.sleep(0.3)
                continue
            return False, f"no response from port {sim_port}"
        except Exception as e:
            if attempt < retries - 1:
                time.sleep(0.3)
                continue
            return False, f"{e}"
        finally:
            if sock:
                sock.close()
    return False, "exhausted retries"


def verify_coil(sim_port, unit_id, addr, expected, description, retries=3):
    """Read a single coil from simulator and compare to expected value."""
    for attempt in range(retries):
        sock = None
        try:
            sock = socket.socket()
            sock.settimeout(5)
            sock.connect(("127.0.0.1", sim_port))
            result = read_coils(sock, unit_id, addr, 1)
            if result is None:
                if attempt < retries - 1:
                    time.sleep(0.3)
                    continue
                return False, f"{description}: no response from port {sim_port}"
            actual = result[0]
            if actual == expected:
                return True, f"{description}: coil[{addr}]={actual}"
            if attempt < retries - 1:
                time.sleep(0.3)
                continue
            return False, f"{description}: coil[{addr}] expected={expected} got={actual}"
        except Exception as e:
            if attempt < retries - 1:
                time.sleep(0.3)
                continue
            return False, f"{description}: {e}"
        finally:
            if sock:
                sock.close()
    return False, f"{description}: exhausted retries"


def verify_register(sim_port, unit_id, addr, expected, description, retries=3):
    """Read a single holding register from simulator and compare to expected."""
    for attempt in range(retries):
        sock = None
        try:
            sock = socket.socket()
            sock.settimeout(5)
            sock.connect(("127.0.0.1", sim_port))
            result = read_registers(sock, unit_id, addr, 1)
            if result is None:
                if attempt < retries - 1:
                    time.sleep(0.3)
                    continue
                return False, f"{description}: no response from port {sim_port}"
            actual = result[0]
            if actual == expected:
                return True, f"{description}: reg[{addr}]={actual}"
            if attempt < retries - 1:
                time.sleep(0.3)
                continue
            return False, f"{description}: reg[{addr}] expected={expected} got={actual}"
        except Exception as e:
            if attempt < retries - 1:
                time.sleep(0.3)
                continue
            return False, f"{description}: {e}"
        finally:
            if sock:
                sock.close()
    return False, f"{description}: exhausted retries"


# ── Phase definitions ──────────────────────────────────────────────────

# Phase 7: io direct writes (C/A → FC05/FC06)
# These match the test_write calls in ci-e2e-test.sh Phase 7.
#
# Address mapping (from config.e2e/io/*/mapping/*.csv):
#   C point_id N → FC05 coil addr (199+N)
#   A point_id N → FC06 reg  addr (1999+N)
#
# Channel → simulator port:
#   1001 (PV)      → 5020
#   1002 (Battery)  → 5021
#   1003 (Diesel)   → 5022
#   1004 (Load)     → 5023

PHASE7_COIL_CASES = [
    # (sim_port, unit_id, coil_addr, expected_bool, description)
    (5020, 1, 200, True, "PV C1=ON"),
    (5020, 1, 201, False, "PV C2=OFF"),
    (5021, 1, 200, True, "Battery C1=ON"),
    (5022, 1, 200, True, "Diesel C1=ON"),
]

PHASE7_REG_CASES = [
    # (sim_port, unit_id, reg_addr, expected_uint16, description)
    (5020, 1, 2000, 4500, "PV A1=4500"),
    (5020, 1, 2001, 3200, "PV A2=3200"),
    (5021, 1, 2000, 5000, "Battery A1=5000"),
    (5022, 1, 2000, 6000, "Diesel A1=6000"),
]

# Phase 9: automation M2C actions (inst action → channel C → FC05 coil)
#
# M2C routing (from config.e2e/automation/instances/*/channel_routing.csv):
#   inst:2 (Battery) A:1→1002 C:1, A:2→1002 C:2, A:3→1002 C:3
#   inst:3 (Diesel)  A:1→1003 C:1, A:2→1003 C:2
#
# Coil 200 was written true by Phase 7, so before Phase 9 actions we reset
# it to false (via --reset-coils). This isolates the M2C path: if coil 200
# reads true after M2C action, it proves A1→C1 routing actually worked.

PHASE9_RESET_COILS = [
    # (sim_port, unit_id, coil_addr, value, description)
    (5021, 1, 200, False, "Reset Battery coil[200] (Phase 7 contamination)"),
    (5022, 1, 200, False, "Reset Diesel coil[200] (Phase 7 contamination)"),
]

PHASE9_COIL_CASES = [
    # (sim_port, unit_id, coil_addr, expected_bool, description)
    (5021, 1, 200, True, "Battery A1 via M2C (C:1)"),
    (5021, 1, 201, True, "Battery A2 via M2C (C:2)"),
    (5021, 1, 202, True, "Battery A3 via M2C (C:3)"),
    (5022, 1, 200, True, "Diesel A1 via M2C (C:1)"),
    (5022, 1, 201, True, "Diesel A2 via M2C (C:2)"),
]


def reset_phase9_coils():
    """Reset coils contaminated by Phase 7, so Phase 9 can isolate M2C path."""
    print(f"{LINE_V}")
    print(f"{LINE_V} Resetting Phase 7 coils to isolate M2C routing path...")
    all_ok = True
    for port, uid, addr, value, desc in PHASE9_RESET_COILS:
        ok, detail = write_coil(port, uid, addr, value)
        status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
        print(f"{LINE_V}   {status} {desc}: {detail}")
        if not ok:
            all_ok = False
    # Verify coils are actually false after reset
    time.sleep(0.3)
    for port, uid, addr, _, desc in PHASE9_RESET_COILS:
        ok, detail = verify_coil(port, uid, addr, False, f"Confirm {desc}")
        status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
        print(f"{LINE_V}   {status} Confirm reset: {detail}")
        if not ok:
            all_ok = False
    print(f"{LINE_V}")
    return 0 if all_ok else 1


def run_phase(phase):
    """Run readback verification for the given phase. Returns exit code."""
    # Allow async write to propagate through io to simulator
    # io API → CommandTxCache → channel task → Modbus FC05/FC06 → simulator
    time.sleep(1.0)

    passed = 0
    failed = 0

    if phase == 7:
        print(f"{LINE_V}")
        print(f"{LINE_V} Modbus readback: coil verification (FC01)...")
        for port, uid, addr, expected, desc in PHASE7_COIL_CASES:
            ok, detail = verify_coil(port, uid, addr, expected, desc)
            status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
            print(f"{LINE_V}   {status} {detail}")
            if ok:
                passed += 1
            else:
                failed += 1

        print(f"{LINE_V}")
        print(f"{LINE_V} Modbus readback: register verification (FC03)...")
        for port, uid, addr, expected, desc in PHASE7_REG_CASES:
            ok, detail = verify_register(port, uid, addr, expected, desc)
            status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
            print(f"{LINE_V}   {status} {detail}")
            if ok:
                passed += 1
            else:
                failed += 1

    elif phase == 9:
        print(f"{LINE_V}")
        print(f"{LINE_V} Modbus readback: M2C coil verification (FC01)...")
        print(
            f"{LINE_V}   {YELLOW}i{NC} Only checking coils NOT written in Phase 7 (isolates M2C path)"
        )
        for port, uid, addr, expected, desc in PHASE9_COIL_CASES:
            ok, detail = verify_coil(port, uid, addr, expected, desc)
            status = f"{GREEN}✓{NC}" if ok else f"{RED}✗{NC}"
            print(f"{LINE_V}   {status} {detail}")
            if ok:
                passed += 1
            else:
                failed += 1

    else:
        print(f"{LINE_V} {RED}Unknown phase: {phase}{NC}")
        return 1

    print(f"{LINE_V}")
    total = passed + failed
    if failed == 0:
        print(
            f"{LINE_V} {GREEN}✓ Modbus readback: {passed}/{total} verified{NC}"
        )
    else:
        print(
            f"{LINE_V} {RED}✗ Modbus readback: {failed}/{total} failed{NC}"
        )
    return 0 if failed == 0 else 1


def main():
    parser = argparse.ArgumentParser(description="E2E Modbus readback verification")
    parser.add_argument(
        "--phase", type=int, choices=[7, 9], help="Test phase to verify"
    )
    parser.add_argument(
        "--reset-coils",
        action="store_true",
        help="Reset Phase 7 coils before Phase 9 M2C verification",
    )
    args = parser.parse_args()

    if args.reset_coils:
        sys.exit(reset_phase9_coils())
    elif args.phase:
        sys.exit(run_phase(args.phase))
    else:
        parser.error("either --phase or --reset-coils is required")


if __name__ == "__main__":
    main()
