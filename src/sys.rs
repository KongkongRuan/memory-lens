//! 通过 Windows 原生 API 采集内存数据。
//!
//! - `GetPerformanceInfo`：物理总量、提交量、内核池、系统缓存
//! - `NtQuerySystemInformation(SystemMemoryListInformation)`：RAMMap 同款页列表明细（需管理员）
//! - `NtQuerySystemInformation(SystemProcessInformation)`：枚举进程，提取 "Memory Compression" 压缩存储
//! - PDH：分页池**常驻**物理内存的大小（GetPerformanceInfo 给的是含已换出的总量）

use std::ffi::c_void;

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, POINT};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_LARGE,
};
use windows::Win32::System::ProcessStatus::{
    GetModuleFileNameExW, GetPerformanceInfo, PERFORMANCE_INFORMATION,
};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetCursorPos, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, WindowFromPoint, GA_ROOT,
};

const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
const SYSTEM_MEMORY_LIST_INFORMATION_CLASS: u32 = 80;
const PROCESS_COMMAND_LINE_INFORMATION_CLASS: u32 = 60;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;

type NtQuerySystemInformationFn =
    unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> i32;
type NtQueryInformationProcessFn =
    unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> i32;

#[repr(C)]
struct UnicodeString {
    length: u16,
    _maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct SystemProcessInformation {
    next_entry_offset: u32,
    _number_of_threads: u32,
    _working_set_private_size: i64,
    _hard_fault_count: u32,
    _number_of_threads_high_watermark: u32,
    _cycle_time: u64,
    _create_time: i64,
    _user_time: i64,
    _kernel_time: i64,
    image_name: UnicodeString,
    _base_priority: i32,
    unique_process_id: *mut c_void,
    inherited_from_unique_process_id: *mut c_void,
    _handle_count: u32,
    _session_id: u32,
    _unique_process_key: *mut c_void,
    _peak_virtual_size: usize,
    _virtual_size: usize,
    _page_fault_count: u32,
    _peak_working_set_size: usize,
    working_set_size: usize,
}

#[repr(C)]
#[derive(Default)]
struct MemoryListInformation {
    zero_page_count: usize,
    free_page_count: usize,
    modified_page_count: usize,
    modified_no_write_page_count: usize,
    bad_page_count: usize,
    page_count_by_priority: [usize; 8],
    _repurposed_pages_by_priority: [usize; 8],
    _modified_page_count_page_file: usize,
    _modified_no_write_page_count_page_file: usize,
}

/// RAMMap 风格的物理页列表（字节）。
pub struct PageLists {
    pub zero: u64,
    pub free: u64,
    pub standby: u64,
    pub modified: u64,
    pub modified_no_write: u64,
    pub bad: u64,
}

/// 单个进程信息。
#[derive(Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// 工作集 = 进程当前驻留的物理内存（含与其他进程共享的页面）。
    pub working_set: u64,
    /// 完整命令行（受保护进程可能取不到）。
    pub cmdline: String,
}

/// 一次完整的内存快照。
pub struct Snapshot {
    pub physical_total: u64,
    /// GetPerformanceInfo 的"可用"（降级模式下使用）。
    pub physical_available: u64,
    pub commit_total: u64,
    pub commit_limit: u64,
    pub nonpaged_pool: u64,
    /// 分页池**常驻**物理内存的部分（PDH 计数器）。
    pub paged_pool_resident: u64,
    /// 系统缓存工作集。
    pub system_cache: u64,
    /// "Memory Compression" 进程工作集 = 压缩存储占用的物理内存。
    pub compression: u64,
    pub compression_found: bool,
    /// 所有进程（含 System），按工作集降序。
    pub processes: Vec<ProcessInfo>,
    pub page_lists: Option<PageLists>,
}

impl Snapshot {
    fn empty() -> Self {
        Snapshot {
            physical_total: 0,
            physical_available: 0,
            commit_total: 0,
            commit_limit: 0,
            nonpaged_pool: 0,
            paged_pool_resident: 0,
            system_cache: 0,
            compression: 0,
            compression_found: false,
            processes: Vec::new(),
            page_lists: None,
        }
    }
}

pub fn collect() -> Snapshot {
    let mut snap = Snapshot::empty();

    let mut page_size = 4096u64;
    unsafe {
        let mut pi = PERFORMANCE_INFORMATION {
            cb: std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32,
            ..Default::default()
        };
        if GetPerformanceInfo(&mut pi, pi.cb).is_ok() {
            page_size = pi.PageSize.max(1) as u64;
            let pages = |n: usize| n as u64 * page_size;
            snap.physical_total = pages(pi.PhysicalTotal);
            snap.physical_available = pages(pi.PhysicalAvailable);
            snap.commit_total = pages(pi.CommitTotal);
            snap.commit_limit = pages(pi.CommitLimit);
            snap.nonpaged_pool = pages(pi.KernelNonpaged);
            snap.system_cache = pages(pi.SystemCache);
        }
    }

    snap.paged_pool_resident = paged_pool_resident().unwrap_or(0);

    if let Some(nt_query) = nt_query_fn() {
        snap.page_lists = unsafe { query_memory_list(nt_query, page_size) };
        if let Some((compression, found, processes)) = unsafe { query_processes(nt_query) } {
            snap.compression = compression;
            snap.compression_found = found;
            snap.processes = processes;
        }
    }

    snap
}

fn nt_query_fn() -> Option<NtQuerySystemInformationFn> {
    unsafe {
        let ntdll = GetModuleHandleA(s!("ntdll.dll")).ok()?;
        let proc = GetProcAddress(ntdll, s!("NtQuerySystemInformation"))?;
        Some(std::mem::transmute(proc))
    }
}

unsafe fn query_memory_list(f: NtQuerySystemInformationFn, page_size: u64) -> Option<PageLists> {
    let mut raw = MemoryListInformation::default();
    let mut ret_len = 0u32;
    let status = f(
        SYSTEM_MEMORY_LIST_INFORMATION_CLASS,
        &mut raw as *mut _ as *mut c_void,
        std::mem::size_of::<MemoryListInformation>() as u32,
        &mut ret_len,
    );
    if status != 0 {
        return None;
    }
    let pages = |n: usize| n as u64 * page_size;
    Some(PageLists {
        zero: pages(raw.zero_page_count),
        free: pages(raw.free_page_count),
        standby: raw.page_count_by_priority.iter().map(|&c| pages(c)).sum(),
        modified: pages(raw.modified_page_count),
        modified_no_write: pages(raw.modified_no_write_page_count),
        bad: pages(raw.bad_page_count),
    })
}

/// 返回 (压缩存储字节数, 是否找到压缩进程, 进程列表)。
unsafe fn query_processes(f: NtQuerySystemInformationFn) -> Option<(u64, bool, Vec<ProcessInfo>)> {
    let mut words = 1usize << 17; // 1 MB
    loop {
        // 用 u64 数组保证 8 字节对齐。
        let mut buf = vec![0u64; words];
        let mut ret_len = 0u32;
        let status = f(
            SYSTEM_PROCESS_INFORMATION_CLASS,
            buf.as_mut_ptr() as *mut c_void,
            (words * 8) as u32,
            &mut ret_len,
        );
        if status == STATUS_INFO_LENGTH_MISMATCH && words < (1 << 23) {
            words *= 2;
            continue;
        }
        if status != 0 {
            return None;
        }

        let mut compression = 0u64;
        let mut found = false;
        let mut processes: Vec<ProcessInfo> = Vec::new();
        let mut offset = 0usize;
        loop {
            let info = &*(buf.as_ptr().add(offset / 8) as *const SystemProcessInformation);
            let ws = info.working_set_size as u64;
            let pid = info.unique_process_id as usize;

            let name = if !info.image_name.buffer.is_null() && info.image_name.length > 0 {
                let slice = std::slice::from_raw_parts(
                    info.image_name.buffer,
                    (info.image_name.length / 2) as usize,
                );
                String::from_utf16_lossy(slice)
            } else {
                String::new()
            };

            if name.eq_ignore_ascii_case("Memory Compression") {
                compression = ws;
                found = true;
            }

            if pid != 0 {
                processes.push(ProcessInfo {
                    pid: pid as u32,
                    ppid: info.inherited_from_unique_process_id as usize as u32,
                    name: if name.is_empty() { format!("PID {pid}") } else { name },
                    working_set: ws,
                    cmdline: String::new(),
                });
            }
            if info.next_entry_offset == 0 {
                break;
            }
            offset += info.next_entry_offset as usize;
        }
        processes.sort_by(|a, b| b.working_set.cmp(&a.working_set));
        fill_command_lines(&mut processes);
        return Some((compression, found, processes));
    }
}

/// 逐个进程读取完整命令行（失败/受保护进程留空）。
fn fill_command_lines(processes: &mut [ProcessInfo]) {
    let Some(nt_query_process) = nt_query_process_fn() else { return };
    for p in processes.iter_mut() {
        if let Some(cmd) = read_command_line(nt_query_process, p.pid) {
            p.cmdline = cmd;
        }
    }
}

fn nt_query_process_fn() -> Option<NtQueryInformationProcessFn> {
    unsafe {
        let ntdll = GetModuleHandleA(s!("ntdll.dll")).ok()?;
        let proc = GetProcAddress(ntdll, s!("NtQueryInformationProcess"))?;
        Some(std::mem::transmute(proc))
    }
}

fn read_command_line(f: NtQueryInformationProcessFn, pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        // 分配足够大的缓冲：UNICODE_STRING 头 + 命令行内容
        let mut buf = vec![0u16; 4096];
        let mut ret_len = 0u32;
        let status = f(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION_CLASS,
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * 2) as u32,
            &mut ret_len,
        );
        let _ = CloseHandle(handle);
        if status != 0 {
            return None;
        }
        let us = &*(buf.as_ptr() as *const UnicodeString);
        if us.buffer.is_null() || us.length == 0 {
            return None;
        }
        let slice = std::slice::from_raw_parts(us.buffer, (us.length / 2) as usize);
        Some(String::from_utf16_lossy(slice))
    }
}

/// PDH 计数器 `\Memory\Pool Paged Resident Bytes`。
fn paged_pool_resident() -> Option<u64> {
    unsafe {
        let mut query = Default::default();
        if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
            return None;
        }
        let mut counter = Default::default();
        let status = PdhAddEnglishCounterW(
            query,
            w!("\\Memory\\Pool Paged Resident Bytes"),
            0,
            &mut counter,
        );
        let result = if status == 0 && PdhCollectQueryData(query) == 0 {
            let mut value = PDH_FMT_COUNTERVALUE::default();
            if PdhGetFormattedCounterValue(counter, PDH_FMT_LARGE, None, &mut value) == 0
                && value.CStatus == 0
            {
                Some(value.Anonymous.largeValue as u64)
            } else {
                None
            }
        } else {
            None
        };
        let _ = PdhCloseQuery(query);
        result
    }
}

/// 光标下方窗口的信息（用于"窗口定位"功能）。
#[derive(Clone)]
pub struct WindowTarget {
    pub pid: u32,
    pub name: String,
    pub title: String,
}

/// 获取当前光标位置，并解析光标下方窗口所属进程。
pub fn window_at_cursor() -> Option<WindowTarget> {
    unsafe {
        let mut pt = POINT::default();
        GetCursorPos(&mut pt).ok()?;
        let hwnd = WindowFromPoint(pt);
        if hwnd.0.is_null() {
            return None;
        }
        // 提升到顶层窗口，避免命中子控件
        let root = GetAncestor(hwnd, GA_ROOT);
        let hwnd = if root.0.is_null() { hwnd } else { root };

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let name = process_name_by_pid(pid).unwrap_or_else(|| format!("PID {pid}"));

        let len = GetWindowTextLengthW(hwnd);
        let title = if len > 0 {
            let mut buf = vec![0u16; (len + 1) as usize];
            let n = GetWindowTextW(hwnd, &mut buf);
            String::from_utf16_lossy(&buf[..n as usize])
        } else {
            String::new()
        };

        Some(WindowTarget { pid, name, title })
    }
}

/// 通过 PID 获取进程的可执行文件名（如 "chrome.exe"）。
pub fn process_name_by_pid(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let n = GetModuleFileNameExW(Some(handle), None, &mut buf);
        let _ = CloseHandle(handle);
        if n == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..n as usize]);
        // 取最后一段文件名
        let name = full
            .rsplit(['\\', '/'])
            .next()
            .map(|s| s.to_string())
            .unwrap_or(full);
        Some(name)
    }
}

/// 结束指定进程。返回是否成功。
pub fn kill_process(pid: u32) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) else {
            return false;
        };
        let ok = TerminateProcess(handle, 1).is_ok();
        let _ = CloseHandle(handle);
        ok
    }
}

/// 批量结束多个进程。返回 (成功数, 失败数)。
pub fn kill_processes(pids: &[u32]) -> (usize, usize) {
    let mut ok = 0;
    let mut fail = 0;
    for &pid in pids {
        if kill_process(pid) {
            ok += 1;
        } else {
            fail += 1;
        }
    }
    (ok, fail)
}

