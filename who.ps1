$procs = Get-CimInstance Win32_Process -Filter "Name='node.exe'"
$all = Get-CimInstance Win32_Process
$byId = @{}
foreach ($p in $all) { $byId[$p.ProcessId] = $p }

foreach ($p in $procs) {
    $parent = $byId[$p.ParentProcessId]
    $pname = if ($parent) { $parent.Name } else { "(已退出 PID $($p.ParentProcessId))" }
    $pcmd = if ($parent) { $parent.CommandLine } else { "" }
    Write-Output ("node.exe PID {0}  工作集 {1} MB" -f $p.ProcessId, [math]::Round($p.WorkingSetSize/1MB,1))
    Write-Output ("  父进程: {0} (PID {1})" -f $pname, $p.ParentProcessId)
    if ($pcmd) { Write-Output ("  父命令行: {0}" -f $pcmd) }
    Write-Output ("  自身命令行: {0}" -f $p.CommandLine)
    Write-Output ""
}
