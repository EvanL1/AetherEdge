#!/usr/bin/env python3
"""Modbus 协议验证测试脚本 - 测试 FC01/02/03/05/0F 功能码"""

import socket
import struct
import sys


def modbus_tcp_request(sock, tx_id, unit_id, fc, data):
    """发送 Modbus TCP 请求并返回响应"""
    pdu = bytes([fc]) + data
    length = len(pdu) + 1  # PDU + unit_id
    mbap = struct.pack(">HHHB", tx_id, 0, length, unit_id)
    sock.send(mbap + pdu)
    resp = sock.recv(256)
    return resp


def test_read_coils(sock, unit_id, addr, count):
    """FC01: 读线圈"""
    data = struct.pack(">HH", addr, count)
    resp = modbus_tcp_request(sock, 1, unit_id, 0x01, data)
    if len(resp) < 9:
        return None
    byte_count = resp[8]
    coil_bytes = resp[9 : 9 + byte_count]
    # 解包线圈
    coils = []
    for i in range(count):
        byte_idx = i // 8
        bit_idx = i % 8
        if byte_idx < len(coil_bytes):
            coils.append((coil_bytes[byte_idx] >> bit_idx) & 1 == 1)
    return coils


def test_write_single_coil(sock, unit_id, addr, value):
    """FC05: 写单线圈"""
    value_raw = 0xFF00 if value else 0x0000
    data = struct.pack(">HH", addr, value_raw)
    resp = modbus_tcp_request(sock, 2, unit_id, 0x05, data)
    if len(resp) < 12:
        return False
    return resp[7] == 0x05  # 检查功能码


def test_read_discrete_inputs(sock, unit_id, addr, count):
    """FC02: 读离散输入"""
    data = struct.pack(">HH", addr, count)
    resp = modbus_tcp_request(sock, 3, unit_id, 0x02, data)
    if len(resp) < 9:
        return None
    byte_count = resp[8]
    input_bytes = resp[9 : 9 + byte_count]
    # 解包
    inputs = []
    for i in range(count):
        byte_idx = i // 8
        bit_idx = i % 8
        if byte_idx < len(input_bytes):
            inputs.append((input_bytes[byte_idx] >> bit_idx) & 1 == 1)
    return inputs


def test_read_registers(sock, unit_id, addr, count):
    """FC03: 读寄存器"""
    data = struct.pack(">HH", addr, count)
    resp = modbus_tcp_request(sock, 4, unit_id, 0x03, data)
    if len(resp) < 9 + count * 2:
        return None
    values = struct.unpack(">" + "H" * count, resp[9 : 9 + count * 2])
    return values


def test_write_multiple_coils(sock, unit_id, addr, values):
    """FC0F: 写多线圈"""
    quantity = len(values)
    byte_count = (quantity + 7) // 8
    # 打包线圈
    coil_bytes = [0] * byte_count
    for i, v in enumerate(values):
        if v:
            coil_bytes[i // 8] |= 1 << (i % 8)
    data = struct.pack(">HHB", addr, quantity, byte_count) + bytes(coil_bytes)
    resp = modbus_tcp_request(sock, 5, unit_id, 0x0F, data)
    if len(resp) < 12:
        return False
    return resp[7] == 0x0F


def main():
    print("=" * 60)
    print("   Modbus 协议验证测试 (FC01/02/03/05/0F)")
    print("=" * 60)

    try:
        sock = socket.socket()
        sock.settimeout(5)
        sock.connect(("127.0.0.1", 5030))

        tests_passed = 0
        tests_failed = 0

        # 测试 1: FC01 读线圈 (地址 0-7 应该是 0x55 = 01010101)
        print("\n[FC01] 读线圈测试...")
        coils = test_read_coils(sock, 1, 0, 8)
        expected = [True, False, True, False, True, False, True, False]
        if coils == expected:
            print(f"  ✓ 地址 0-7 读取正确: {coils}")
            tests_passed += 1
        else:
            print(f"  ✗ 地址 0-7 读取错误: 期望 {expected}, 实际 {coils}")
            tests_failed += 1

        # 测试 2: FC01 读线圈 (地址 8-15 应该是 0xAA = 10101010)
        coils = test_read_coils(sock, 1, 8, 8)
        expected = [False, True, False, True, False, True, False, True]
        if coils == expected:
            print(f"  ✓ 地址 8-15 读取正确: {coils}")
            tests_passed += 1
        else:
            print(f"  ✗ 地址 8-15 读取错误: 期望 {expected}, 实际 {coils}")
            tests_failed += 1

        # 测试 3: FC02 读离散输入 (地址 0-7 应该是 0x81)
        print("\n[FC02] 读离散输入测试...")
        inputs = test_read_discrete_inputs(sock, 1, 0, 8)
        expected = [True, False, False, False, False, False, False, True]
        if inputs == expected:
            print(f"  ✓ 离散输入读取正确: {inputs}")
            tests_passed += 1
        else:
            print(f"  ✗ 离散输入读取错误: 期望 {expected}, 实际 {inputs}")
            tests_failed += 1

        # 测试 4: FC05 写单线圈
        print("\n[FC05] 写单线圈测试...")
        success = test_write_single_coil(sock, 1, 1000, True)
        if success:
            # 回读验证
            coils = test_read_coils(sock, 1, 1000, 1)
            if coils == [True]:
                print("  ✓ 写单线圈成功并验证")
                tests_passed += 1
            else:
                print("  ✗ 写单线圈回读失败")
                tests_failed += 1
        else:
            print("  ✗ 写单线圈失败")
            tests_failed += 1

        # 测试 5: FC0F 写多线圈
        print("\n[FC0F] 写多线圈测试...")
        values_to_write = [True, True, False, True, False, False, True, True]
        success = test_write_multiple_coils(sock, 1, 2000, values_to_write)
        if success:
            coils = test_read_coils(sock, 1, 2000, 8)
            if coils == values_to_write:
                print(f"  ✓ 写多线圈成功并验证: {coils}")
                tests_passed += 1
            else:
                print(
                    f"  ✗ 写多线圈回读失败: 期望 {values_to_write}, 实际 {coils}"
                )
                tests_failed += 1
        else:
            print("  ✗ 写多线圈失败")
            tests_failed += 1

        # 测试 6: FC03 读寄存器 - 位域测试
        print("\n[FC03] 寄存器位域测试...")
        regs = test_read_registers(sock, 1, 100, 1)
        if regs and regs[0] == 0x8421:
            print(
                f"  ✓ 寄存器 100 = 0x{regs[0]:04X} (bit0=1, bit5=1, bit10=1, bit15=1)"
            )
            tests_passed += 1
        else:
            print(f"  ✗ 寄存器 100 错误: 期望 0x8421, 实际 {regs}")
            tests_failed += 1

        # 测试 7: Float32 字节序
        print("\n[FC03] Float32 字节序测试...")
        regs = test_read_registers(sock, 1, 200, 2)
        if regs and regs[0] == 0x4228 and regs[1] == 0x0000:
            # 解析 ABCD 格式
            bytes_data = struct.pack(">HH", regs[0], regs[1])
            float_val = struct.unpack(">f", bytes_data)[0]
            if abs(float_val - 42.0) < 0.001:
                print(f"  ✓ Float32 ABCD 解析正确: {float_val}")
                tests_passed += 1
            else:
                print(f"  ✗ Float32 ABCD 解析错误: 期望 42.0, 实际 {float_val}")
                tests_failed += 1
        else:
            print(f"  ✗ Float32 寄存器读取错误: {regs}")
            tests_failed += 1

        sock.close()

        print("\n" + "=" * 60)
        print(f"   测试结果: {tests_passed} 通过, {tests_failed} 失败")
        print("=" * 60)

        sys.exit(0 if tests_failed == 0 else 1)

    except Exception as e:
        print(f"  ✗ 测试异常: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()
