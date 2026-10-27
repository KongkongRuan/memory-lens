use serde::Deserialize;
use spdr::{Expo, Picoseconds, RatedTimings, TimingPair, Timings, Xmp};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

include!(concat!(env!("OUT_DIR"), "/spd_bridge_files.rs"));

const BRIDGE_TIMEOUT: Duration = Duration::from_secs(150);
const BRIDGE_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROFILE_SUBTITLE: &str = "SPD 预设，非当前运行时值";
const UNAVAILABLE: &str = "未提供";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct SpdDocument {
    pub driver: String,
    pub warnings: Vec<String>,
    pub diagnostics: String,
    pub modules: Vec<SpdModule>,
}

#[derive(Clone, Debug)]
pub struct SpdModule {
    pub bus: i32,
    pub bus_name: String,
    pub address: u8,
    pub memory_type: String,
    pub capacity_gb: f32,
    pub manufacturer: String,
    pub dram_manufacturer: String,
    pub part_number: String,
    pub serial_number: String,
    pub raw: Vec<u8>,
    pub profiles: Vec<TimingProfile>,
    pub legacy_timings: Vec<LegacyTiming>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LegacyTiming {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct TimingProfile {
    pub title: String,
    pub subtitle: String,
    pub core_timings: String,
    pub data_rate_mt_s: Option<u32>,
    pub voltage_summary: String,
    pub crc_valid: Option<bool>,
    pub rows: Vec<TimingRow>,
}

#[derive(Clone, Debug)]
pub struct TimingRow {
    pub parameter: String,
    pub cycles: String,
    pub time: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeResponse {
    success: bool,
    #[serde(default)]
    driver: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    diagnostics: String,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    modules: Vec<BridgeModule>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeModule {
    bus: i32,
    #[serde(default)]
    bus_name: String,
    address: u8,
    #[serde(default)]
    memory_type: String,
    #[serde(default)]
    capacity_gb: f32,
    #[serde(default)]
    manufacturer: String,
    #[serde(default)]
    dram_manufacturer: String,
    #[serde(default)]
    part_number: String,
    #[serde(default)]
    serial_number: String,
    #[serde(default)]
    raw_hex: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    timings: Vec<BridgeTiming>,
}

#[derive(Deserialize)]
struct BridgeTiming {
    name: String,
    value: String,
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct BridgeTempDirectory {
    path: PathBuf,
}

impl BridgeTempDirectory {
    fn create() -> Result<Self, String> {
        let base = std::env::temp_dir();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        for _ in 0..128 {
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "memory-lens-spd-{}-{timestamp}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "无法创建 SPD 桥接临时目录 {}：{error}",
                        path.display()
                    ));
                }
            }
        }

        Err("无法分配唯一的 SPD 桥接临时目录".to_owned())
    }

    fn extract(&self) -> Result<(), String> {
        for &(name, bytes) in SPD_BRIDGE_FILES {
            let path = safe_extraction_path(&self.path, name)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!("无法创建 SPD 桥接子目录 {}：{error}", parent.display())
                })?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| format!("无法创建 SPD 桥接文件 {}：{error}", path.display()))?;
            file.write_all(bytes)
                .map_err(|error| format!("无法写入 SPD 桥接文件 {}：{error}", path.display()))?;
        }
        Ok(())
    }
}

impl Drop for BridgeTempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn read_spd() -> Result<SpdDocument, String> {
    let output = run_embedded_bridge()?;
    let response: BridgeResponse = serde_json::from_slice(&output.stdout).map_err(|error| {
        with_stderr(
            format!(
                "无法解析 SPD 桥接 JSON（{}）：{error}",
                exit_status_text(output.status)
            ),
            &output.stderr,
        )
    })?;

    if !response.success {
        let message = if response.error.trim().is_empty() {
            "SPD 桥接报告失败，但未提供 error".to_owned()
        } else {
            format!("SPD 桥接失败：{}", response.error.trim())
        };
        return Err(with_stderr(message, &output.stderr));
    }

    let mut document = SpdDocument {
        driver: response.driver,
        warnings: response.warnings,
        diagnostics: response.diagnostics,
        modules: response.modules.into_iter().map(convert_module).collect(),
    };

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        if !document.diagnostics.is_empty() {
            document.diagnostics.push('\n');
        }
        document.diagnostics.push_str("桥接标准错误：\n");
        document.diagnostics.push_str(stderr);
    }

    Ok(document)
}

fn run_embedded_bridge() -> Result<ProcessOutput, String> {
    let directory = BridgeTempDirectory::create()?;
    directory.extract()?;
    run_bridge(
        &directory.path.join("MemoryLens.SpdBridge.exe"),
        &directory.path,
    )
}

fn safe_extraction_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty() {
        return Err("SPD 桥接包含空文件名".to_owned());
    }

    let mut path = root.to_path_buf();
    for component in Path::new(name).components() {
        match component {
            Component::Normal(part) => path.push(part),
            _ => return Err(format!("SPD 桥接文件路径不安全：{name}")),
        }
    }
    Ok(path)
}

fn run_bridge(executable: &Path, working_directory: &Path) -> Result<ProcessOutput, String> {
    let mut command = Command::new(executable);
    command
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);

    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 SPD 桥接 {}：{error}", executable.display()))?;

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("无法接管 SPD 桥接的标准输出或标准错误".to_owned());
    };
    let stdout_reader = match spawn_reader(stdout, "spd-bridge-stdout") {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("无法创建 SPD 桥接标准输出读取线程：{error}"));
        }
    };
    let stderr_reader = match spawn_reader(stderr, "spd-bridge-stderr") {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = join_reader(stdout_reader, "标准输出");
            return Err(format!("无法创建 SPD 桥接标准错误读取线程：{error}"));
        }
    };

    let started = Instant::now();
    let mut timed_out = false;
    let status_result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() >= BRIDGE_TIMEOUT => {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| format!("终止超时的 SPD 桥接后无法等待其退出：{error}"));
            }
            Ok(None) => {
                let remaining = BRIDGE_TIMEOUT.saturating_sub(started.elapsed());
                thread::sleep(remaining.min(BRIDGE_POLL_INTERVAL));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("无法查询 SPD 桥接进程状态：{error}"));
            }
        }
    };

    let stdout_result = join_reader(stdout_reader, "标准输出");
    let stderr_result = join_reader(stderr_reader, "标准错误");
    let status = status_result?;
    let stdout = stdout_result?;
    let stderr = stderr_result?;

    if timed_out {
        return Err(with_stderr(
            "SPD 桥接执行超过 150 秒，已终止".to_owned(),
            &stderr,
        ));
    }

    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
    })
}

fn spawn_reader<R>(mut reader: R, name: &str) -> io::Result<JoinHandle<io::Result<Vec<u8>>>>
where
    R: Read + Send + 'static,
{
    thread::Builder::new().name(name.to_owned()).spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_reader(
    handle: JoinHandle<io::Result<Vec<u8>>>,
    stream_name: &str,
) -> Result<Vec<u8>, String> {
    handle
        .join()
        .map_err(|_| format!("SPD 桥接{stream_name}读取线程异常终止"))?
        .map_err(|error| format!("读取 SPD 桥接{stream_name}失败：{error}"))
}

fn exit_status_text(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "进程异常终止".to_owned(),
        |code| format!("退出码 {code}"),
    )
}

fn with_stderr(mut message: String, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        message.push_str("\n桥接标准错误：\n");
        message.push_str(stderr);
    }
    message
}

fn convert_module(module: BridgeModule) -> SpdModule {
    let BridgeModule {
        bus,
        bus_name,
        address,
        memory_type,
        capacity_gb,
        manufacturer,
        dram_manufacturer,
        part_number,
        serial_number,
        raw_hex,
        error,
        timings,
    } = module;

    let mut converted = SpdModule {
        bus,
        bus_name,
        address,
        memory_type,
        capacity_gb,
        manufacturer,
        dram_manufacturer,
        part_number,
        serial_number,
        raw: Vec::new(),
        profiles: Vec::new(),
        legacy_timings: timings
            .into_iter()
            .map(|timing| LegacyTiming {
                name: timing.name,
                value: timing.value,
            })
            .collect(),
        error: if error.trim().is_empty() {
            None
        } else {
            Some(error)
        },
    };

    let raw_is_valid = match decode_hex(&raw_hex) {
        Ok(raw) => {
            converted.raw = raw;
            true
        }
        Err(error) => {
            append_module_error(&mut converted.error, error);
            false
        }
    };

    if raw_is_valid {
        if is_ddr5(&converted.memory_type) {
            match decode_ddr5_profiles(&converted.raw) {
                Ok(profiles) => converted.profiles = profiles,
                Err(error) => append_module_error(&mut converted.error, error),
            }
        } else if is_ddr3(&converted.memory_type) || is_ddr4(&converted.memory_type) {
            let crc_valid = match verify_legacy_crc(&converted.raw) {
                Ok(valid) => Some(valid),
                Err(error) => {
                    append_module_error(&mut converted.error, error);
                    None
                }
            };
            converted.profiles = vec![decode_legacy_jedec_profile(&converted, crc_valid)];
        }
    }

    converted
}

fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err(format!(
            "rawHex 长度为奇数（{} 个字符），无法解码",
            bytes.len()
        ));
    }

    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        let high = hex_digit(pair[0])
            .ok_or_else(|| format!("rawHex 在字符位置 {} 包含非十六进制字符", index * 2))?;
        let low = hex_digit(pair[1])
            .ok_or_else(|| format!("rawHex 在字符位置 {} 包含非十六进制字符", index * 2 + 1))?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_ddr5(memory_type: &str) -> bool {
    memory_type.trim().to_ascii_uppercase().contains("DDR5")
}

fn is_ddr4(memory_type: &str) -> bool {
    memory_type.trim().to_ascii_uppercase().contains("DDR4")
}

fn is_ddr3(memory_type: &str) -> bool {
    memory_type.trim().to_ascii_uppercase().contains("DDR3")
}

fn verify_legacy_crc(raw: &[u8]) -> Result<bool, String> {
    if raw.len() < 128 {
        return Err(format!(
            "SPD 长度不足以校验 JEDEC CRC：至少需要 128 字节，实际为 {} 字节",
            raw.len()
        ));
    }
    let computed = spdr::crc16(&raw[..126]);
    let stored = u16::from_le_bytes([raw[126], raw[127]]);
    Ok(computed == stored)
}

fn decode_legacy_jedec_profile(module: &SpdModule, crc_valid: Option<bool>) -> TimingProfile {
    let tck = timing_ps(module, "MinimumCycleTime");
    let taa = timing_ps(module, "MinimumCASLatencyTime");
    let trcd = timing_ps(module, "MinimumRASToCASDelayTime");
    let trp = timing_ps(module, "MinimumRowPrechargeDelayTime");
    let tras = timing_ps(module, "MinimumActiveToPrechargeDelayTime");
    let trc = timing_ps(module, "MinimumActiveToActiveRefreshDelayTime");
    let twr = timing_ps(module, "MinimumWriteRecoveryTime");
    let tfaw = timing_ps(module, "MinimumFourActivateWindowTime");
    let cas = parse_cas_list(timing_value(module, "CASLatenciesSupported"));

    let cl = tck.zip(taa).and_then(|(tck, taa)| ceil_cycles(taa, tck));
    let cl_is_supported = cl.is_some_and(|value| cas.contains(&value));
    let t_rcd = timing_cycles(trcd, tck);
    let t_rp = timing_cycles(trp, tck);
    let t_ras = timing_cycles(tras, tck);
    let core_timings = if cl_is_supported {
        match (cl, t_rcd, t_rp, t_ras) {
            (Some(cl), Some(rcd), Some(rp), Some(ras)) => Some(format!("{cl}-{rcd}-{rp}-{ras}")),
            _ => None,
        }
    } else {
        None
    };

    let mut subtitle = PROFILE_SUBTITLE.to_owned();
    if cl.is_some() && !cl_is_supported {
        subtitle.push_str("；换算 CL 不在 CAS 支持列表");
    }
    if cas.is_empty() {
        subtitle.push_str("；CAS 支持列表未提供");
    }

    let mut rows = vec![
        clock_row_optional(tck),
        cl_timing_row(taa, if cl_is_supported { cl } else { None }),
        legacy_row("tRCD", trcd, tck),
        legacy_row("tRP", trp, tck),
        legacy_row("tRAS", tras, tck),
        legacy_row("tRC", trc, tck),
        legacy_row("tWR", twr, tck),
    ];

    if is_ddr4(&module.memory_type) {
        rows.extend([
            legacy_row(
                "tRFC1",
                timing_ps(module, "MinimumRefreshRecoveryDelayTime1"),
                tck,
            ),
            legacy_row(
                "tRFC2",
                timing_ps(module, "MinimumRefreshRecoveryDelayTime2"),
                tck,
            ),
            legacy_row(
                "tRFC4",
                timing_ps(module, "MinimumRefreshRecoveryDelayTime4"),
                tck,
            ),
            legacy_row(
                "tRRD_S",
                timing_ps(module, "MinimumActivateToActivateDelay_DiffGroup"),
                tck,
            ),
            legacy_row(
                "tRRD_L",
                timing_ps(module, "MinimumActivateToActivateDelay_SameGroup"),
                tck,
            ),
            legacy_row(
                "tCCD_L",
                timing_ps(module, "MinimumCASToCASDelay_SameGroup"),
                tck,
            ),
            legacy_row(
                "tWTR_S",
                timing_ps(module, "MinimumWriteToReadTime_DiffGroup"),
                tck,
            ),
            legacy_row(
                "tWTR_L",
                timing_ps(module, "MinimumWriteToReadTime_SameGroup"),
                tck,
            ),
        ]);
    } else {
        rows.extend([
            legacy_row(
                "tRFC",
                timing_ps(module, "MinimumRefreshRecoveryDelayTime"),
                tck,
            ),
            legacy_row(
                "tRRD",
                timing_ps(module, "MinimumActivateToActivateDelayTime"),
                tck,
            ),
            legacy_row("tWTR", timing_ps(module, "MinimumWriteToReadTime"), tck),
            legacy_row("tRTP", timing_ps(module, "MinimumReadToPrechargeTime"), tck),
        ]);
    }
    rows.push(legacy_row("tFAW", tfaw, tck));

    TimingProfile {
        title: "JEDEC 基础时序".to_owned(),
        subtitle,
        core_timings: core_timings.unwrap_or_else(|| UNAVAILABLE.to_owned()),
        data_rate_mt_s: tck.and_then(data_rate_from_tck),
        voltage_summary: String::new(),
        crc_valid,
        rows,
    }
}

fn timing_value<'a>(module: &'a SpdModule, name: &str) -> Option<&'a str> {
    module
        .legacy_timings
        .iter()
        .find(|timing| timing.name == name)
        .map(|timing| timing.value.as_str())
}

fn timing_ps(module: &SpdModule, name: &str) -> Option<u32> {
    parse_ns_to_ps(timing_value(module, name))
}

fn parse_ns_to_ps(value: Option<&str>) -> Option<u32> {
    let nanoseconds: f64 = value?.trim().parse().ok()?;
    if !nanoseconds.is_finite() || nanoseconds <= 0.0 {
        return None;
    }
    let picoseconds = (nanoseconds * 1000.0).round();
    if picoseconds < 1.0 || picoseconds > f64::from(u32::MAX) {
        return None;
    }
    Some(picoseconds as u32)
}

fn parse_cas_list(value: Option<&str>) -> Vec<u32> {
    let mut values: Vec<u32> = value
        .unwrap_or_default()
        .split(',')
        .filter_map(|part| part.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
        .collect();
    values.sort_unstable();
    values.dedup();
    values
}

fn data_rate_from_tck(tck_ps: u32) -> Option<u32> {
    if tck_ps == 0 {
        return None;
    }
    let rate = (2_000_000.0 / f64::from(tck_ps)).round();
    if rate < 1.0 || rate > f64::from(u32::MAX) {
        return None;
    }
    Some(rate as u32)
}

fn timing_cycles(value_ps: Option<u32>, tck_ps: Option<u32>) -> Option<u32> {
    value_ps
        .zip(tck_ps)
        .and_then(|(value, tck)| ceil_cycles(value, tck))
}

fn ceil_cycles(value_ps: u32, tck_ps: u32) -> Option<u32> {
    if tck_ps == 0 {
        return None;
    }
    let value = u64::from(value_ps);
    let tck = u64::from(tck_ps);
    Some(((value + tck - 1) / tck) as u32)
}

fn clock_row_optional(tck_ps: Option<u32>) -> TimingRow {
    TimingRow {
        parameter: "tCK".to_owned(),
        cycles: "1".to_owned(),
        time: tck_ps
            .map(format_ps)
            .unwrap_or_else(|| UNAVAILABLE.to_owned()),
    }
}

fn cl_timing_row(value_ps: Option<u32>, cl: Option<u32>) -> TimingRow {
    TimingRow {
        parameter: "tAA/CL".to_owned(),
        cycles: cl
            .map(|value| value.to_string())
            .unwrap_or_else(|| UNAVAILABLE.to_owned()),
        time: value_ps
            .map(format_ps)
            .unwrap_or_else(|| UNAVAILABLE.to_owned()),
    }
}

fn legacy_row(parameter: &str, value_ps: Option<u32>, tck_ps: Option<u32>) -> TimingRow {
    TimingRow {
        parameter: parameter.to_owned(),
        cycles: timing_cycles(value_ps, tck_ps)
            .map(|cycles| cycles.to_string())
            .unwrap_or_else(|| UNAVAILABLE.to_owned()),
        time: value_ps
            .map(format_ps)
            .unwrap_or_else(|| UNAVAILABLE.to_owned()),
    }
}

fn append_module_error(slot: &mut Option<String>, message: String) {
    match slot {
        Some(existing) if !existing.is_empty() => {
            existing.push_str("；");
            existing.push_str(&message);
        }
        _ => *slot = Some(message),
    }
}

fn decode_ddr5_profiles(raw: &[u8]) -> Result<Vec<TimingProfile>, String> {
    if raw.len() != 1024 {
        return Err(format!(
            "DDR5 SPD 长度应为 1024 字节，实际为 {} 字节",
            raw.len()
        ));
    }

    let timings =
        spdr::decode_timings(raw).map_err(|error| format!("DDR5 JEDEC 时序解码失败：{error}"))?;
    let base_crc =
        spdr::verify_base_crc(raw).map_err(|error| format!("DDR5 JEDEC CRC 校验失败：{error}"))?;
    let mut profiles = vec![jedec_profile(&timings, base_crc.matches)];

    let vendor = spdr::decode_vendor_profiles(raw)
        .map_err(|error| format!("DDR5 XMP/EXPO 解码失败：{error}"))?;
    if let Xmp::Present {
        profile1, profile2, ..
    } = vendor.xmp
    {
        for profile in [profile1, profile2].into_iter().flatten() {
            profiles.push(rated_profile(
                format!("XMP 3.0 配置 {}", profile.profile_number),
                &profile.rated,
                profile.crc.matches,
            ));
        }
    }
    if let Expo::Present {
        block_crc,
        profile1,
        profile2,
    } = vendor.expo
    {
        for profile in [profile1, profile2].into_iter().flatten() {
            profiles.push(rated_profile(
                format!("EXPO 配置 {}", profile.profile_number),
                &profile.rated,
                block_crc.matches,
            ));
        }
    }

    Ok(profiles)
}

fn jedec_profile(timings: &Timings, crc_valid: bool) -> TimingProfile {
    let tck = timings.tckavg_min.picoseconds();
    let data_rate = timings.base_data_rate_mt_s();
    TimingProfile {
        title: "JEDEC 基础时序".to_owned(),
        subtitle: PROFILE_SUBTITLE.to_owned(),
        core_timings: jedec_core_timings(timings).unwrap_or_else(|| UNAVAILABLE.to_owned()),
        data_rate_mt_s: (data_rate != 0).then_some(data_rate),
        voltage_summary: String::new(),
        crc_valid: Some(crc_valid),
        rows: vec![
            clock_row(tck),
            minimum_row("tAA/CL", timings.taa, tck),
            minimum_row("tRCD", timings.trcd, tck),
            minimum_row("tRP", timings.trp, tck),
            minimum_row("tRAS", timings.tras, tck),
            minimum_row("tRC", timings.trc, tck),
            minimum_row("tWR", timings.twr, tck),
            minimum_row("tRFC1", timings.trfc1, tck),
            minimum_row("tRFC2", timings.trfc2, tck),
            minimum_row("tRFCsb", timings.trfcsb, tck),
            pair_row("tRRD_L", timings.trrd_l, tck),
            pair_row("tCCD_L", timings.tccd_l, tck),
            pair_row("tCCD_L_WR", timings.tccd_l_wr, tck),
            pair_row("tCCD_L_WR2", timings.tccd_l_wr2, tck),
            pair_row("tFAW", timings.tfaw, tck),
            pair_row("tWTR_L", timings.twtr_l, tck),
            pair_row("tWTR_S", timings.twtr_s, tck),
            pair_row("tRTP", timings.trtp, tck),
        ],
    }
}

fn rated_profile(title: String, rated: &RatedTimings, crc_valid: bool) -> TimingProfile {
    let tck = rated.cycle_time.picoseconds();
    let cas_cycles = if tck == 0 {
        UNAVAILABLE.to_owned()
    } else {
        rated.cas_latency.to_string()
    };
    TimingProfile {
        title,
        subtitle: PROFILE_SUBTITLE.to_owned(),
        core_timings: rated_core_timings(rated).unwrap_or_else(|| UNAVAILABLE.to_owned()),
        data_rate_mt_s: (rated.data_rate_mt_s != 0).then_some(rated.data_rate_mt_s),
        voltage_summary: format!(
            "VDD {} / VDDQ {} / VPP {}",
            rated.vdd, rated.vddq, rated.vpp
        ),
        crc_valid: Some(crc_valid),
        rows: vec![
            clock_row(tck),
            TimingRow {
                parameter: "tAA/CL".to_owned(),
                cycles: cas_cycles,
                time: format_time(rated.taa),
            },
            rated_row("tRCD", rated.trcd, tck),
            rated_row("tRP", rated.trp, tck),
            rated_row("tRAS", rated.tras, tck),
        ],
    }
}

fn jedec_core_timings(timings: &Timings) -> Option<String> {
    let tck = timings.tckavg_min.picoseconds();
    Some(format!(
        "{}-{}-{}-{}",
        minimum_cycles(timings.taa, tck)?,
        minimum_cycles(timings.trcd, tck)?,
        minimum_cycles(timings.trp, tck)?,
        minimum_cycles(timings.tras, tck)?
    ))
}

fn rated_core_timings(rated: &RatedTimings) -> Option<String> {
    let tck = rated.cycle_time.picoseconds();
    if tck == 0 {
        return None;
    }
    Some(format!(
        "{}-{}-{}-{}",
        rated.cas_latency,
        rated_cycles(rated.trcd, tck)?,
        rated_cycles(rated.trp, tck)?,
        rated_cycles(rated.tras, tck)?
    ))
}

fn clock_row(tck: u32) -> TimingRow {
    TimingRow {
        parameter: "tCK".to_owned(),
        cycles: "1".to_owned(),
        time: format_ps(tck),
    }
}

fn minimum_row(parameter: &str, value: Picoseconds, tck: u32) -> TimingRow {
    TimingRow {
        parameter: parameter.to_owned(),
        cycles: minimum_cycles(value, tck)
            .map_or_else(|| UNAVAILABLE.to_owned(), |cycles| cycles.to_string()),
        time: format_time(value),
    }
}

fn rated_row(parameter: &str, value: Picoseconds, tck: u32) -> TimingRow {
    TimingRow {
        parameter: parameter.to_owned(),
        cycles: rated_cycles(value, tck)
            .map_or_else(|| UNAVAILABLE.to_owned(), |cycles| cycles.to_string()),
        time: format_time(value),
    }
}

fn pair_row(parameter: &str, pair: TimingPair, tck: u32) -> TimingRow {
    let stored = u32::from(pair.clocks.cycles());
    let cycles = match minimum_cycles(pair.time, tck) {
        Some(calculated) if calculated == stored => format!("≥ {stored}"),
        Some(calculated) => format!("≥ {stored}（时间换算 ≥ {calculated}）"),
        None => format!("≥ {stored}（时间换算{UNAVAILABLE}）"),
    };
    TimingRow {
        parameter: parameter.to_owned(),
        cycles,
        time: format_time(pair.time),
    }
}

fn minimum_cycles(value: Picoseconds, tck: u32) -> Option<u32> {
    ceil_cycles(value.picoseconds(), tck)
}

fn rated_cycles(value: Picoseconds, tck: u32) -> Option<u32> {
    if tck == 0 {
        return None;
    }
    let value = u64::from(value.picoseconds());
    let tck = u64::from(tck);
    Some(((value + tck / 2) / tck) as u32)
}

fn format_time(value: Picoseconds) -> String {
    format_ps(value.picoseconds())
}

fn format_ps(ps: u32) -> String {
    let mut time = format!("{}.{:03}", ps / 1000, ps % 1000);
    while time.ends_with('0') {
        time.pop();
    }
    if time.ends_with('.') {
        time.pop();
    }
    time.push_str(" ns");
    time
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_module(memory_type: &str, timings: &[(&str, &str)]) -> SpdModule {
        SpdModule {
            bus: 0,
            bus_name: "SMBus".to_owned(),
            address: 0x50,
            memory_type: memory_type.to_owned(),
            capacity_gb: 16.0,
            manufacturer: "Module Vendor".to_owned(),
            dram_manufacturer: "DRAM Vendor".to_owned(),
            part_number: "TEST-PART".to_owned(),
            serial_number: "TEST-SERIAL".to_owned(),
            raw: Vec::new(),
            profiles: Vec::new(),
            legacy_timings: timings
                .iter()
                .map(|(name, value)| LegacyTiming {
                    name: (*name).to_owned(),
                    value: (*value).to_owned(),
                })
                .collect(),
            error: None,
        }
    }

    fn valid_legacy_raw() -> Vec<u8> {
        let mut raw = vec![0u8; 256];
        for (index, byte) in raw.iter_mut().take(126).enumerate() {
            *byte = (index as u8).wrapping_mul(17).wrapping_add(3);
        }
        let crc = spdr::crc16(&raw[..126]);
        raw[126..128].copy_from_slice(&crc.to_le_bytes());
        raw
    }

    #[test]
    fn legacy_crc_covers_first_126_bytes() {
        let mut raw = valid_legacy_raw();
        assert_eq!(verify_legacy_crc(&raw), Ok(true));

        raw[125] ^= 0x01;
        assert_eq!(verify_legacy_crc(&raw), Ok(false));
        assert!(verify_legacy_crc(&raw[..127]).is_err());
    }

    #[test]
    fn converts_nanoseconds_cycles_and_data_rate() {
        assert_eq!(parse_ns_to_ps(Some("0.625")), Some(625));
        assert_eq!(parse_ns_to_ps(Some("13.75")), Some(13_750));
        assert_eq!(parse_ns_to_ps(Some("bad")), None);
        assert_eq!(parse_ns_to_ps(Some("0")), None);
        assert_eq!(ceil_cycles(13_750, 625), Some(22));
        assert_eq!(ceil_cycles(13_751, 625), Some(23));
        assert_eq!(data_rate_from_tck(625), Some(3200));
        assert_eq!(data_rate_from_tck(750), Some(2667));
        assert_eq!(parse_cas_list(Some("24, 22, 24")), vec![22, 24]);
    }

    #[test]
    fn decodes_ddr4_jedec_core_timings() {
        let module = legacy_module(
            "DDR4",
            &[
                ("MinimumCycleTime", "0.625"),
                ("MinimumCASLatencyTime", "13.75"),
                ("MinimumRASToCASDelayTime", "13.75"),
                ("MinimumRowPrechargeDelayTime", "13.75"),
                ("MinimumActiveToPrechargeDelayTime", "32"),
                ("MinimumActiveToActiveRefreshDelayTime", "45.75"),
                ("MinimumWriteRecoveryTime", "15"),
                ("MinimumRefreshRecoveryDelayTime1", "350"),
                ("MinimumActivateToActivateDelay_DiffGroup", "3.75"),
                ("MinimumActivateToActivateDelay_SameGroup", "6.25"),
                ("MinimumCASToCASDelay_SameGroup", "6.25"),
                ("MinimumWriteToReadTime_DiffGroup", "2.5"),
                ("MinimumWriteToReadTime_SameGroup", "7.5"),
                ("MinimumFourActivateWindowTime", "30"),
                ("CASLatenciesSupported", "22, 24"),
            ],
        );

        let profile = decode_legacy_jedec_profile(&module, Some(true));
        assert_eq!(profile.core_timings, "22-22-22-52");
        assert_eq!(profile.data_rate_mt_s, Some(3200));
        assert!(profile
            .rows
            .iter()
            .any(|row| row.parameter == "tRFC1" && row.time == "350 ns"));
    }

    #[test]
    fn missing_or_unsupported_cl_is_not_fabricated() {
        let missing = legacy_module(
            "DDR4",
            &[
                ("MinimumCycleTime", "0.625"),
                ("CASLatenciesSupported", "22, 24"),
            ],
        );
        assert_eq!(
            decode_legacy_jedec_profile(&missing, Some(true)).core_timings,
            UNAVAILABLE
        );

        let unsupported = legacy_module(
            "DDR4",
            &[
                ("MinimumCycleTime", "0.625"),
                ("MinimumCASLatencyTime", "14"),
                ("CASLatenciesSupported", "22, 24"),
            ],
        );
        let profile = decode_legacy_jedec_profile(&unsupported, Some(true));
        assert_eq!(profile.core_timings, UNAVAILABLE);
        assert!(profile.subtitle.contains("CL 不在 CAS 支持列表"));
    }
}
