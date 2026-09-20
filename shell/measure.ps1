<#
Sums the memory of a process tree, for the stage 1 memory check.
  .\measure.ps1 -RootName streamloop-shell -OutCsv shell.csv
  .\measure.ps1 -Analyze -OutCsv shell.csv
  .\measure.ps1 -SelfTest
#>
[CmdletBinding(DefaultParameterSetName = 'Measure')]
param(
  [Parameter(ParameterSetName = 'Measure', Mandatory = $true)][string]$RootName,
  [Parameter(ParameterSetName = 'Measure', Mandatory = $true)]
  [Parameter(ParameterSetName = 'Analyze', Mandatory = $true)][string]$OutCsv,
  [Parameter(ParameterSetName = 'Measure')][int]$IntervalSec = 60,
  [Parameter(ParameterSetName = 'Analyze', Mandatory = $true)][switch]$Analyze,
  [Parameter(ParameterSetName = 'SelfTest', Mandatory = $true)][switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

function Get-TreePids($procs, [string]$rootName) {
  $exe = "$rootName.exe"
  $byId = @{}
  foreach ($p in $procs) { $byId[[int]$p.ProcessId] = $p }

  $children = @{}
  foreach ($p in $procs) {
    $parent = $byId[[int]$p.ParentProcessId]
    # Windows reuses PIDs, so a parent that is younger than its child is an unrelated process.
    if ($parent -and $parent.CreationDate -le $p.CreationDate) {
      $key = [int]$p.ParentProcessId
      if (-not $children.ContainsKey($key)) { $children[$key] = @() }
      $children[$key] += [int]$p.ProcessId
    }
  }

  $queue = New-Object System.Collections.Queue
  foreach ($p in $procs) {
    if ($p.Name -ne $exe) { continue }
    $parent = $byId[[int]$p.ParentProcessId]
    if (-not ($parent -and $parent.Name -eq $exe)) { $queue.Enqueue([int]$p.ProcessId) }
  }

  $seen = @{}
  while ($queue.Count -gt 0) {
    $id = $queue.Dequeue()
    if ($seen.ContainsKey($id)) { continue }
    $seen[$id] = $true
    foreach ($child in $children[$id]) { $queue.Enqueue($child) }
  }
  return @($seen.Keys)
}

function Get-Verdict($rows) {
  $live = @($rows | Where-Object { [int]$_.Processes -gt 0 })
  if ($live.Count -eq 0) { throw 'No samples with a running process tree.' }

  $sorted = @($live | ForEach-Object { [int]$_.PrivateMB } | Sort-Object)
  $p95 = $sorted[[math]::Ceiling(0.95 * $sorted.Count) - 1]

  $start = [datetime]$live[0].Timestamp
  $end = [datetime]$live[-1].Timestamp
  $first = $live | Where-Object { [datetime]$_.Timestamp -lt $start.AddHours(6) } | ForEach-Object { [int]$_.PrivateMB }
  $last = $live | Where-Object { [datetime]$_.Timestamp -gt $end.AddHours(-6) } | ForEach-Object { [int]$_.PrivateMB }
  $firstMean = ($first | Measure-Object -Average).Average
  $lastMean = ($last | Measure-Object -Average).Average

  [pscustomobject]@{
    Samples = $live.Count
    Hours = [int][math]::Floor(($end - $start).TotalHours)
    P95MB = $p95
    FirstMeanMB = [int][math]::Round($firstMean)
    LastMeanMB = [int][math]::Round($lastMean)
    MemoryPass = ($p95 -lt 200)
    TrendPass = ($lastMean -le 1.10 * $firstMean)
    LongEnough = (($end - $start).TotalHours -ge 24)
  }
}

function Assert-That($condition, [string]$message) {
  if (-not $condition) { throw "SelfTest failed: $message" }
}

function New-FakeProcess([int]$id, [int]$parentId, [string]$name, [int]$startedMinute) {
  [pscustomobject]@{
    ProcessId = $id; ParentProcessId = $parentId; Name = $name
    CreationDate = (Get-Date '2026-01-01').AddMinutes($startedMinute)
  }
}

function New-FakeRows([int]$hours, [scriptblock]$privateMbAtHour) {
  $start = Get-Date '2026-01-01'
  0..($hours * 60) | ForEach-Object {
    [pscustomobject]@{
      Timestamp = $start.AddMinutes($_).ToString('s'); Processes = '5'
      PrivateMB = [string](& $privateMbAtHour ($_ / 60)); WorkingSetMB = '0'
    }
  }
}

function Invoke-SelfTest {
  $procs = @(
    (New-FakeProcess 100 1 'streamloop-shell.exe' 10),
    (New-FakeProcess 200 100 'msedgewebview2.exe' 11),
    (New-FakeProcess 300 200 'msedgewebview2.exe' 12),
    (New-FakeProcess 400 1 'node.exe' 5),
    (New-FakeProcess 500 400 'mpv.exe' 6),
    # Parent PID 300 was reused: this process is older than its so-called parent.
    (New-FakeProcess 600 300 'notepad.exe' 1)
  )
  $tree = Get-TreePids $procs 'streamloop-shell' | Sort-Object
  Assert-That (($tree -join ',') -eq '100,200,300') "shell tree was $($tree -join ',')"

  $firefox = @(
    (New-FakeProcess 10 1 'firefox.exe' 1),
    (New-FakeProcess 20 10 'firefox.exe' 2),
    (New-FakeProcess 30 20 'firefox.exe' 3)
  )
  $tree = Get-TreePids $firefox 'firefox' | Sort-Object
  Assert-That (($tree -join ',') -eq '10,20,30') "firefox tree was $($tree -join ',')"

  $none = @(Get-TreePids $procs 'not-running')
  Assert-That ($none.Count -eq 0) 'a missing root must give an empty tree'

  $flat = Get-Verdict (New-FakeRows 25 { param($h) 150 })
  Assert-That ($flat.P95MB -eq 150) "flat p95 was $($flat.P95MB)"
  Assert-That ($flat.MemoryPass -and $flat.TrendPass -and $flat.LongEnough) 'flat 150 MB for 25 h must pass everything'

  $leak = Get-Verdict (New-FakeRows 25 { param($h) 100 + [int]($h * 3) })
  Assert-That ($leak.MemoryPass) 'leak run stays under 200 MB'
  Assert-That (-not $leak.TrendPass) 'growing 3 MB per hour must fail the trend check'

  $big = Get-Verdict (New-FakeRows 25 { param($h) 250 })
  Assert-That (-not $big.MemoryPass) '250 MB must fail the memory bar'

  $short = Get-Verdict (New-FakeRows 3 { param($h) 150 })
  Assert-That (-not $short.LongEnough) 'a 3 h run is not long enough'

  'SelfTest passed'
}

if ($SelfTest) { Invoke-SelfTest; return }

if ($Analyze) { Get-Verdict (Import-Csv -Path $OutCsv); return }

function Get-Sample([string]$rootName) {
  $procs = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name, CreationDate
  $tree = @(Get-TreePids $procs $rootName)
  $private = 0
  $workingSet = 0
  if ($tree.Count -gt 0) {
    $perf = Get-CimInstance Win32_PerfRawData_PerfProc_Process -Property IDProcess, WorkingSetPrivate, WorkingSet
    foreach ($row in $perf) {
      if ($tree -contains [int]$row.IDProcess) {
        $private += $row.WorkingSetPrivate
        $workingSet += $row.WorkingSet
      }
    }
  }
  [pscustomobject]@{
    Timestamp = (Get-Date).ToString('s')
    Processes = $tree.Count
    PrivateMB = [int][math]::Round($private / 1MB)
    WorkingSetMB = [int][math]::Round($workingSet / 1MB)
  }
}

"Sampling the $RootName process tree every $IntervalSec s into $OutCsv. Ctrl+C to stop."
while ($true) {
  $sample = Get-Sample $RootName
  $sample | Export-Csv -Path $OutCsv -Append -NoTypeInformation
  "$($sample.Timestamp)  processes=$($sample.Processes)  private=$($sample.PrivateMB) MB"
  Start-Sleep -Seconds $IntervalSec
}
