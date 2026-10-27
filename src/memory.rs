use std::hint::black_box;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use windows::core::{Error, BSTR, HRESULT, PCWSTR};
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, RPC_E_TOO_LATE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CoUninitialize,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_DEFAULT,
    RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Variant::{
    VariantClear, VARIANT, VT_BSTR, VT_EMPTY, VT_I1, VT_I2, VT_I4, VT_I8, VT_NULL, VT_UI1, VT_UI2,
    VT_UI4, VT_UI8,
};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator,
    WBEM_E_NOT_FOUND, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE,
    WBEM_S_FALSE,
};

#[derive(Clone, Debug)]
pub struct MemoryModule {
    pub device_locator: String,
    pub bank_label: String,
    pub capacity: u64,
    pub speed_mt_s: Option<u32>,
    pub configured_speed_mt_s: Option<u32>,
    pub manufacturer: String,
    pub part_number: String,
    pub serial_number: String,
    pub memory_type: Option<String>,
    pub form_factor: Option<String>,
    pub voltage_mv: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct MemoryHardware {
    pub modules: Vec<MemoryModule>,
    pub total_slots: Option<usize>,
    pub installed_bytes: u64,
    pub max_capacity_bytes: Option<u64>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkStage {
    Preparing,
    Read,
    Write,
    Copy,
    Latency,
}

#[derive(Clone, Copy, Debug)]
pub struct BenchmarkProgress {
    pub stage: BenchmarkStage,
    pub completed: u32,
    pub total: u32,
}

#[derive(Clone, Debug)]
pub struct BenchmarkResult {
    pub buffer_bytes: u64,
    pub read_gib_s: f64,
    pub write_gib_s: f64,
    pub copy_gib_s: f64,
    pub latency_ns: f64,
    pub samples: u32,
    pub elapsed_seconds: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BenchmarkError {
    Cancelled,
    Failed(String),
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;
const MIN_AVAILABLE_BYTES: u64 = 256 * MIB;
const MIN_BUFFER_BYTES: u64 = 32 * MIB;
const MAX_BUFFER_BYTES: u64 = 256 * MIB;
const LATENCY_MIN_BYTES: u64 = 32 * MIB;
const LATENCY_MAX_BYTES: u64 = 64 * MIB;
const LATENCY_ROUNDS: u32 = 5;
const LATENCY_STEPS: usize = 1 << 20;
const SAMPLE_ROUNDS: u32 = 5;
const CALIBRATION_ATTEMPTS: u32 = 6;
const RPC_C_AUTHN_WINNT_VALUE: u32 = 10;
const RPC_C_AUTHZ_NONE_VALUE: u32 = 0;

pub fn collect_hardware() -> Result<MemoryHardware, String> {
    let _apartment = ComApartment::initialize()?;
    unsafe { collect_hardware_in_apartment() }
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self, String> {
        let status = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if status == RPC_E_CHANGED_MODE {
            return Err(hresult_error(
                "初始化 COM（线程已使用不兼容的单元模型）",
                status,
            ));
        }
        if status.is_err() {
            return Err(hresult_error("初始化 COM (CoInitializeEx)", status));
        }
        let apartment = Self;

        let security = unsafe {
            CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            )
        };
        if let Err(error) = security {
            if error.code() != RPC_E_TOO_LATE {
                return Err(wmi_error("初始化 COM 安全性 (CoInitializeSecurity)", error));
            }
        }
        Ok(apartment)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct WmiVariant(VARIANT);

impl Default for WmiVariant {
    fn default() -> Self {
        Self(VARIANT::default())
    }
}

impl Drop for WmiVariant {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

unsafe fn collect_hardware_in_apartment() -> Result<MemoryHardware, String> {
    let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
        .map_err(|error| wmi_error("创建 IWbemLocator (CoCreateInstance)", error))?;
    let empty = BSTR::new();
    let namespace = BSTR::from("ROOT\\cimv2");
    let services = locator
        .ConnectServer(&namespace, &empty, &empty, &empty, 0, &empty, None)
        .map_err(|error| wmi_error("连接 WMI 命名空间 root/cimv2 (ConnectServer)", error))?;

    CoSetProxyBlanket(
        &services,
        RPC_C_AUTHN_WINNT_VALUE,
        RPC_C_AUTHZ_NONE_VALUE,
        PCWSTR::null(),
        RPC_C_AUTHN_LEVEL_DEFAULT,
        RPC_C_IMP_LEVEL_IMPERSONATE,
        None,
        EOAC_NONE,
    )
    .map_err(|error| wmi_error("设置 WMI 代理安全性 (CoSetProxyBlanket)", error))?;

    let modules = collect_modules(&services)?;
    let installed_bytes = modules
        .iter()
        .try_fold(0u64, |total, module| total.checked_add(module.capacity));
    let installed_bytes = installed_bytes.ok_or_else(|| {
        "WMI 汇总 Win32_PhysicalMemory.Capacity：模块容量总和溢出 u64".to_string()
    })?;

    let mut warnings = Vec::new();
    let array_info = match collect_array_info(&services) {
        Ok(info) => info,
        Err(error) => {
            warnings.push(format!("物理内存数组信息不可用：{error}"));
            ArrayInfo {
                total_slots: None,
                max_capacity_bytes: None,
            }
        }
    };

    Ok(MemoryHardware {
        modules,
        total_slots: array_info.total_slots,
        installed_bytes,
        max_capacity_bytes: array_info.max_capacity_bytes,
        warnings,
    })
}

unsafe fn query_objects(
    services: &IWbemServices,
    class_name: &str,
) -> Result<Vec<IWbemClassObject>, String> {
    let language = BSTR::from("WQL");
    let query = BSTR::from(format!("SELECT * FROM {class_name}"));
    let enumerator: IEnumWbemClassObject = services
        .ExecQuery(
            &language,
            &query,
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )
        .map_err(|error| wmi_error(&format!("查询 {class_name} (ExecQuery)"), error))?;

    let mut objects = Vec::new();
    loop {
        let mut object = None;
        let mut returned = 0u32;
        let status = enumerator.Next(
            WBEM_INFINITE,
            std::slice::from_mut(&mut object),
            &mut returned,
        );
        if status.is_err() {
            return Err(hresult_error(
                &format!("枚举 {class_name} (IEnumWbemClassObject::Next)"),
                status,
            ));
        }
        if returned > 0 {
            if let Some(object) = object {
                objects.push(object);
            }
        }
        if status.0 == WBEM_S_FALSE.0 || returned == 0 {
            break;
        }
    }
    Ok(objects)
}

unsafe fn collect_modules(services: &IWbemServices) -> Result<Vec<MemoryModule>, String> {
    let objects = query_objects(services, "Win32_PhysicalMemory")?;
    let mut modules = Vec::with_capacity(objects.len());
    for object in objects {
        let capacity = optional_u64(&object, "Capacity")?
            .filter(|&value| value > 0)
            .ok_or_else(|| {
                "WMI 解析 Win32_PhysicalMemory.Capacity：必需容量为 NULL、0 或格式不受支持"
                    .to_string()
            })?;
        let smbios_type = optional_u32(&object, "SMBIOSMemoryType")?;
        let legacy_type = if smbios_type.is_none() {
            optional_u32(&object, "MemoryType")?
        } else {
            None
        };

        modules.push(MemoryModule {
            device_locator: optional_string(&object, "DeviceLocator")?.unwrap_or_default(),
            bank_label: optional_string(&object, "BankLabel")?.unwrap_or_default(),
            capacity,
            speed_mt_s: non_zero(optional_u32(&object, "Speed")?),
            configured_speed_mt_s: non_zero(optional_u32(&object, "ConfiguredClockSpeed")?),
            manufacturer: optional_string(&object, "Manufacturer")?.unwrap_or_default(),
            part_number: optional_string(&object, "PartNumber")?.unwrap_or_default(),
            serial_number: optional_string(&object, "SerialNumber")?.unwrap_or_default(),
            memory_type: smbios_type
                .or(legacy_type)
                .map(|value| memory_type_name(value).to_string()),
            form_factor: optional_u32(&object, "FormFactor")?
                .map(|value| form_factor_name(value).to_string()),
            voltage_mv: non_zero(optional_u32(&object, "ConfiguredVoltage")?),
        });
    }
    Ok(modules)
}

struct ArrayInfo {
    total_slots: Option<usize>,
    max_capacity_bytes: Option<u64>,
}

unsafe fn collect_array_info(services: &IWbemServices) -> Result<ArrayInfo, String> {
    let objects = query_objects(services, "Win32_PhysicalMemoryArray")?;
    let mut found_memory_array = false;
    let mut slots_complete = true;
    let mut capacity_complete = true;
    let mut total_slots = 0usize;
    let mut max_capacity_bytes = 0u64;

    for object in objects {
        if optional_u32(&object, "Use")? != Some(3) {
            continue;
        }
        found_memory_array = true;

        match optional_u32(&object, "MemoryDevices")?.filter(|&value| value > 0) {
            Some(devices) => {
                total_slots = total_slots.checked_add(devices as usize).ok_or_else(|| {
                    "WMI 汇总 Win32_PhysicalMemoryArray.MemoryDevices：槽位数溢出 usize".to_string()
                })?;
            }
            None => slots_complete = false,
        }

        let extended = optional_u64(&object, "MaxCapacityEx")?.filter(|&value| value > 0);
        let capacity = if extended.is_some() {
            extended
        } else {
            match optional_u64(&object, "MaxCapacity")?.filter(|&value| value > 0) {
                Some(kib) => Some(kib.checked_mul(1024).ok_or_else(|| {
                    "WMI 转换 Win32_PhysicalMemoryArray.MaxCapacity：KB 转字节溢出 u64".to_string()
                })?),
                None => None,
            }
        };
        match capacity {
            Some(bytes) => {
                max_capacity_bytes = max_capacity_bytes.checked_add(bytes).ok_or_else(|| {
                    "WMI 汇总 Win32_PhysicalMemoryArray 最大容量：字节数溢出 u64".to_string()
                })?;
            }
            None => capacity_complete = false,
        }
    }

    Ok(ArrayInfo {
        total_slots: (found_memory_array && slots_complete).then_some(total_slots),
        max_capacity_bytes: (found_memory_array && capacity_complete).then_some(max_capacity_bytes),
    })
}

unsafe fn property(object: &IWbemClassObject, name: &str) -> Result<Option<WmiVariant>, String> {
    let wide_name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut value = WmiVariant::default();
    match object.Get(PCWSTR(wide_name.as_ptr()), 0, &mut value.0, None, None) {
        Ok(()) => Ok(Some(value)),
        Err(error) if error.code().0 == WBEM_E_NOT_FOUND.0 => Ok(None),
        Err(error) => Err(wmi_error(
            &format!("读取属性 {name} (IWbemClassObject::Get)"),
            error,
        )),
    }
}

unsafe fn optional_string(object: &IWbemClassObject, name: &str) -> Result<Option<String>, String> {
    Ok(property(object, name)?.and_then(|value| unsafe { variant_string(&value.0) }))
}

unsafe fn optional_u64(object: &IWbemClassObject, name: &str) -> Result<Option<u64>, String> {
    Ok(property(object, name)?.and_then(|value| unsafe { variant_u64(&value.0) }))
}

unsafe fn optional_u32(object: &IWbemClassObject, name: &str) -> Result<Option<u32>, String> {
    Ok(optional_u64(object, name)?.and_then(|value| u32::try_from(value).ok()))
}

unsafe fn variant_string(value: &VARIANT) -> Option<String> {
    let data = &value.Anonymous.Anonymous;
    if data.vt.0 != VT_BSTR.0 {
        return None;
    }
    let text = data.Anonymous.bstrVal.to_string();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

unsafe fn variant_u64(value: &VARIANT) -> Option<u64> {
    let data = &value.Anonymous.Anonymous;
    if data.vt.0 == VT_EMPTY.0 || data.vt.0 == VT_NULL.0 {
        return None;
    }
    let number = &data.Anonymous;
    match data.vt.0 {
        value if value == VT_UI1.0 => Some(number.bVal as u64),
        value if value == VT_UI2.0 => Some(number.uiVal as u64),
        value if value == VT_UI4.0 => Some(number.ulVal as u64),
        value if value == VT_UI8.0 => Some(number.ullVal),
        value if value == VT_I1.0 => u64::try_from(number.cVal).ok(),
        value if value == VT_I2.0 => u64::try_from(number.iVal).ok(),
        value if value == VT_I4.0 => u64::try_from(number.lVal).ok(),
        value if value == VT_I8.0 => u64::try_from(number.llVal).ok(),
        value if value == VT_BSTR.0 => number.bstrVal.to_string().trim().parse().ok(),
        _ => None,
    }
}

fn non_zero(value: Option<u32>) -> Option<u32> {
    value.filter(|&value| value != 0)
}

fn memory_type_name(value: u32) -> String {
    match value {
        1 => "Other".to_string(),
        2 => "DRAM".to_string(),
        3 => "Synchronous DRAM".to_string(),
        4 => "Cache DRAM".to_string(),
        5 => "EDO".to_string(),
        6 => "EDRAM".to_string(),
        7 => "VRAM".to_string(),
        8 => "SRAM".to_string(),
        9 => "RAM".to_string(),
        10 => "ROM".to_string(),
        11 => "Flash".to_string(),
        12 => "EEPROM".to_string(),
        13 => "FEPROM".to_string(),
        14 => "EPROM".to_string(),
        15 => "CDRAM".to_string(),
        16 => "3DRAM".to_string(),
        17 => "SDRAM".to_string(),
        18 => "SGRAM".to_string(),
        19 => "RDRAM".to_string(),
        20 => "DDR".to_string(),
        21 => "DDR2".to_string(),
        22 => "DDR2 FB-DIMM".to_string(),
        24 => "DDR3".to_string(),
        25 => "FBD2".to_string(),
        26 => "DDR4".to_string(),
        27 => "LPDDR".to_string(),
        28 => "LPDDR2".to_string(),
        29 => "LPDDR3".to_string(),
        30 => "LPDDR4".to_string(),
        31 => "Logical Non-Volatile".to_string(),
        32 => "HBM".to_string(),
        33 => "HBM2".to_string(),
        34 => "DDR5".to_string(),
        35 => "LPDDR5".to_string(),
        36 => "HBM2E".to_string(),
        other => other.to_string(),
    }
}

fn form_factor_name(value: u32) -> String {
    match value {
        1 => "Other".to_string(),
        2 => "SIP".to_string(),
        3 => "DIP".to_string(),
        4 => "ZIP".to_string(),
        5 => "SOJ".to_string(),
        6 => "Proprietary".to_string(),
        7 => "SIMM".to_string(),
        8 => "DIMM".to_string(),
        9 => "TSOP".to_string(),
        10 => "PGA".to_string(),
        11 => "RIMM".to_string(),
        12 => "SO-DIMM".to_string(),
        13 => "SRIMM".to_string(),
        14 => "SMD".to_string(),
        15 => "SSMP".to_string(),
        16 => "QFP".to_string(),
        17 => "TQFP".to_string(),
        18 => "SOIC".to_string(),
        19 => "LCC".to_string(),
        20 => "PLCC".to_string(),
        21 => "BGA".to_string(),
        22 => "FPBGA".to_string(),
        23 => "LGA".to_string(),
        24 => "FB-DIMM".to_string(),
        other => other.to_string(),
    }
}

fn hresult_error(stage: &str, status: HRESULT) -> String {
    wmi_error(stage, Error::from_hresult(status))
}

fn wmi_error(stage: &str, error: Error) -> String {
    let status = error.code();
    let message = error.message();
    if message.is_empty() {
        format!("WMI 阶段“{stage}”失败 (HRESULT 0x{:08X})", status.0 as u32)
    } else {
        format!(
            "WMI 阶段“{stage}”失败：{message} (HRESULT 0x{:08X})",
            status.0 as u32
        )
    }
}

pub fn run_benchmark(
    available_bytes: u64,
    cancel: Arc<AtomicBool>,
    progress: impl FnMut(BenchmarkProgress),
) -> Result<BenchmarkResult, BenchmarkError> {
    let buffer_bytes = benchmark_buffer_bytes(available_bytes).ok_or_else(|| {
        BenchmarkError::Failed(format!(
            "可用内存不足：需要至少 {MIN_AVAILABLE_BYTES} 字节（256 MiB），实际为 {available_bytes} 字节"
        ))
    })?;
    if cancel.load(Ordering::Relaxed) {
        return Err(BenchmarkError::Cancelled);
    }

    let words = (buffer_bytes / std::mem::size_of::<u64>() as u64) as usize;
    let mut source = Vec::new();
    let mut destination = Vec::new();
    source.try_reserve_exact(words).map_err(|error| {
        BenchmarkError::Failed(format!(
            "为读/复制源缓冲区预留 {buffer_bytes} 字节失败：{error}"
        ))
    })?;
    destination.try_reserve_exact(words).map_err(|error| {
        BenchmarkError::Failed(format!(
            "为写/复制目标缓冲区预留 {buffer_bytes} 字节失败：{error}"
        ))
    })?;
    source.resize(words, 0);
    destination.resize(words, 0);
    for (index, word) in source.iter_mut().enumerate() {
        *word = 0x9E37_79B9_7F4A_7C15u64 ^ (index as u64).wrapping_mul(0x1000_0000_01B3);
    }
    for (index, word) in destination.iter_mut().enumerate() {
        *word = 0xD1B5_4A32_D192_ED03u64 ^ (index as u64).wrapping_mul(0x9E37_79B1);
    }

    let started = Instant::now();
    let mut reporter = ProgressReporter::new(progress);
    reporter.emit(BenchmarkStage::Preparing, 0, 3, true);
    let mut checksum = 0u64;
    let mut repetitions = [0u32; 3];
    for (index, operation) in [Operation::Read, Operation::Write, Operation::Copy]
        .into_iter()
        .enumerate()
    {
        repetitions[index] = calibrate(
            operation,
            &source,
            &mut destination,
            cancel.as_ref(),
            &mut checksum,
        )?;
        reporter.emit(BenchmarkStage::Preparing, index as u32 + 1, 3, true);
    }

    let read_gib_s = sample_stage(
        Operation::Read,
        BenchmarkStage::Read,
        buffer_bytes,
        repetitions[0],
        &source,
        &mut destination,
        cancel.as_ref(),
        &mut checksum,
        &mut reporter,
    )?;
    let write_gib_s = sample_stage(
        Operation::Write,
        BenchmarkStage::Write,
        buffer_bytes,
        repetitions[1],
        &source,
        &mut destination,
        cancel.as_ref(),
        &mut checksum,
        &mut reporter,
    )?;
    let copy_gib_s = sample_stage(
        Operation::Copy,
        BenchmarkStage::Copy,
        buffer_bytes,
        repetitions[2],
        &source,
        &mut destination,
        cancel.as_ref(),
        &mut checksum,
        &mut reporter,
    )?;
    black_box(checksum);
    if cancel.load(Ordering::Relaxed) {
        return Err(BenchmarkError::Cancelled);
    }

    drop(source);
    drop(destination);
    let latency_ns = measure_latency(buffer_bytes, cancel.as_ref(), &mut reporter)?;

    Ok(BenchmarkResult {
        buffer_bytes,
        read_gib_s,
        write_gib_s,
        copy_gib_s,
        latency_ns,
        samples: SAMPLE_ROUNDS,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    })
}

fn measure_latency<F: FnMut(BenchmarkProgress)>(
    buffer_bytes: u64,
    cancel: &AtomicBool,
    reporter: &mut ProgressReporter<F>,
) -> Result<f64, BenchmarkError> {
    let target_bytes = (buffer_bytes * 2).clamp(LATENCY_MIN_BYTES, LATENCY_MAX_BYTES);
    let target_entries = (target_bytes / std::mem::size_of::<u32>() as u64) as usize;
    let entries = target_entries.next_power_of_two();
    if entries > u32::MAX as usize {
        return Err(BenchmarkError::Failed(
            "内存延迟测试的索引长度溢出".to_owned(),
        ));
    }

    let mut next = Vec::new();
    next.try_reserve_exact(entries).map_err(|error| {
        BenchmarkError::Failed(format!(
            "为内存延迟测试预留 {} 项索引失败：{error}",
            entries
        ))
    })?;
    next.resize(entries, 0u32);

    let mask = (entries - 1) as u64;
    for (index, slot) in next.iter_mut().enumerate() {
        *slot = ((index as u64)
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223)
            & mask) as u32;
    }

    let mut warmup_index = 0usize;
    for _ in 0..(LATENCY_STEPS / 4) {
        warmup_index = next[warmup_index] as usize;
    }
    black_box(warmup_index);

    let mut total_ns = 0.0;
    for round in 0..LATENCY_ROUNDS {
        if cancel.load(Ordering::Relaxed) {
            return Err(BenchmarkError::Cancelled);
        }
        let started = Instant::now();
        let mut index = (round as usize * 17) % entries;
        let mut checksum = 0u64;
        for _ in 0..LATENCY_STEPS {
            index = next[index] as usize;
            checksum = checksum.wrapping_add(index as u64);
        }
        black_box(checksum);
        total_ns += started.elapsed().as_secs_f64() * 1e9 / LATENCY_STEPS as f64;
        reporter.emit(BenchmarkStage::Latency, round + 1, LATENCY_ROUNDS, true);
    }

    Ok(total_ns / f64::from(LATENCY_ROUNDS))
}

fn benchmark_buffer_bytes(available_bytes: u64) -> Option<u64> {
    if available_bytes < MIN_AVAILABLE_BYTES {
        return None;
    }
    let target = (available_bytes / 8).min(MAX_BUFFER_BYTES);
    let floor_power_of_two = 1u64 << (u64::BITS - 1 - target.leading_zeros());
    Some(floor_power_of_two.clamp(MIN_BUFFER_BYTES, MAX_BUFFER_BYTES))
}

#[derive(Clone, Copy)]
enum Operation {
    Read,
    Write,
    Copy,
}

struct ProgressReporter<F> {
    callback: F,
    last_sent: Option<Instant>,
}

impl<F: FnMut(BenchmarkProgress)> ProgressReporter<F> {
    fn new(callback: F) -> Self {
        Self {
            callback,
            last_sent: None,
        }
    }

    fn emit(&mut self, stage: BenchmarkStage, completed: u32, total: u32, force: bool) {
        if !force
            && self
                .last_sent
                .is_some_and(|last| last.elapsed() < Duration::from_millis(100))
        {
            return;
        }
        (self.callback)(BenchmarkProgress {
            stage,
            completed,
            total,
        });
        self.last_sent = Some(Instant::now());
    }
}

fn calibrate(
    operation: Operation,
    source: &[u64],
    destination: &mut [u64],
    cancel: &AtomicBool,
    checksum: &mut u64,
) -> Result<u32, BenchmarkError> {
    let _ = measure_operation(operation, source, destination, 1, cancel, checksum)?;
    let mut repetitions = 1u32;
    let mut best_repetitions = repetitions;
    let mut best_delta = f64::INFINITY;

    for _ in 0..CALIBRATION_ATTEMPTS {
        let elapsed = measure_operation(
            operation,
            source,
            destination,
            repetitions,
            cancel,
            checksum,
        )?;
        let milliseconds = elapsed.as_secs_f64().max(1e-9) * 1000.0;
        let delta = (milliseconds - 200.0).abs();
        if delta < best_delta {
            best_delta = delta;
            best_repetitions = repetitions;
        }
        if (150.0..=250.0).contains(&milliseconds) || (milliseconds > 250.0 && repetitions == 1) {
            return Ok(repetitions);
        }

        let scaled = (repetitions as f64 * 200.0 / milliseconds).ceil();
        let mut next = if scaled >= u32::MAX as f64 {
            u32::MAX
        } else {
            scaled.max(1.0) as u32
        };
        if next == repetitions {
            if milliseconds < 150.0 {
                next = next.saturating_add(1);
            } else if repetitions > 1 {
                next -= 1;
            }
        }
        repetitions = next;
    }
    Ok(best_repetitions)
}

fn sample_stage<F: FnMut(BenchmarkProgress)>(
    operation: Operation,
    stage: BenchmarkStage,
    buffer_bytes: u64,
    repetitions: u32,
    source: &[u64],
    destination: &mut [u64],
    cancel: &AtomicBool,
    checksum: &mut u64,
    reporter: &mut ProgressReporter<F>,
) -> Result<f64, BenchmarkError> {
    let traffic_multiplier = match operation {
        Operation::Copy => 2,
        Operation::Read | Operation::Write => 1,
    };
    let bytes_per_round = buffer_bytes
        .checked_mul(repetitions as u64)
        .and_then(|bytes| bytes.checked_mul(traffic_multiplier))
        .ok_or_else(|| BenchmarkError::Failed("测速单轮字节数溢出 u64".to_string()))?;
    let mut rates = Vec::with_capacity(SAMPLE_ROUNDS as usize);
    reporter.emit(stage, 0, SAMPLE_ROUNDS, true);

    for round in 0..SAMPLE_ROUNDS {
        let elapsed = measure_operation(
            operation,
            source,
            destination,
            repetitions,
            cancel,
            checksum,
        )?;
        let seconds = elapsed.as_secs_f64();
        if seconds == 0.0 {
            return Err(BenchmarkError::Failed(
                "测速计时器返回了零时长，无法计算带宽".to_string(),
            ));
        }
        rates.push(bytes_per_round as f64 / seconds / GIB as f64);
        reporter.emit(stage, round + 1, SAMPLE_ROUNDS, round + 1 == SAMPLE_ROUNDS);
    }
    Ok(median(&mut rates))
}

fn measure_operation(
    operation: Operation,
    source: &[u64],
    destination: &mut [u64],
    repetitions: u32,
    cancel: &AtomicBool,
    checksum: &mut u64,
) -> Result<Duration, BenchmarkError> {
    let started = Instant::now();
    for pass in 0..repetitions {
        if cancel.load(Ordering::Relaxed) {
            return Err(BenchmarkError::Cancelled);
        }
        match operation {
            Operation::Read => {
                *checksum = read_pass(source, *checksum ^ pass as u64);
            }
            Operation::Write => {
                let pattern = *checksum ^ (pass as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                write_pass(destination, pattern);
                *checksum = sample_checksum(destination);
            }
            Operation::Copy => {
                copy_pass(source, destination);
                *checksum = sample_checksum(destination) ^ *checksum;
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(BenchmarkError::Cancelled);
    }
    let elapsed = started.elapsed();
    black_box(*checksum);
    Ok(elapsed)
}

fn read_pass(buffer: &[u64], seed: u64) -> u64 {
    let buffer = black_box(buffer);
    let mut sum = black_box(seed);
    for &word in buffer {
        sum = sum.wrapping_add(word);
    }
    black_box(sum)
}

fn write_pass(buffer: &mut [u64], pattern: u64) {
    let buffer = black_box(buffer);
    for word in buffer.iter_mut() {
        *word = pattern;
    }
    black_box(&*buffer);
}

fn copy_pass(source: &[u64], destination: &mut [u64]) {
    black_box(&mut *destination).copy_from_slice(black_box(source));
    black_box(destination);
}

fn sample_checksum(buffer: &[u64]) -> u64 {
    black_box(buffer[0] ^ buffer[buffer.len() / 2] ^ buffer[buffer.len() - 1])
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}
