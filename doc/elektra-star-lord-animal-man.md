# 完善“内存信息”功能（修订版）

## 目标

把现有半成品修到可构建、可运行、信息更完整的状态；DDR3/DDR4 复用现有 RAMSPDToolkit 的 JEDEC 解码，DDR5 保留现有 `spdr` 解码，不新增依赖。现有机器的历史 SPD 输出显示 PawnIO/兼容 SMBus 曾不可用，因此真实 SPD 硬件读取只能在机器当前状态允许时验收，不能用模拟数据冒充。

## 当前状态

- `cargo check --all-targets` 当前有 5 处 `.size()` 浮点字面量错误（`src/memory_page.rs` 第 622、639、675、678、717 行）和 1 处未使用导入；不是旧版方案写的 20 处。
- `dotnet build spd-bridge/MemoryLens.SpdBridge.csproj --no-restore` 可成功构建。
- `src/spd.rs` 目前只解析 DDR5；`spd-bridge/Program.cs` 已通过反射输出 DDR3/DDR4 timing 属性、`busName` 和 `dramManufacturer`，C# 侧只需在必要时补齐字段契约。
- WMI 的序列号已经读取，但界面尚未显示。
- 本机当前为 DDR5 平台，未检测到 PawnIO，当前进程也非管理员；本轮不能把真实 SPD 硬件读取作为完成条件。

## 实施步骤

### 1. 恢复可构建状态

- 修正 `src/memory_page.rs` 中上述 5 处 `.size(16)`、`.size(20)`、`.size(25)`、`.size(16)`、`.size(19)` 为浮点字面量，并移除未使用的 `MemoryModule` 导入。
- 运行 `cargo check --all-targets`，先消除现有错误，再继续功能改动。
- 本文件修订后、功能改动前建立一次基线提交；后续改动应能用 `git diff` 审查，避免把基线和功能改动混在同一提交中。

### 2. 接通 DDR3/DDR4 JEDEC 详细时序

- 继续使用桥接现有的 `timings` 名称/值数组；`ReadTimings` 已输出 `DDR3Timings` / `DDR4Timings` 的 decimal 属性和 `CASLatenciesSupported`。Rust 侧按名称查找，不依赖数组顺序。
- 明确规定桥接字段单位与格式：
  - timing 值为 invariant culture 的十进制纳秒字符串；
  - `CASLatenciesSupported` 为逗号分隔的整数时钟列表；
  - 缺失字段、空字符串、非数字、非正数或超出范围时显示“未提供”，不能 panic。
- Rust 侧把纳秒值转换为皮秒整数后再计算，避免浮点累计误差；需要转换的字段至少包括：
  - `MinimumCycleTime`
  - `MinimumCASLatencyTime`
  - `MinimumRASToCASDelayTime`
  - `MinimumRowPrechargeDelayTime`
  - `MinimumActiveToPrechargeDelayTime`
  - `MinimumActiveToActiveRefreshDelayTime`
  - `MinimumRefreshRecoveryDelayTime`（DDR3）及 `MinimumRefreshRecoveryDelayTime1/2/4`（DDR4）
  - `MinimumWriteRecoveryTime`
  - `MinimumWriteToReadTime` 及 DDR4 的 `_DiffGroup` / `_SameGroup` 字段
  - `MinimumReadToPrechargeTime`
  - `MinimumFourActivateWindowTime`
  - `MinimumActivateToActivateDelayTime` 及 DDR4 的 `_DiffGroup` / `_SameGroup` 字段
  - `MinimumCASToCASDelay_SameGroup`
- 换算规则：
  - `tCK_ps = round(MinimumCycleTime_ns × 1000)`；
  - 数据率 `MT/s = round(2000 / tCK_ns)`，显示时按整数 MT/s；
  - `CL = ceil(MinimumCASLatencyTime_ns / tCK_ns)`；
  - `tRCD`、`tRP`、`tRAS` 等周期数均按最小时间向上取整；
  - 计算出的 `CL` 必须存在于 `CASLatenciesSupported` 中；否则核心时序显示“未提供”，并列出支持的 CAS 列表，不能取列表首项或末项冒充。
- DDR3/DDR4 的 JEDEC CRC：
  - CRC 存储在 SPD 字节 126–127；
  - 计算范围是字节 0–125，共 126 字节；
  - 按小端读取存储的 CRC，使用现有 `spdr::crc16`（CRC-16/XMODEM）比较；
  - CRC 不匹配只显示警示，不阻止查看时序和原始数据；长度不足 128 字节时显示“未提供/无法校验”。
- 主区域生成与 DDR5 相同结构的 JEDEC 时序表，至少覆盖 tCK、tAA/CL、tRCD、tRP、tRAS、tRC、tWR、tRFC、tFAW、tRRD、tCCD、tWTR、tRTP 等该内存类型可用的字段。
- DDR3/DDR4 工具包原始字段放入折叠区；主区域不再直接把原始属性列表当作详细时序展示。
- DDR4 XMP 2.0 尚无现成解析器或可信样本，本轮不猜测字段偏移；界面明确区分“JEDEC 已解析”和“XMP 预设尚未解析”。DDR5 的 XMP/EXPO 解析保持现状。

### 3. 补全内存与 SPD 字段展示

- 在 `src/memory_page.rs` 的内存详情中显示 WMI 已读取的序列号；将序列号放在型号下方，避免把七列表格继续扩宽。
- 在 `src/spd.rs` 的桥接模型中接入 `busName`、`dramManufacturer`；在 SPD 模块详情中显示：
  - 总线名称（为空时退回总线序号）；
  - SPD 地址；
  - 模块厂商；
  - DRAM 颗粒厂商；
  - 型号；
  - 序列号。
- 空值统一显示“未提供”。
- SPD 与 WMI 的插槽匹配规则：
  - 先按归一化后的序列号（去首尾空格、转大写、去除内部空白）唯一匹配；
  - 序列号不可用时，退回到归一化型号唯一匹配；
  - 同型号存在多根、序列号缺失或匹配不唯一时，不显示插槽关联，显示“未关联插槽”；
  - 不能仅凭型号在有多根同型号内存时强行关联。
- 修正 `speed_summary` / `kind_summary`：
  - 所有模块字段完整且值相同：显示单一值；
  - 字段完整但存在不同值：显示“混插”并列出不同值；
  - 部分模块缺字段：优先显示“部分未提供”，不能因为剩余值相同就显示单一值，也不能误报“混插”。
- 槽位条增加“数量示意，非实际槽位位置/顺序”的说明。

### 4. 完善交互与结果可信度

- 切换 SPD 内存条时，在模块选择变化后立即把 `selected_profile` 重置为 0，避免沿用上一根的 XMP/JEDEC 索引；预设数量变化时再安全钳制索引。
- 保持 WMI/SPD/测速后台执行、取消测速、低可用内存检查及错误诊断的现有行为。
- 更新界面说明：
  - SPD 预设、固件频率和 SPD 时序都不是实时总线测量；
  - DDR3/DDR4 当前只解析 JEDEC，DDR4 XMP 2.0 尚未解析；
  - DDR5 才显示其已有的 JEDEC/XMP/EXPO 结果。

### 5. 测试、格式化与验收

- 为纯逻辑增加单元测试，不依赖真实硬件：
  - CRC 正确、CRC 错误、长度不足；
  - 纳秒到皮秒转换、最小时间向上取整；
  - 数据率取整和 CL 支持列表校验；
  - `speed_summary` / `kind_summary` 的“完整一致 / 混插 / 部分未提供”三类状态。
- 测试用的合成 SPD 字节只用于算法验证，不能冒充真实硬件读取结果。
- 对 Rust 和 C# 改动区域按现有风格格式化。运行 `cargo fmt --all` 前先确认基线提交已存在；如产生大量无关文件 diff，优先只格式化本次改动文件。
- 运行：
  - `cargo check --all-targets`
  - `cargo test`
  - `cargo build`
  - `dotnet build spd-bridge/MemoryLens.SpdBridge.csproj --no-restore --nologo`
- 启动新构建的 GUI，进入“内存信息”，检查 WMI 数据、SPD 选择器、序列号/厂商展示、预设切换和测速开始/取消；使用项目现有截图/点击脚本辅助验收。
- 只有机器已有可用 PawnIO 和兼容 SMBus 时才执行真实 SPD 读取；若驱动要求重启或硬件不兼容，保留明确错误路径并如实报告未验证项，不擅自重启或绕过驱动流程。
- 最终报告必须列出：
  - 已验证的编译、单元测试和 GUI 行为；
  - 仍无法在本机验证的 DDR3/DDR4/DDR5 真实 SPD 场景；
  - 尚未实现的 DDR4 XMP 2.0。
