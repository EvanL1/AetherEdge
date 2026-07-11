# Energy pack io 配置示例

本包包含通道 1（PCS#1）与通道 2（BAMS#1）的 YAML、四遥 CSV 与 Modbus 映射文件。

## 通道 2（BAMS#1）
- 遥测：按 62001 起的偏移规则映射；多字节采用 ABCD 字序。
- 遥信：展开如下 Map 为按位点位：BaFaultCode0..7（8×8 位）、BcuFaultMap、BcuAirFaultMap、BcuOnlineMap（各 16 位/寄存器）。
- 遥控：ClearFault/Reset/Start/Stop 作为写保持寄存器占位；确认寄存器号后替换。
- 遥调：Table11 62700..62703，默认 FC=6。
