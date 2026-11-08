param([string]$A,[string]$B)
$docs=@('beans','vogtle','viaduct','sahib_processed_1794115972')
if($env:GATE_EXTRA){ $docs += ($env:GATE_EXTRA -split ';') }
$modes=@(@('--fresh-backend'),@('--repeat','2'))
$bad=0;$n=0
function HashRun($bin,$d,$s,$m){
  $path = if($d -like '*:*'){$d}else{"D:\pdf-test\$d.pdf"}
  & $bin run $path --scale $s --hash @m 2>$null | Where-Object {$_ -notmatch '^#'} | ForEach-Object { $f=$_ -split "`t"; "$($f[0]):$($f[1]):$($f[-1])" } }
foreach($d in $docs){foreach($s in '1.0','1.37','2.0'){foreach($g in '0','1'){foreach($m in $modes){
  $env:PROBE_GRAY8=$g
  $ha=HashRun $A $d $s $m
  $hb=HashRun $B $d $s $m
  $n++
  if(($ha -join ',') -ne ($hb -join ',') -or -not $ha){$bad++; "DIFF $d scale=$s gray=$g $($m -join ' ') (pages a=$($ha.Count) b=$($hb.Count))"}
}}}}
"gate: $n configs, $bad differ"
